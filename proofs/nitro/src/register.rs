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

use alloy_network::EthereumWallet;
use alloy_primitives::{Address, Bytes, TxHash};
use alloy_provider::ProviderBuilder;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::sol;
use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha384};
use tracing::{info, warn};
use url::Url;

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

sol! {
    /// Minimal on-chain surface of `NitroEnclaveKeyRegistry` needed for self-registration.
    #[sol(rpc)]
    interface INitroEnclaveKeyRegistry {
        /// Reverted by `registerKey` when the key is already `Active`.
        error KeyAlreadyRegistered();
        /// Reverted by `registerKey` when the key was permanently revoked.
        error KeyRevokedPermanently();
        /// Reverted by `registerKey` when the attestation's public key is malformed.
        error InvalidPublicKey();

        function registerKey(bytes attestationTbs, bytes signature, bytes attestationSigHints)
            external
            returns (bytes publicKey, bytes32 pcr0, bytes32 pcr1, bytes32 pcr2);
        function isKeyRegistered(bytes publicKey) external view returns (bool);
    }
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
/// The flow is idempotent: if the key is already `Active` in the registry (or a concurrent
/// registration wins the race and the tx reverts with `KeyAlreadyRegistered`) this returns
/// [`RegistrationOutcome::AlreadyRegistered`] instead of erroring.
///
/// # Errors
///
/// Returns an error if the enclave is unreachable, the RPC/key are invalid, the
/// `registerKey` transaction reverts for a reason other than `KeyAlreadyRegistered`
/// (e.g. PCR set not approved, CertManager not pre-warmed), or the key is still not
/// registered after a confirmed transaction.
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
    info!(
        target: "world_chain::nitro",
        pubkey = %hex::encode(&public_key),
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
    let registry = INitroEnclaveKeyRegistry::new(registry_address, provider);

    // 3. Short-circuit if the key is already registered.
    if registry
        .isKeyRegistered(public_key.clone())
        .call()
        .await
        .context("isKeyRegistered pre-check")?
    {
        info!(
            target: "world_chain::nitro",
            pubkey = %hex::encode(&public_key),
            registry = %registry_address,
            "enclave key already registered on-chain; nothing to do"
        );
        return Ok(RegistrationOutcome::AlreadyRegistered);
    }

    // 4. Build calldata and submit registerKey.
    let calldata = build_registration_calldata(&attestation_doc)?;
    info!(
        target: "world_chain::nitro",
        registry = %registry_address,
        signer = %signer_address,
        tbs_bytes = calldata.attestation_tbs.len(),
        hint_bytes = calldata.attestation_sig_hints.len(),
        "submitting registerKey"
    );

    let pending = match registry
        .registerKey(
            Bytes::from(calldata.attestation_tbs),
            Bytes::from(calldata.signature),
            Bytes::from(calldata.attestation_sig_hints),
        )
        .send()
        .await
    {
        Ok(pending) => pending,
        Err(err) => {
            // A concurrent registration may have landed between our pre-check and the
            // send (e.g. another worker/enclave with the same key registered first).
            // Re-query the registry — this is decode-independent, so it reliably
            // distinguishes that race (key now Active) from a genuine failure such as
            // an unapproved PCR set or an un-pre-warmed CertManager. The custom errors
            // are also declared on the sol! interface above so alloy can decode the
            // revert into a readable reason in the propagated error.
            if registry
                .isKeyRegistered(public_key.clone())
                .call()
                .await
                .unwrap_or(false)
            {
                warn!(
                    target: "world_chain::nitro",
                    "registerKey failed but the key is already registered; treating as success"
                );
                return Ok(RegistrationOutcome::AlreadyRegistered);
            }
            return Err(anyhow!("registerKey send failed: {err}"));
        }
    };

    let tx_hash = *pending.tx_hash();
    let receipt = pending
        .get_receipt()
        .await
        .with_context(|| format!("awaiting registerKey receipt (tx {tx_hash})"))?;
    if !receipt.status() {
        bail!("registerKey transaction {tx_hash} reverted");
    }

    // 5. Confirm the key is now registered.
    let registered = registry
        .isKeyRegistered(public_key.clone())
        .call()
        .await
        .context("isKeyRegistered post-check")?;
    if !registered {
        bail!("registerKey tx {tx_hash} confirmed but key is still not registered");
    }

    info!(
        target: "world_chain::nitro",
        %tx_hash,
        pubkey = %hex::encode(&public_key),
        signer = %signer_address,
        "enclave key registered on-chain"
    );
    Ok(RegistrationOutcome::Registered { tx_hash })
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
}
