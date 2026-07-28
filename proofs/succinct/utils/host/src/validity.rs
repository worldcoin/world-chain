//! End-to-end validity proof helper built on the generic succinct prover session API.

use std::time::Duration;

use crate::{
    WorldSuccinctProver, aggregation_artifact_from_sp1_proof, range_artifact_from_sp1_proof,
};
use alloy_primitives::B256;
use anyhow::{Context, bail};
use sp1_sdk::SP1ProofWithPublicValues;
use world_chain_proof_core::artifacts::{AggregationProofArtifact, RangeProofArtifact};
use world_chain_proof_kona_host_utils::online::{
    OnlineHostConfig, RangeMetadata, RangeWitnessRequest, build_range_input,
    fetch_l1_header_by_hash,
};
use world_chain_proof_succinct_utils::{
    AggregationSessionRequest, RangeProofRequest, Sp1ProofRequest, Sp1SessionStatus,
};

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
}

struct BuiltRangeRequest {
    request: RangeProofRequest,
    metadata: RangeMetadata,
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

    let range_input = build_range_request(host, &request)
        .await
        .context("failed to build range proof request")?;
    let range_session_id = prover
        .submit(Sp1ProofRequest::Range(range_input.request))
        .await
        .context("failed to submit range proof")?;
    let range = wait_and_download_range(prover, range_session_id)
        .await
        .context("failed to complete range proof")?;

    validate_range_artifact(&range_input.metadata, &range)?;

    let aggregation_request = build_aggregation_request(host, &range)
        .await
        .context("failed to build aggregation proof request")?;
    let aggregation_session_id = prover
        .submit(Sp1ProofRequest::Aggregation(aggregation_request))
        .await
        .context("failed to submit aggregation proof")?;
    let aggregation = wait_and_download_aggregation(prover, aggregation_session_id)
        .await
        .context("failed to complete aggregation proof")?;

    validate_aggregation_artifact(&range_input.metadata, &aggregation)?;

    Ok(aggregation)
}

async fn build_range_request(
    host: &OnlineHostConfig,
    request: &ValidityProofRequest,
) -> anyhow::Result<BuiltRangeRequest> {
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

    let request = RangeProofRequest::from_witness_data(&input.witness)
        .context("failed to serialize SP1 range witness")?;

    Ok(BuiltRangeRequest {
        request,
        metadata: input.metadata,
    })
}

