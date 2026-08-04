//! On-chain enclave key registration.
//!
//! Registering the enclave's ephemeral secp256k1 signing key on-chain binds the key to the
//! approved PCR set in [`NitroEnclaveKeyRegistry`], so that `NitroProofVerifier.verify` will
//! accept `ecrecover` signatures produced by this enclave.
//!
//! Two layers live here:
//!
//! - [`build_registration_calldata`] — a pure, platform-independent helper that turns a
//!   `public_key`-embedding attestation document into the exact
//!   `(attestationTbs, signature, attestationSigHints)` triple that
//!   `NitroEnclaveKeyRegistry.registerKey` expects. It reuses
//!   [`crate::cose::decode_attestation_tbs`], [`crate::attestation::leaf_cert_pubkey_xy`],
//!   and [`crate::p384_hints::collect_hints`].
//! - [`register_enclave_key`] (Linux + `enclave` feature) — the full flow: fetch the
//!   attestation from a running enclave over vsock, build the calldata, submit `registerKey`
//!   to L1, and confirm registration. Used by both the `world-chain-prover-nitro register`
//!   subcommand and the worker's `--auto-register` startup hook.

use std::time::Duration;

use alloy_network::EthereumWallet;
use alloy_primitives::{Address, Bytes, TxHash, keccak256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::sol;
use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha384};
use tracing::{info, warn};
use url::Url;

use crate::prewarm::{ColdCert, build_prewarm_plan, packed_cert_not_after};

/// Max attempts for the `registerKey` submission. Retries let the flow survive transient
/// RPC errors and funding-account nonce contention when several worker replicas share the
/// same `REGISTER_PRIVATE_KEY`/`PRIVATE_KEY` and self-register simultaneously.
const REGISTER_MAX_ATTEMPTS: u32 = 5;
/// Base backoff between `registerKey` attempts (scaled by the attempt number).
const REGISTER_RETRY_BASE_DELAY: Duration = Duration::from_secs(2);

/// Classifies a `registerKey` attempt failure so the retry loop can tell a deterministic
/// revert (fail fast) apart from a transient nonce/RPC error (retry).
enum AttemptError {
    /// Deterministic on-chain revert (unapproved PCRs, revoked signer, cold CertManager,
    /// bad hints) surfaced at estimation — retrying can't help.
    Permanent(anyhow::Error),
    /// Transient failure (funding-account nonce race, RPC hiccup, dropped tx) — retryable.
    Transient(anyhow::Error),
}

use crate::{
    ExpectedPcrs,
    host::{EnclaveEndpoint, NitroProver},
};

/// Calldata for `NitroEnclaveKeyRegistry.registerKey(bytes,bytes,bytes)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrationCalldata {
    /// COSE_Sign1 TBS bytes (`NitroValidator.decodeAttestationTbs` equivalent).
    pub attestation_tbs: Vec<u8>,
    /// 96-byte `r || s` P-384 attestation signature.
    pub signature: Vec<u8>,
    /// Off-chain modular-inverse hints for the P-384 attestation signature.
    pub attestation_sig_hints: Vec<u8>,
}

/// Builds the `registerKey` calldata from a `public_key`-embedding attestation document.
///
/// The document must be one produced by the enclave's `EnclaveRequest::PublicKey` handler
/// (i.e. it carries the enclave's ephemeral public key). A bare attestation
/// (`EnclaveRequest::GetAttestation`) works for the CBOR/signature decode but has no key for
/// the registry to bind, so use a `PublicKey` attestation here.
///
/// # Errors
///
/// Returns an error if the document is not a well-formed COSE_Sign1 structure, is missing
/// its leaf certificate, or if hint generation fails.
fn build_registration_calldata(attestation_doc: &[u8]) -> Result<RegistrationCalldata> {
    let (attestation_tbs, signature) = crate::cose::decode_attestation_tbs(attestation_doc)
        .context("decoding attestation TBS + signature")?;

    let leaf_pubkey = crate::attestation::leaf_cert_pubkey_xy(attestation_doc)
        .map_err(|e| anyhow::anyhow!("extracting leaf certificate public key: {e}"))?;

    // The attestation signature covers SHA-384 of the TBS bytes.
    let hash = Sha384::digest(&attestation_tbs);

    let attestation_sig_hints = crate::p384_hints::collect_hints(&hash, &signature, &leaf_pubkey)
        .context("generating P-384 attestation signature hints")?;

    Ok(RegistrationCalldata {
        attestation_tbs,
        signature,
        attestation_sig_hints,
    })
}

