//! End-to-end validity proof helper built on the generic succinct prover session API.

use std::time::Duration;

use alloy_primitives::{Address, B256, keccak256};
use anyhow::{Context, bail};
use world_chain_proof_core::artifacts::{
    AggregationProofArtifact, ProofArtifact, RangeProofArtifact,
};
use world_chain_proof_kona_host_utils::online::{
    OnlineHostConfig, RangeWitnessRequest, build_range_input, fetch_l1_header_by_hash,
};
use world_chain_proof_succinct_utils::{
    AggregationSessionRequest, ProofRequest as SuccinctProofRequest, RangeProofRequest,
    Sp1SessionStatus, WorldSuccinctProver,
};
use world_chain_prover_service::{ProofRequestId, SessionType};

/// Request for proving one contiguous L2 validity range and aggregating it into a final proof.
#[derive(Clone, Debug)]
pub struct ValidityProofRequest {
    /// L2 block number immediately before the proved range.
    pub start_block: u64,
    /// L2 block number at the end of the proved range.
    pub end_block: u64,
    /// Optional L1 head hash pinning the witness data.
    pub l1_head: Option<B256>,
    /// Allow proving blocks newer than the finalized L2 head.
    pub allow_unfinalized: bool,
    /// Number of range proofs to split the request into. Only one is currently supported.
    pub split_count: u64,
    /// Prover address committed by the aggregation guest.
    pub prover_address: Address,
}

/// Builds, proves, and aggregates a single-range SP1 validity proof.
pub async fn prove_validity<P>(
    host: &OnlineHostConfig,
    prover: &P,
    request: ValidityProofRequest,
) -> anyhow::Result<AggregationProofArtifact>
where
    P: WorldSuccinctProver + Sync,
{
    if request.end_block <= request.start_block {
        bail!(
            "end block {} must be greater than start block {}",
            request.end_block,
            request.start_block
        );
    }

    if request.split_count.max(1) != 1 {
        bail!(
            "SP1 validity proving currently supports exactly one range proof, got {}",
            request.split_count
        );
    }

    let proof_id = validity_proof_id(&request);
    let range_request = build_range_request(host, &request)
        .await
        .context("failed to build range proof request")?;
    let range_session_id = prover
        .submit(
            proof_id,
            SessionType::Stark,
            SuccinctProofRequest::Range(range_request),
        )
        .await
        .context("failed to submit range proof")?;
    let range = wait_and_download_range(prover, range_session_id)
        .await
        .context("failed to complete range proof")?;

    validate_range_artifact(host, &request, &range)?;

    let aggregation_request = build_aggregation_request(host, &request, &range)
        .await
        .context("failed to build aggregation proof request")?;
    let aggregation_session_id = prover
        .submit(
            proof_id,
            SessionType::Snark,
            SuccinctProofRequest::Aggregation(aggregation_request),
        )
        .await
        .context("failed to submit aggregation proof")?;
    let aggregation = wait_and_download_aggregation(prover, aggregation_session_id)
        .await
        .context("failed to complete aggregation proof")?;

    validate_aggregation_artifact(host, &request, &range, &aggregation)?;

    Ok(aggregation)
}

fn validity_proof_id(request: &ValidityProofRequest) -> ProofRequestId {
    let mut bytes = Vec::with_capacity(16 + 8 + 8 + 32 + 20);
    bytes.extend_from_slice(b"sp1-validity-v1");
    bytes.extend_from_slice(&request.start_block.to_be_bytes());
    bytes.extend_from_slice(&request.end_block.to_be_bytes());
    bytes.extend_from_slice(request.l1_head.unwrap_or_default().as_slice());
    bytes.extend_from_slice(request.prover_address.as_slice());
    ProofRequestId(keccak256(bytes))
}

async fn build_range_request(
    host: &OnlineHostConfig,
    request: &ValidityProofRequest,
) -> anyhow::Result<RangeProofRequest> {
    let input = build_range_input(
        host,
        RangeWitnessRequest {
            start_block: request.start_block,
            end_block: request.end_block,
            l1_head: request.l1_head,
            allow_unfinalized: request.allow_unfinalized,
        },
    )
    .await
    .context("failed to build SP1 range witness")?;

    RangeProofRequest::from_witness_data(&input.witness, None)
        .context("failed to serialize SP1 range witness")
}

