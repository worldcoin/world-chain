//! Enclave-side library code.
//!
//! This is the worker loop the `world-chain-nitro-enclave` binary runs inside the Nitro
//! Enclave. It listens on vsock, runs the same OP Succinct Lite range/aggregation logic the
//! SP1 guest does, and attests the result via the local NSM device.
//!
//! Gated behind the `enclave` feature so the heavy kona / NSM dependencies are not pulled
//! into the host build.
//!
//! # Ephemeral signing keypair
//!
//! At startup, [`serve_forever`] generates a secp256k1 signing key using entropy from the
//! NSM `GetRandom` request. The key is stored in a [`std::sync::OnceLock`] and is therefore:
//!
//! - **Ephemeral**: generated fresh every time the enclave starts; never written to disk.
//! - **Certified**: the public key is embedded in NSM attestation documents via
//!   [`EnclaveRequest::PublicKey`], binding it to the enclave's PCR measurements.
//!
//! Every [`EnclaveResponse::Range`] and [`EnclaveResponse::Aggregation`] includes a 65-byte
//! recoverable secp256k1 signature over
//! `keccak256(l2_post_root ‖ l2_block_number_be ‖ rollup_config_hash)`.

use std::sync::{Arc, OnceLock};

use alloy_consensus::Header;
use anyhow::{Context, Result, anyhow};
use aws_nitro_enclaves_nsm_api::{
    api::{Request as NsmRequest, Response as NsmResponse},
    driver::{nsm_init, nsm_process_request},
};
use k256::ecdsa::SigningKey;
use kona_proof::{l1::OracleL1ChainProvider, l2::OracleL2ChainProvider};
use rkyv::rancor::Error as RkyvError;
use serde_bytes::ByteBuf;
use tokio_vsock::{VsockAddr, VsockListener, VsockStream};
use tracing::{error, info, warn};
use world_chain_proof_core::{
    BlobStore,
    boot::BootInfoStruct,
    range::{WorldRangeHardforkConfig, WorldRangeProofPublicValues},
    types::AggregationInputs,
    witness::{WitnessData, WorldRangeWitnessData, preimage_store::PreimageStore},
};
use world_chain_proof_kona_client_utils::{
    ETHDAWitnessExecutor, WitnessExecutor, get_inputs_for_pipeline,
};

use crate::protocol::{
    self, DEFAULT_VSOCK_PORT, EnclaveRequest, EnclaveResponse, PROTOCOL_VERSION,
};

const VMADDR_CID_ANY: u32 = 0xFFFF_FFFF;

// ──────────────────────────────────────────────────────────────────────────────────────
// Ephemeral signing key
// ──────────────────────────────────────────────────────────────────────────────────────

/// Ephemeral secp256k1 signing key for this enclave instance.
///
/// Initialised once at startup via [`init_signing_key`] using entropy from the NSM
/// `GetRandom` call. Guaranteed non-`None` after [`serve_forever`] has started.
static SIGNING_KEY: OnceLock<SigningKey> = OnceLock::new();

/// Initialises the enclave's ephemeral signing key from NSM-provided randomness.
///
/// Uses [`NsmRequest::GetRandom`] to obtain 32 bytes of hardware-backed entropy and
/// constructs a secp256k1 [`SigningKey`] from them. The key is stored in
/// [`SIGNING_KEY`] and the public key (uncompressed SEC1, 65 bytes, `0x04 || X || Y`) is returned.
///
/// # Errors
///
/// Returns an error if the NSM device returns an error or unexpected response, or if
/// the 32 bytes of entropy do not form a valid scalar (extremely unlikely).
fn init_signing_key(fd: i32) -> Result<Vec<u8>> {
    // Collect 32 bytes of NSM-provided random entropy.
    let seed = nsm_get_random_32(fd)?;

    let signing_key = SigningKey::from_bytes(&seed.into())
        .context("failed to create secp256k1 signing key from NSM entropy")?;

    let public_key_bytes = signing_key
        .verifying_key()
        .to_encoded_point(false) // uncompressed SEC1
        .as_bytes()
        .to_vec();

    // Ignore the error if the key was already set (race during tests / re-init).
    let _ = SIGNING_KEY.set(signing_key);

    info!(
        target: "world_chain::nitro",
        pubkey = hex::encode(&public_key_bytes),
        "ephemeral secp256k1 signing key initialised"
    );

    Ok(public_key_bytes)
}