/// Trims `value` and errors if it is empty, so a blank flag/env var produces a precise
/// message instead of a downstream parse error.
fn non_empty<'a>(value: &'a str, field: &str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{field} is required (got an empty value)");
    }
    Ok(trimmed)
}

/// Derives the on-chain signer address from the enclave's uncompressed secp256k1 public key
/// (`0x04 || X || Y`, 65 bytes). This mirrors `NitroEnclaveKeyRegistry._signerAddress`:
/// `address(uint160(uint256(keccak256(pubkey[1..65]))))`.
fn enclave_signer_address(public_key: &[u8]) -> Result<Address> {
    if public_key.len() != 65 || public_key[0] != 0x04 {
        bail!(
            "unexpected enclave public key encoding ({} bytes, prefix {:#04x})",
            public_key.len(),
            public_key.first().copied().unwrap_or(0)
        );
    }
    // keccak256 over the 64-byte X||Y coordinates, address = last 20 bytes.
    let hash = keccak256(&public_key[1..65]);
    Ok(Address::from_slice(&hash[12..]))
}

sol! {
    /// Minimal on-chain surface of `NitroEnclaveKeyRegistry` needed for self-registration.
    #[sol(rpc)]
    interface INitroEnclaveKeyRegistry {
        /// Reverted by `registerKey` when the signer is already `Active`.
        error SignerAlreadyRegistered();
        /// Reverted by `registerKey` when the signer was permanently revoked.
        error SignerRevokedPermanently();
        /// Reverted by `registerKey` when the attestation's public key is malformed.
        error InvalidPublicKey();

        function registerKey(bytes attestationTbs, bytes signature, bytes attestationSigHints)
            external
            returns (address signer, bytes32 pcr0, bytes32 pcr1, bytes32 pcr2);
        function isSignerRegistered(address signer) external view returns (bool);
        /// `NitroAttestationVerifier`, which is itself a `NitroValidator` and so exposes
        /// `certManager()`. Used to discover the CertManager without extra configuration.
        function verifier() external view returns (address);
    }

    /// `NitroValidator`'s view of its CertManager. `NitroAttestationVerifier is NitroValidator`,
    /// so this is callable on the address returned by `INitroEnclaveKeyRegistry.verifier()`.
    #[sol(rpc)]
    interface INitroValidator {
        function certManager() external view returns (address);
    }

    /// The subset of `CertManager` needed to pre-warm the attestation's certificate bundle.
    #[sol(rpc)]
    interface ICertManager {
        /// Raw packed `VerifiedCert` record, or empty bytes when the cert is not cached.
        function verified(bytes32 certHash) external view returns (bytes);
        function verifyCACertWithHints(bytes cert, bytes32 parentCertHash, bytes signatureHints)
            external
            returns (bytes32);
        function verifyClientCertWithHints(bytes cert, bytes32 parentCertHash, bytes signatureHints)
            external;
    }
}