async fn build_aggregation_request(
    host: &OnlineHostConfig,
    request: &ValidityProofRequest,
    range: &RangeProofArtifact,
) -> anyhow::Result<AggregationSessionRequest> {
    let l1_head = range.boot_info.l1Head;
    let l1_header = fetch_l1_header_by_hash(&reqwest::Client::new(), &host.l1_rpc, l1_head)
        .await
        .context("failed to fetch L1 header for aggregation proof")?;
    let l1_headers_cbor =
        serde_cbor::to_vec(&vec![l1_header]).context("failed to encode aggregation L1 headers")?;

    Ok(AggregationSessionRequest {
        boot_infos: vec![range.boot_info.clone()],
        latest_l1_checkpoint_head: l1_head,
        prover_address: request.prover_address,
        l1_headers_cbor,
        range_proofs: vec![range.proof.clone()],
    })
}

async fn wait_and_download_range<P>(
    prover: &P,
    session_id: String,
) -> anyhow::Result<RangeProofArtifact>
where
    P: WorldSuccinctProver + Sync,
{
    let artifact =
        wait_and_download_artifact(prover, SessionType::Stark, session_id.clone()).await?;
    let ProofArtifact::Range(range) = artifact else {
        bail!("expected range proof artifact for STARK session {session_id}");
    };
    Ok(range)
}

async fn wait_and_download_aggregation<P>(
    prover: &P,
    session_id: String,
) -> anyhow::Result<AggregationProofArtifact>
where
    P: WorldSuccinctProver + Sync,
{
    let artifact =
        wait_and_download_artifact(prover, SessionType::Snark, session_id.clone()).await?;
    let ProofArtifact::Aggregation(aggregation) = artifact else {
        bail!("expected aggregation proof artifact for SNARK session {session_id}");
    };
    Ok(aggregation)
}

async fn wait_and_download_artifact<P>(
    prover: &P,
    session_type: SessionType,
    session_id: String,
) -> anyhow::Result<ProofArtifact>
where
    P: WorldSuccinctProver + Sync,
{
    let session_label = session_type.as_str();
    loop {
        match prover
            .poll(session_id.clone(), session_type.clone())
            .await?
        {
            Sp1SessionStatus::Running => tokio::time::sleep(Duration::from_secs(10)).await,
            Sp1SessionStatus::Completed => {
                return prover
                    .download(session_id, session_type)
                    .await
                    .with_context(|| format!("failed to download {session_label} proof"));
            }
            Sp1SessionStatus::Failed(reason) => {
                bail!("{session_label} proof session {session_id} failed: {reason}");
            }
            Sp1SessionStatus::NotFound => {
                bail!("{session_label} proof session {session_id} not found by prover");
            }
        }
    }
}

fn validate_range_artifact(
    host: &OnlineHostConfig,
    request: &ValidityProofRequest,
    artifact: &RangeProofArtifact,
) -> anyhow::Result<()> {
    if artifact.boot_info.l2BlockNumber != request.end_block {
        bail!(
            "range proof block mismatch: expected {}, got {}",
            request.end_block,
            artifact.boot_info.l2BlockNumber
        );
    }

    if let Some(expected_l1_head) = request.l1_head {
        if artifact.boot_info.l1Head != expected_l1_head {
            bail!(
                "range proof l1 head mismatch: expected {:?}, got {:?}",
                expected_l1_head,
                artifact.boot_info.l1Head
            );
        }
    }

    if artifact.boot_info.rollupConfigHash != host.rollup_config_hash {
        bail!(
            "range proof rollup config hash mismatch: expected {:?}, got {:?}",
            host.rollup_config_hash,
            artifact.boot_info.rollupConfigHash
        );
    }

    Ok(())
}

fn validate_aggregation_artifact(
    host: &OnlineHostConfig,
    request: &ValidityProofRequest,
    range: &RangeProofArtifact,
    artifact: &AggregationProofArtifact,
) -> anyhow::Result<()> {
    if artifact.outputs.l2PostRoot != range.boot_info.l2PostRoot {
        bail!(
            "aggregation post root mismatch: expected {:?}, got {:?}",
            range.boot_info.l2PostRoot,
            artifact.outputs.l2PostRoot
        );
    }

    if artifact.outputs.l2BlockNumber != request.end_block {
        bail!(
            "aggregation block mismatch: expected {}, got {}",
            request.end_block,
            artifact.outputs.l2BlockNumber
        );
    }

    if artifact.outputs.l1Head != range.boot_info.l1Head {
        bail!(
            "aggregation l1 head mismatch: expected {:?}, got {:?}",
            range.boot_info.l1Head,
            artifact.outputs.l1Head
        );
    }

    if artifact.outputs.rollupConfigHash != host.rollup_config_hash {
        bail!(
            "aggregation rollup config hash mismatch: expected {:?}, got {:?}",
            host.rollup_config_hash,
            artifact.outputs.rollupConfigHash
        );
    }

    Ok(())
}