/// Obtains 32 bytes of hardware-backed randomness from the NSM device.
///
/// Retries automatically because the NSM may legitimately return fewer than 32
/// bytes per call. If the device returns an empty response more than
/// `MAX_EMPTY_RETRIES` times in a row the function fails rather than spinning
/// indefinitely.
fn nsm_get_random_32(fd: i32) -> Result<[u8; 32]> {
    const MAX_EMPTY_RETRIES: usize = 32;

    let mut seed = [0u8; 32];
    let mut filled = 0usize;
    let mut empty_streak = 0usize;

    while filled < 32 {
        let response = nsm_process_request(fd, NsmRequest::GetRandom);
        match response {
            NsmResponse::GetRandom { random } => {
                if random.is_empty() {
                    empty_streak += 1;
                    if empty_streak > MAX_EMPTY_RETRIES {
                        return Err(anyhow!(
                            "NSM GetRandom returned empty data {MAX_EMPTY_RETRIES} consecutive times"
                        ));
                    }
                    continue;
                }
                empty_streak = 0;
                let needed = 32 - filled;
                let take = random.len().min(needed);
                seed[filled..filled + take].copy_from_slice(&random[..take]);
                filled += take;
            }
            NsmResponse::Error(err) => {
                return Err(anyhow!("NSM GetRandom error: {err:?}"));
            }
            other => {
                return Err(anyhow!("unexpected NSM response to GetRandom: {other:?}"));
            }
        }
    }

    Ok(seed)
}

/// Returns the signing key, panicking if it has not been initialised.
fn signing_key() -> &'static SigningKey {
    SIGNING_KEY
        .get()
        .expect("SIGNING_KEY must be initialised before use; call init_signing_key() at startup")
}

/// Computes the signing commitment and produces a 65-byte recoverable secp256k1 signature.
///
/// Commitment: `keccak256(l2_post_root ‖ l2_block_number_be ‖ rollup_config_hash)`
fn sign_boot_info(boot_info: &BootInfoStruct) -> Result<Vec<u8>> {
    let commitment = protocol::signing_commitment(boot_info);

    let (sig, rec_id) = signing_key()
        .sign_prehash_recoverable(&commitment)
        .context("secp256k1 signing failed")?;

    // 65 bytes: 64-byte compact sig (r ‖ s) + 1-byte EVM recovery id (27 or 28).
    // EVM ecrecover expects v = recovery_id + 27.
    let mut sig_bytes = Vec::with_capacity(65);
    sig_bytes.extend_from_slice(sig.to_bytes().as_slice());
    sig_bytes.push(rec_id.to_byte() + 27);

    Ok(sig_bytes)
}

// ──────────────────────────────────────────────────────────────────────────────────────
// Server loop
// ──────────────────────────────────────────────────────────────────────────────────────

/// Runs the enclave loop forever on the supplied vsock port.
pub async fn serve_forever(port: u32) -> Result<()> {
    // Initialise NSM and the ephemeral signing key before accepting any connections.
    let fd = nsm_init();
    if fd < 0 {
        return Err(anyhow!("nsm_init returned negative fd: {fd}"));
    }
    let _pubkey = init_signing_key(fd).context("failed to initialise enclave signing key")?;

    let addr = VsockAddr::new(VMADDR_CID_ANY, port);
    let mut listener = VsockListener::bind(addr).context("vsock bind")?;
    info!(target: "world_chain::nitro", port, "nitro enclave listening");

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(s) => s,
            Err(err) => {
                error!(target: "world_chain::nitro", %err, "vsock accept failed");
                continue;
            }
        };
        info!(target: "world_chain::nitro", ?peer, "accepted vsock connection");

        tokio::spawn(async move {
            if let Err(err) = handle_connection(stream).await {
                error!(target: "world_chain::nitro", %err, "request failed");
            }
        });
    }
}

async fn handle_connection(mut stream: VsockStream) -> Result<()> {
    let request: EnclaveRequest = protocol::read_frame(&mut stream).await?;
    let response = match dispatch(request).await {
        Ok(response) => response,
        Err(err) => EnclaveResponse::Error {
            message: format!("{err:#}"),
        },
    };
    protocol::write_frame(&mut stream, &response).await?;
    Ok(())
}

async fn dispatch(request: EnclaveRequest) -> Result<EnclaveResponse> {
    match request {
        EnclaveRequest::Range {
            version,
            witness_rkyv,
            expected_public_values,
            nonce,
        } => {
            check_version(version)?;
            handle_range(witness_rkyv, expected_public_values, nonce).await
        }
        EnclaveRequest::Aggregation {
            version,
            inputs,
            l1_headers_cbor,
            nonce,
        } => {
            check_version(version)?;
            handle_aggregation(inputs, l1_headers_cbor, nonce).await
        }
        EnclaveRequest::PublicKey { nonce } => handle_public_key(nonce),
    }
}