async fn build_aggregation_request(
    host: &OnlineHostConfig,
    range: &RangeProofArtifact,
) -> anyhow::Result<AggregationSessionRequest> {
    let l1_head = range.transition_public_values.l1Head;
    let l1_header = fetch_l1_header_by_hash(&reqwest::Client::new(), &host.l1_rpc, l1_head)
        .await
        .context("failed to fetch L1 header for aggregation proof")?;
    let l1_headers_cbor =
        serde_cbor::to_vec(&vec![l1_header]).context("failed to encode aggregation L1 headers")?;

    Ok(AggregationSessionRequest {
        transition_public_values: vec![range.transition_public_values.clone()],
        latest_l1_checkpoint_head: l1_head,
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
    let proof = wait_and_download_proof(prover, session_id, "STARK").await?;
    range_artifact_from_sp1_proof(&proof)
}

async fn wait_and_download_aggregation<P>(
    prover: &P,
    session_id: String,
) -> anyhow::Result<AggregationProofArtifact>
where
    P: WorldSuccinctProver + Sync,
{
    let proof = wait_and_download_proof(prover, session_id, "SNARK").await?;
    aggregation_artifact_from_sp1_proof(&proof)
}

async fn wait_and_download_proof<P>(
    prover: &P,
    session_id: String,
    session_label: &'static str,
) -> anyhow::Result<SP1ProofWithPublicValues>
where
    P: WorldSuccinctProver + Sync,
{
    loop {
        match prover.poll(&session_id).await? {
            Sp1SessionStatus::Running => tokio::time::sleep(Duration::from_secs(10)).await,
            Sp1SessionStatus::Completed => {
                return prover
                    .download(&session_id)
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
    metadata: &RangeMetadata,
    artifact: &RangeProofArtifact,
) -> anyhow::Result<()> {
    if artifact.transition_public_values.l1Head != metadata.l1_head {
        bail!(
            "range proof l1 head mismatch: expected {:?}, got {:?}",
            metadata.l1_head,
            artifact.transition_public_values.l1Head
        );
    }

    if artifact.transition_public_values.l2PreRoot != metadata.l2_pre_root {
        bail!(
            "range proof pre root mismatch: expected {:?}, got {:?}",
            metadata.l2_pre_root,
            artifact.transition_public_values.l2PreRoot
        );
    }

    if artifact.transition_public_values.l2PreBlockNumber != metadata.start_block {
        bail!(
            "range proof pre block mismatch: expected {}, got {}",
            metadata.start_block,
            artifact.transition_public_values.l2PreBlockNumber
        );
    }

    if artifact.transition_public_values.l2PostRoot != metadata.l2_post_root {
        bail!(
            "range proof post root mismatch: expected {:?}, got {:?}",
            metadata.l2_post_root,
            artifact.transition_public_values.l2PostRoot
        );
    }

    if artifact.transition_public_values.l2PostBlockNumber != metadata.end_block {
        bail!(
            "range proof block mismatch: expected {}, got {}",
            metadata.end_block,
            artifact.transition_public_values.l2PostBlockNumber
        );
    }

    if artifact.transition_public_values.rollupConfigHash != metadata.rollup_config_hash {
        bail!(
            "range proof rollup config hash mismatch: expected {:?}, got {:?}",
            metadata.rollup_config_hash,
            artifact.transition_public_values.rollupConfigHash
        );
    }

    Ok(())
}

fn validate_aggregation_artifact(
    metadata: &RangeMetadata,
    artifact: &AggregationProofArtifact,
) -> anyhow::Result<()> {
    let transition = &artifact.public_values.transitionPublicValues;

    if transition.l2PreRoot != metadata.l2_pre_root {
        bail!(
            "aggregation pre root mismatch: expected {:?}, got {:?}",
            metadata.l2_pre_root,
            transition.l2PreRoot
        );
    }

    if transition.l2PreBlockNumber != metadata.start_block {
        bail!(
            "aggregation pre block mismatch: expected {}, got {}",
            metadata.start_block,
            transition.l2PreBlockNumber
        );
    }

    if transition.l2PostRoot != metadata.l2_post_root {
        bail!(
            "aggregation post root mismatch: expected {:?}, got {:?}",
            metadata.l2_post_root,
            transition.l2PostRoot
        );
    }

    if transition.l2PostBlockNumber != metadata.end_block {
        bail!(
            "aggregation block mismatch: expected {}, got {}",
            metadata.end_block,
            transition.l2PostBlockNumber
        );
    }

    if transition.l1Head != metadata.l1_head {
        bail!(
            "aggregation l1 head mismatch: expected {:?}, got {:?}",
            metadata.l1_head,
            transition.l1Head
        );
    }

    if transition.rollupConfigHash != metadata.rollup_config_hash {
        bail!(
            "aggregation rollup config hash mismatch: expected {:?}, got {:?}",
            metadata.rollup_config_hash,
            transition.rollupConfigHash
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use world_chain_proof_core::{boot::TransitionPublicValues, types::AggregationPublicValues};

    use super::*;

    fn metadata() -> RangeMetadata {
        RangeMetadata {
            start_block: 10,
            end_block: 20,
            finalized_l2_head: Some(30),
            l1_head: B256::repeat_byte(0x11),
            l2_pre_root: B256::repeat_byte(0x22),
            l2_post_root: B256::repeat_byte(0x33),
            rollup_config_hash: B256::repeat_byte(0x44),
            active_fork: "Jovian".to_string(),
            world_spec_id: "JOVIAN".to_string(),
        }
    }

    fn range_artifact(metadata: &RangeMetadata) -> RangeProofArtifact {
        RangeProofArtifact {
            transition_public_values: TransitionPublicValues {
                l1Head: metadata.l1_head,
                l2PreRoot: metadata.l2_pre_root,
                l2PreBlockNumber: metadata.start_block,
                l2PostRoot: metadata.l2_post_root,
                l2PostBlockNumber: metadata.end_block,
                rollupConfigHash: metadata.rollup_config_hash,
            },
            proof: vec![1, 2, 3],
        }
    }

    fn aggregation_artifact(metadata: &RangeMetadata) -> AggregationProofArtifact {
        AggregationProofArtifact {
            public_values: AggregationPublicValues {
                transitionPublicValues: TransitionPublicValues {
                    l1Head: metadata.l1_head,
                    l2PreRoot: metadata.l2_pre_root,
                    l2PreBlockNumber: metadata.start_block,
                    l2PostRoot: metadata.l2_post_root,
                    l2PostBlockNumber: metadata.end_block,
                    rollupConfigHash: metadata.rollup_config_hash,
                },
                multiBlockVKey: B256::repeat_byte(0x55),
            },
            proof: vec![4, 5, 6],
        }
    }

    #[test]
    fn range_validation_rejects_post_root_mismatch() {
        let metadata = metadata();
        let mut artifact = range_artifact(&metadata);
        artifact.transition_public_values.l2PostRoot = B256::repeat_byte(0x99);

        let error = validate_range_artifact(&metadata, &artifact).unwrap_err();

        assert!(error.to_string().contains("range proof post root mismatch"));
    }

    #[test]
    fn aggregation_validation_rejects_post_root_mismatch() {
        let metadata = metadata();
        let mut artifact = aggregation_artifact(&metadata);
        artifact.public_values.transitionPublicValues.l2PostRoot = B256::repeat_byte(0x99);

        let error = validate_aggregation_artifact(&metadata, &artifact).unwrap_err();

        assert!(error.to_string().contains("aggregation post root mismatch"));
    }
}