/// Ensures every certificate in `plan` is present in the on-chain `CertManager` cache,
/// submitting a verification transaction for each one that is not.
///
/// `registerKey` re-walks the attestation's certificate bundle with **empty** hints, so an
/// uncached certificate makes it revert with `"inverse hint underflow"` regardless of how good
/// the attestation's own hints are. AWS rotates the enclave leaf certificate roughly every three
/// hours, so this runs on every registration rather than as a one-off deploy step.
///
/// Entries are submitted in order because each one's parent must be cached first. A certificate
/// cached by a peer replica between the check and the submit is treated as success, not an error.
///
/// # Errors
///
/// Returns an error if a certificate is cached but already expired (a new attestation is needed;
/// resubmitting cannot help), or if a verification transaction fails and the certificate is
/// still not cached afterwards.
async fn prewarm_cert_bundle<P: Provider + Clone>(
    provider: P,
    cert_manager_address: Address,
    plan: &[ColdCert],
    now_secs: u64,
) -> Result<usize> {
    let cert_manager = ICertManager::new(cert_manager_address, provider);
    let mut submitted = 0usize;

    for (i, entry) in plan.iter().enumerate() {
        let cached = cert_manager
            .verified(entry.cache_key)
            .call()
            .await
            .with_context(|| format!("checking CertManager cache for chain[{i}]"))?;

        if !cached.is_empty() {
            // A cached-but-expired cert cannot be re-verified: `_verifyCert` short-circuits on
            // the cache and reverts with "cert expired". Only a fresh attestation fixes it, so
            // say so explicitly rather than letting registerKey fail opaquely later.
            match packed_cert_not_after(&cached) {
                Some(not_after) if not_after <= now_secs => bail!(
                    "chain[{i}] (cache key {}) is cached but expired at {not_after} (now {now_secs}); \
                     the enclave needs a fresh attestation before it can register",
                    entry.cache_key
                ),
                _ => {}
            }
            continue;
        }

        info!(
            target: "world_chain::nitro",
            cert_manager = %cert_manager_address,
            cache_key = %entry.cache_key,
            parent = %entry.parent_hash,
            is_ca = entry.is_ca,
            hint_bytes = entry.hints.len(),
            "pre-warming CertManager with uncached certificate"
        );

        let cert = Bytes::from(entry.cert.clone());
        let hints = Bytes::from(entry.hints.clone());
        let sent = if entry.is_ca {
            cert_manager
                .verifyCACertWithHints(cert, entry.parent_hash, hints)
                .send()
                .await
        } else {
            cert_manager
                .verifyClientCertWithHints(cert, entry.parent_hash, hints)
                .send()
                .await
        };

        let outcome = match sent {
            Ok(pending) => {
                let tx_hash = *pending.tx_hash();
                pending
                    .get_receipt()
                    .await
                    .map(|receipt| (tx_hash, receipt.status()))
                    .map_err(|err| anyhow!("awaiting receipt for tx {tx_hash}: {err}"))
            }
            Err(err) => Err(anyhow!("submitting cert verification: {err}")),
        };

        match outcome {
            Ok((tx_hash, true)) => {
                submitted += 1;
                info!(
                    target: "world_chain::nitro",
                    %tx_hash,
                    cache_key = %entry.cache_key,
                    "certificate verified into the CertManager cache"
                );
            }
            // A revert (or a send failure) is only fatal if the cert is still uncached — a peer
            // replica pre-warming the same chain concurrently is a benign race.
            Ok((tx_hash, false)) => {
                let now_cached = cert_manager
                    .verified(entry.cache_key)
                    .call()
                    .await
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);
                if !now_cached {
                    bail!(
                        "cert verification tx {tx_hash} reverted and chain[{i}] is still uncached"
                    );
                }
                warn!(
                    target: "world_chain::nitro",
                    %tx_hash,
                    "cert verification reverted but the cert is now cached; treating as success"
                );
            }
            Err(err) => {
                let now_cached = cert_manager
                    .verified(entry.cache_key)
                    .call()
                    .await
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);
                if !now_cached {
                    return Err(err)
                        .with_context(|| format!("pre-warming CertManager for chain[{i}] failed"));
                }
                warn!(
                    target: "world_chain::nitro",
                    error = %err,
                    "cert verification failed but the cert is now cached; treating as success"
                );
            }
        }
    }

    Ok(submitted)
}

/// Inputs for [`register_enclave_key`].
#[derive(Clone, Debug)]
pub struct RegisterParams {
    /// vsock CID of the running enclave.
    pub enclave_cid: u32,
    /// vsock port of the running enclave.
    pub enclave_port: u32,
    /// Expected PCRs used for host-side attestation verification. Use
    /// [`ExpectedPcrs::PLACEHOLDER`] in dev/test to skip host-side checks (the on-chain
    /// verifier still enforces the approved PCR allowlist).
    pub expected_pcrs: ExpectedPcrs,
    /// L1 execution RPC URL to submit `registerKey` to.
    pub l1_rpc_url: String,
    /// `NitroEnclaveKeyRegistry` contract address on L1 (hex, `0x`-prefixed).
    pub registry: String,
    /// Hex-encoded private key used to sign (and pay gas for) the `registerKey` tx.
    /// `registerKey` is **not** owner-gated, so any funded key works.
    pub private_key: String,
}

/// Result of a registration attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistrationOutcome {
    /// The enclave's key was already registered on-chain; no transaction was sent.
    AlreadyRegistered,
    /// A `registerKey` transaction was submitted and confirmed.
    Registered {
        /// Hash of the confirmed `registerKey` transaction.
        tx_hash: TxHash,
    },
}