fn check_version(version: u32) -> Result<()> {
    if version != PROTOCOL_VERSION {
        return Err(anyhow!(
            "unsupported protocol version: enclave={PROTOCOL_VERSION}, host={version}"
        ));
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────────────
// Request handlers
// ──────────────────────────────────────────────────────────────────────────────────────

async fn handle_range(
    witness_rkyv: Vec<u8>,
    expected_public_values: Option<WorldRangeProofPublicValues>,
    nonce: [u8; 32],
) -> Result<EnclaveResponse> {
    info!(
        target: "world_chain::nitro",
        bytes = witness_rkyv.len(),
        "deserializing range witness"
    );
    let witness_data: WorldRangeWitnessData =
        rkyv::from_bytes::<WorldRangeWitnessData, RkyvError>(&witness_rkyv)
            .map_err(|err| anyhow!("failed to rkyv-deserialize WorldRangeWitnessData: {err}"))?;

    let world_schedule = witness_data.schedule.clone();
    let (oracle, beacon) = witness_data
        .get_oracle_and_blob_provider()
        .await
        .map_err(|err| anyhow!("failed to construct oracle/blob provider: {err}"))?;

    let boot_info = run_full_range_program(
        ETHDAWitnessExecutor::<PreimageStore, BlobStore>::new(),
        oracle,
        beacon,
        world_schedule,
    )
    .await?;

    if let Some(expected) = expected_public_values {
        ensure_boot_info_matches(&expected, &boot_info)?;
    }

    let signature = sign_boot_info(&boot_info)?;

    let user_data = protocol::range_user_data(&boot_info);
    let attestation_doc = request_attestation_doc(Some(&user_data), &nonce)?;

    Ok(EnclaveResponse::Range {
        boot_info,
        attestation_doc,
        signature,
    })
}

async fn handle_aggregation(
    inputs: AggregationInputs,
    l1_headers_cbor: Vec<u8>,
    nonce: [u8; 32],
) -> Result<EnclaveResponse> {
    let first = inputs
        .boot_infos
        .first()
        .ok_or_else(|| anyhow!("aggregation requires at least one range boot info"))?;
    let last = inputs.boot_infos.last().expect("checked non-empty above");

    // Verify consecutive range chain (same checks as the SP1 aggregation program).
    for pair in inputs.boot_infos.windows(2) {
        let (prev, next) = (&pair[0], &pair[1]);
        if prev.l2PostRoot != next.l2PreRoot {
            return Err(anyhow!("range chain broken: l2PostRoot/l2PreRoot mismatch"));
        }
        if prev.rollupConfigHash != next.rollupConfigHash {
            return Err(anyhow!("range chain broken: rollupConfigHash mismatch"));
        }
    }

    // Decode and verify the L1 header chain.
    let headers: Vec<Header> = serde_cbor::from_slice(&l1_headers_cbor)
        .map_err(|e| anyhow!("failed to decode L1 headers: {e}"))?;

    let mut l1_heads_map: std::collections::HashMap<alloy_primitives::B256, bool> = inputs
        .boot_infos
        .iter()
        .map(|bi| (bi.l1Head, false))
        .collect();

    let mut current_hash = inputs.latest_l1_checkpoint_head;
    for header in headers.iter().rev() {
        if header.hash_slow() != current_hash {
            return Err(anyhow!("L1 header chain hash mismatch at {current_hash}"));
        }
        if let Some(found) = l1_heads_map.get_mut(&current_hash) {
            *found = true;
        }
        current_hash = header.parent_hash;
    }

    for (l1_head, found) in &l1_heads_map {
        if !found {
            return Err(anyhow!(
                "l1Head {l1_head:?} not found in the provided header chain"
            ));
        }
    }

    let boot_info = BootInfoStruct {
        l1Head: inputs.latest_l1_checkpoint_head,
        l2PreRoot: first.l2PreRoot,
        l2PostRoot: last.l2PostRoot,
        l2BlockNumber: last.l2BlockNumber,
        rollupConfigHash: last.rollupConfigHash,
    };

    let signature = sign_boot_info(&boot_info)?;

    let user_data = protocol::aggregation_user_data(&boot_info, &inputs);
    let attestation_doc = request_attestation_doc(Some(&user_data), &nonce)?;

    Ok(EnclaveResponse::Aggregation {
        boot_info,
        attestation_doc,
        signature,
    })
}

/// Handles a [`EnclaveRequest::PublicKey`] request.
///
/// Calls the NSM device with the enclave's ephemeral public key embedded so that verifiers
/// can bind the secp256k1 key to the PCR measurements without an extra round-trip.
fn handle_public_key(nonce: [u8; 32]) -> Result<EnclaveResponse> {
    let attestation_doc = request_attestation_doc(None, &nonce)?;
    let public_key = signing_key()
        .verifying_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();
    Ok(EnclaveResponse::Attestation {
        attestation_doc,
        public_key,
    })
}

// ──────────────────────────────────────────────────────────────────────────────────────
// Range program runner
// ──────────────────────────────────────────────────────────────────────────────────────

/// Runs the full Kona derivation + execution range program. Mirrors the implementation in
/// `proofs/succinct/programs/range/utils/src/lib.rs` since that crate is excluded from
/// the workspace.
async fn run_full_range_program<E>(
    executor: E,
    oracle: Arc<PreimageStore>,
    beacon: BlobStore,
    world_schedule: WorldRangeHardforkConfig,
) -> Result<BootInfoStruct>
where
    E: WitnessExecutor<
            O = PreimageStore,
            B = BlobStore,
            L1 = OracleL1ChainProvider<PreimageStore>,
            L2 = OracleL2ChainProvider<PreimageStore>,
        > + Send
        + Sync,
{
    let (boot_info, input) = get_inputs_for_pipeline(oracle.clone())
        .await
        .map_err(|err| anyhow!("get_inputs_for_pipeline: {err}"))?;
    let boot_info = match input {
        Some((cursor, l1_provider, l2_provider)) => {
            let rollup_config = Arc::new(boot_info.rollup_config.clone());
            let l1_config = Arc::new(boot_info.l1_config.clone());

            let pipeline = executor
                .create_pipeline(
                    rollup_config,
                    l1_config,
                    cursor.clone(),
                    oracle,
                    beacon,
                    l1_provider,
                    l2_provider.clone(),
                )
                .await
                .map_err(|err| anyhow!("create_pipeline: {err}"))?;

            executor
                .run_with_world_schedule(
                    boot_info,
                    pipeline,
                    cursor,
                    l2_provider,
                    Some(world_schedule.clone()),
                )
                .await
                .map_err(|err| anyhow!("run_with_world_schedule: {err}"))?
        }
        None => boot_info,
    };

    Ok(BootInfoStruct::try_from_kona_boot_info(
        boot_info,
        &world_schedule,
    )?)
}

fn ensure_boot_info_matches(
    expected: &WorldRangeProofPublicValues,
    actual: &BootInfoStruct,
) -> Result<()> {
    let mismatches = [
        ("l1Head", expected.boot_info.l1_head == actual.l1Head),
        (
            "l2PreRoot",
            expected.boot_info.l2_pre_root == actual.l2PreRoot,
        ),
        (
            "l2PostRoot",
            expected.boot_info.l2_post_root == actual.l2PostRoot,
        ),
        (
            "l2BlockNumber",
            expected.boot_info.l2_block_number == actual.l2BlockNumber,
        ),
        (
            "rollupConfigHash",
            expected.boot_info.rollup_config_hash == actual.rollupConfigHash,
        ),
    ];
    if let Some((field, _)) = mismatches.iter().find(|(_, ok)| !ok) {
        return Err(anyhow!(
            "enclave-derived boot info disagrees with host expectation on {field}"
        ));
    }
    Ok(())
}

/// Calls the NSM device to produce an attestation document committing to `user_data`.
fn request_attestation_doc(user_data: Option<&[u8; 32]>, nonce: &[u8; 32]) -> Result<Vec<u8>> {
    let fd = nsm_init();
    if fd < 0 {
        return Err(anyhow!("nsm_init returned negative fd: {fd}"));
    }

    let public_key_bytes = signing_key()
        .verifying_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();

    let request = NsmRequest::Attestation {
        user_data: user_data.map(|d| ByteBuf::from(d.to_vec())),
        nonce: Some(ByteBuf::from(nonce.to_vec())),
        public_key: Some(ByteBuf::from(public_key_bytes)),
    };
    let response = nsm_process_request(fd, request);

    match response {
        NsmResponse::Attestation { document } => Ok(document),
        NsmResponse::Error(err) => {
            warn!(target: "world_chain::nitro", ?err, "nsm error");
            Err(anyhow!("nsm returned error: {err:?}"))
        }
        other => Err(anyhow!("unexpected nsm response: {other:?}")),
    }
}

/// Convenience entry point used by the binary target.
pub async fn run() -> Result<()> {
    let port = std::env::var("NITRO_VSOCK_PORT")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_VSOCK_PORT);
    serve_forever(port).await
}
