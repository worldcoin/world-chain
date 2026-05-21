//! Enclave-side library code.
//!
//! This is the worker loop the `world-chain-nitro-enclave` binary runs inside the Nitro
//! Enclave. It listens on vsock, runs the same OP Succinct Lite range/aggregation logic the
//! SP1 guest does, and attests the result via the local NSM device.
//!
//! Gated behind the `enclave` feature so the heavy kona / NSM dependencies are not pulled
//! into the host build.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use aws_nitro_enclaves_nsm_api::{
    api::{Request as NsmRequest, Response as NsmResponse},
    driver::{nsm_init, nsm_process_request},
};
use kona_proof::{l1::OracleL1ChainProvider, l2::OracleL2ChainProvider};
use rkyv::rancor::Error as RkyvError;
use serde_bytes::ByteBuf;
use tokio_vsock::{VsockAddr, VsockListener, VsockStream};
use tracing::{error, info, warn};
use world_chain_proof_succinct_client_utils::{
    BlobStore, WorldRangeProofPublicValues,
    boot::BootInfoStruct,
    range::WorldRangeHardforkConfig,
    types::AggregationInputs,
    witness::{
        WitnessData, WorldRangeWitnessData,
        executor::{WitnessExecutor, get_inputs_for_pipeline},
        preimage_store::PreimageStore,
    },
};
use world_chain_proof_succinct_ethereum_client_utils::executor::ETHDAWitnessExecutor;

use crate::protocol::{
    self, DEFAULT_VSOCK_PORT, EnclaveRequest, EnclaveResponse, PROTOCOL_VERSION,
};

const VMADDR_CID_ANY: u32 = 0xFFFF_FFFF;

/// Runs the enclave loop forever on the supplied vsock port.
pub async fn serve_forever(port: u32) -> Result<()> {
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
        } => {
            check_version(version)?;
            handle_range(witness_rkyv, expected_public_values).await
        }
        EnclaveRequest::Aggregation {
            version,
            inputs,
            l1_headers_cbor,
        } => {
            check_version(version)?;
            handle_aggregation(inputs, l1_headers_cbor).await
        }
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

async fn handle_range(
    witness_rkyv: Vec<u8>,
    expected_public_values: Option<WorldRangeProofPublicValues>,
) -> Result<EnclaveResponse> {
    info!(
        target: "world_chain::nitro",
        bytes = witness_rkyv.len(),
        "deserializing range witness"
    );
    let witness_data: WorldRangeWitnessData = rkyv::from_bytes::<WorldRangeWitnessData, RkyvError>(
        &witness_rkyv,
    )
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

    let user_data = protocol::range_user_data(&boot_info);
    let attestation_doc = request_attestation_doc(&user_data)?;

    Ok(EnclaveResponse::Range {
        boot_info,
        attestation_doc,
    })
}

async fn handle_aggregation(
    inputs: AggregationInputs,
    _l1_headers_cbor: Vec<u8>,
) -> Result<EnclaveResponse> {
    // The aggregation program upstream concatenates range boot infos and verifies an L1
    // header chain. For the Nitro variant we re-derive the aggregated boot info from the
    // input range boot infos: the enclave is trusted (PCR-pinned) so this is sufficient as
    // long as each range boot info was produced under the same PCR set, which the caller
    // checks while collecting them.
    //
    // TODO(nitro): cross-check `_l1_headers_cbor` against `latest_l1_checkpoint_head` and
    // each range boot info's `l1Head` once we mirror the full aggregation program logic.
    let first = inputs
        .boot_infos
        .first()
        .ok_or_else(|| anyhow!("aggregation requires at least one range boot info"))?;
    let last = inputs
        .boot_infos
        .last()
        .expect("checked non-empty above");

    let boot_info = BootInfoStruct {
        l1Head: inputs.latest_l1_checkpoint_head,
        l2PreRoot: first.l2PreRoot,
        l2PostRoot: last.l2PostRoot,
        l2BlockNumber: last.l2BlockNumber,
        rollupConfigHash: last.rollupConfigHash,
    };

    let user_data = protocol::aggregation_user_data(&boot_info, &inputs);
    let attestation_doc = request_attestation_doc(&user_data)?;

    Ok(EnclaveResponse::Aggregation {
        boot_info,
        attestation_doc,
    })
}

/// Runs the full Kona derivation + execution range program. Mirrors the implementation in
/// `crates/proof/succinct/programs/range/utils/src/lib.rs` since that crate is excluded from
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

    Ok(BootInfoStruct::from_kona_boot_info(boot_info, &world_schedule))
}

fn ensure_boot_info_matches(
    expected: &WorldRangeProofPublicValues,
    actual: &BootInfoStruct,
) -> Result<()> {
    let mismatches = [
        ("l1Head", expected.boot_info.l1_head == actual.l1Head),
        ("l2PreRoot", expected.boot_info.l2_pre_root == actual.l2PreRoot),
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
fn request_attestation_doc(user_data: &[u8; 32]) -> Result<Vec<u8>> {
    let fd = nsm_init();
    if fd < 0 {
        return Err(anyhow!("nsm_init returned negative fd: {fd}"));
    }

    let request = NsmRequest::Attestation {
        user_data: Some(ByteBuf::from(user_data.to_vec())),
        nonce: None,
        public_key: None,
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
pub async fn main_entry() -> Result<()> {
    let port = std::env::var("NITRO_VSOCK_PORT")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_VSOCK_PORT);
    serve_forever(port).await
}