/// Fetches the enclave's public-key attestation over vsock and registers the key on-chain.
///
/// The flow is idempotent: if the signer is already `Active` in the registry (or a concurrent
/// registration wins the race and the tx reverts with `SignerAlreadyRegistered`) this returns
/// [`RegistrationOutcome::AlreadyRegistered`] instead of erroring.
///
/// The attestation's certificate chain is pre-warmed into the on-chain `CertManager` first
/// (see [`prewarm_cert_bundle`]), so registration recovers on its own after AWS rotates the
/// enclave's leaf certificate — no operator-run pre-warm step is required.
///
/// # Errors
///
/// Returns an error if the enclave is unreachable, the RPC/key are invalid, the certificate
/// chain cannot be pre-warmed, the `registerKey` transaction reverts for a reason other than
/// `SignerAlreadyRegistered` (e.g. PCR set not approved), or the key is still not registered
/// after a confirmed transaction.
pub async fn register_enclave_key(params: RegisterParams) -> Result<RegistrationOutcome> {
    // 0. Validate the string inputs up front with clear messages. `.parse()` below
    //    would also reject these, but an explicit empty-string check gives operators a
    //    precise error instead of a generic "invalid address/URL/key".
    let l1_rpc_url = non_empty(&params.l1_rpc_url, "L1 RPC URL")?;
    let registry = non_empty(&params.registry, "NitroEnclaveKeyRegistry address")?;
    let private_key = non_empty(&params.private_key, "registration private key")?;

    // 1. Fetch a public-key-embedding attestation from the running enclave.
    let endpoint = EnclaveEndpoint::with_port(params.enclave_cid, params.enclave_port);
    let prover = NitroProver::new(endpoint, params.expected_pcrs);
    let (attestation_doc, public_key) = prover
        .get_public_key_async()
        .await
        .map_err(|e| anyhow!("failed to fetch enclave public-key attestation: {e}"))?;
    let public_key = Bytes::from(public_key);
    // The registry keys registrations on the Ethereum address derived from the enclave's
    // uncompressed secp256k1 public key (keccak256(pubkey[1..65])[12..]).
    let enclave_signer = enclave_signer_address(public_key.as_ref())?;
    info!(
        target: "world_chain::nitro",
        pubkey = %hex::encode(&public_key),
        signer = %enclave_signer,
        "fetched enclave public-key attestation"
    );

    // 2. Build the provider + registry binding.
    let url = Url::parse(l1_rpc_url).context("invalid L1 RPC URL")?;
    let registry_address: Address = registry
        .parse()
        .context("invalid NitroEnclaveKeyRegistry address")?;
    let signer: PrivateKeySigner = private_key
        .parse()
        .context("invalid registration private key")?;
    let signer_address = signer.address();
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect_http(url);
    let registry = INitroEnclaveKeyRegistry::new(registry_address, provider.clone());

    // 3. Pre-warm the CertManager with this attestation's certificate chain. `registerKey`
    //    re-walks the bundle with empty hints, so any uncached certificate makes it revert with
    //    "inverse hint underflow" no matter how good the attestation hints are — and AWS rotates
    //    the enclave leaf roughly every three hours, so the cache goes cold on its own. The
    //    CertManager address is discovered through the registry so there is no second address to
    //    configure and drift.
    let verifier_address = registry
        .verifier()
        .call()
        .await
        .context("reading NitroEnclaveKeyRegistry.verifier()")?;
    let cert_manager_address = INitroValidator::new(verifier_address, provider.clone())
        .certManager()
        .call()
        .await
        .context("reading NitroAttestationVerifier.certManager()")?;

    let plan = build_prewarm_plan(&attestation_doc)
        .context("building the CertManager pre-warm plan from the enclave attestation")?;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let submitted =
        prewarm_cert_bundle(provider.clone(), cert_manager_address, &plan, now_secs).await?;
    info!(
        target: "world_chain::nitro",
        cert_manager = %cert_manager_address,
        chain_len = plan.len(),
        submitted,
        "CertManager pre-warm complete"
    );

    // 4-6. Submit registerKey with bounded retries. Each enclave registers its OWN distinct
    //      signer, so retries exist to survive transient RPC failures and funding-account
    //      nonce contention when several worker replicas share REGISTER_PRIVATE_KEY and
    //      register at the same time. Every attempt first re-checks isSignerRegistered so we
    //      never resubmit once the signer is Active (by us or a peer).
    let calldata = build_registration_calldata(&attestation_doc)?;
    let mut last_err: Option<anyhow::Error> = None;

    for attempt in 1..=REGISTER_MAX_ATTEMPTS {
        // Idempotency guard: stop as soon as the signer is registered. A transient RPC error
        // on this check is not fatal — fall through and let the attempt (and its post-receipt
        // check) sort it out rather than aborting worker startup.
        match registry.isSignerRegistered(enclave_signer).call().await {
            Ok(true) => {
                info!(
                    target: "world_chain::nitro",
                    signer = %enclave_signer,
                    registry = %registry_address,
                    "enclave signer already registered on-chain; nothing to do"
                );
                return Ok(RegistrationOutcome::AlreadyRegistered);
            }
            Ok(false) => {}
            Err(err) => {
                warn!(
                    target: "world_chain::nitro",
                    error = %err,
                    "isSignerRegistered pre-check failed; attempting registration anyway"
                );
            }
        }

        info!(
            target: "world_chain::nitro",
            registry = %registry_address,
            tx_signer = %signer_address,
            attempt,
            max_attempts = REGISTER_MAX_ATTEMPTS,
            tbs_bytes = calldata.attestation_tbs.len(),
            hint_bytes = calldata.attestation_sig_hints.len(),
            "submitting registerKey"
        );

        // One submit attempt: send, then await the receipt. Send/receipt failures are
        // classified so a deterministic revert (bad PCRs, cold CertManager, revoked signer,
        // bad hints) fails fast, while nonce races / transient RPC errors are retried.
        let attempt_result: std::result::Result<(TxHash, bool), AttemptError> = async {
            let pending = registry
                .registerKey(
                    Bytes::from(calldata.attestation_tbs.clone()),
                    Bytes::from(calldata.signature.clone()),
                    Bytes::from(calldata.attestation_sig_hints.clone()),
                )
                .send()
                .await
                .map_err(|err| {
                    // A revert surfaced at estimation/simulation carries revert data and is
                    // deterministic; a transport/nonce error does not and is retryable.
                    if err.as_revert_data().is_some() {
                        AttemptError::Permanent(anyhow!(
                            "registerKey reverted at estimation: {err}"
                        ))
                    } else {
                        AttemptError::Transient(anyhow!("registerKey send failed: {err}"))
                    }
                })?;
            let tx_hash = *pending.tx_hash();
            let receipt = pending.get_receipt().await.map_err(|err| {
                AttemptError::Transient(anyhow!(
                    "awaiting registerKey receipt (tx {tx_hash}): {err}"
                ))
            })?;
            Ok((tx_hash, receipt.status()))
        }
        .await;

        match attempt_result {
            Ok((tx_hash, mined_ok)) => {
                // The tx was mined. Whether the signer is now registered decides the
                // outcome. A transient RPC error on THIS check is retryable (the tx may well
                // have succeeded), so we never turn it into a hard failure.
                match registry.isSignerRegistered(enclave_signer).call().await {
                    Ok(true) => {
                        if mined_ok {
                            info!(
                                target: "world_chain::nitro",
                                %tx_hash,
                                pubkey = %hex::encode(&public_key),
                                enclave_signer = %enclave_signer,
                                tx_signer = %signer_address,
                                "enclave signer registered on-chain"
                            );
                            return Ok(RegistrationOutcome::Registered { tx_hash });
                        }
                        warn!(
                            target: "world_chain::nitro",
                            %tx_hash,
                            "registerKey tx reverted but the signer is already registered; treating as success"
                        );
                        return Ok(RegistrationOutcome::AlreadyRegistered);
                    }
                    Ok(false) => {
                        // Mined but the signer is definitively not registered. Resubmitting
                        // won't help — either a confirmed-but-absent write, or a deterministic
                        // revert (unapproved PCR set, permanently-revoked signer, un-pre-warmed
                        // CertManager, or bad hints) — so fail fast instead of retrying.
                        if mined_ok {
                            bail!(
                                "registerKey tx {tx_hash} confirmed but signer is still not registered"
                            );
                        }
                        bail!(
                            "registerKey tx {tx_hash} reverted on-chain and the signer is not registered \
                             (check the approved PCR set, CertManager pre-warm and attestation hints, \
                             and that the signer is not revoked)"
                        );
                    }
                    Err(check_err) => {
                        // Could not verify the outcome due to a transient RPC error — retry
                        // rather than hard-fail, since the registration may have succeeded.
                        last_err = Some(anyhow!(
                            "verifying registration after tx {tx_hash} failed: {check_err}"
                        ));
                    }
                }
            }
            Err(AttemptError::Permanent(err)) => {
                // Deterministic revert at estimation (unapproved PCR set, revoked signer,
                // cold CertManager, bad hints). If a peer registered our signer in the
                // meantime that's success; otherwise fail fast — resubmitting can't help.
                if matches!(
                    registry.isSignerRegistered(enclave_signer).call().await,
                    Ok(true)
                ) {
                    warn!(
                        target: "world_chain::nitro",
                        "registerKey reverted but the signer is already registered; treating as success"
                    );
                    return Ok(RegistrationOutcome::AlreadyRegistered);
                }
                return Err(err).context(
                    "registerKey reverted deterministically (check the approved PCR set, \
                     CertManager pre-warm and attestation hints, and that the signer is not revoked)",
                );
            }
            Err(AttemptError::Transient(err)) => {
                // The tx never made it on-chain (e.g. a funding-account nonce race or a
                // transient RPC error). If the signer is registered anyway (a peer won the
                // race), we're done; otherwise this is retryable.
                if matches!(
                    registry.isSignerRegistered(enclave_signer).call().await,
                    Ok(true)
                ) {
                    warn!(
                        target: "world_chain::nitro",
                        "registerKey failed but the signer is already registered; treating as success"
                    );
                    return Ok(RegistrationOutcome::AlreadyRegistered);
                }
                last_err = Some(err);
            }
        }

        if attempt < REGISTER_MAX_ATTEMPTS {
            let delay = REGISTER_RETRY_BASE_DELAY * attempt;
            warn!(
                target: "world_chain::nitro",
                attempt,
                delay_secs = delay.as_secs(),
                error = ?last_err,
                "registerKey attempt failed; retrying"
            );
            tokio::time::sleep(delay).await;
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("registerKey did not succeed"))).with_context(|| {
        format!("registerKey did not succeed after {REGISTER_MAX_ATTEMPTS} attempts")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ciborium::value::Value;

    /// Builds a COSE_Sign1 document with a valid 96-byte signature but no real certificate,
    /// so we can exercise the CBOR/signature decode path of `build_registration_calldata`.
    fn make_cose_without_cert() -> Vec<u8> {
        let payload = {
            let mut buf = Vec::new();
            ciborium::ser::into_writer(&Value::Map(Vec::new()), &mut buf).unwrap();
            buf
        };
        let cose = Value::Array(vec![
            Value::Bytes(vec![0xa0]),
            Value::Map(Vec::new()),
            Value::Bytes(payload),
            Value::Bytes(vec![1u8; 96]),
        ]);
        let mut out = Vec::new();
        ciborium::ser::into_writer(&cose, &mut out).unwrap();
        out
    }

    #[test]
    fn build_calldata_requires_leaf_certificate() {
        // Without a leaf certificate we cannot derive hints, so this must fail cleanly
        // rather than panic.
        let doc = make_cose_without_cert();
        let err = build_registration_calldata(&doc).unwrap_err();
        assert!(
            err.to_string().contains("leaf certificate") || err.to_string().contains("certificate"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn derives_signer_address_matching_contract() {
        // secp256k1 private key = 1 → well-known uncompressed public key and address.
        // Must match `NitroEnclaveKeyRegistry._signerAddress` = keccak256(pubkey[1..65])[12..].
        let pubkey = hex::decode(
            "0479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798\
             483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8",
        )
        .unwrap();
        let addr = enclave_signer_address(&pubkey).unwrap();
        let expected: Address = "0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf"
            .parse()
            .unwrap();
        assert_eq!(addr, expected);
    }

    #[test]
    fn rejects_malformed_public_key() {
        assert!(enclave_signer_address(&[0u8; 33]).is_err()); // wrong length
        assert!(enclave_signer_address(&[0x02; 65]).is_err()); // wrong SEC1 prefix
    }
}
