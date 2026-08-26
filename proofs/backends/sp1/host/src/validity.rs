//! End-to-end validity proof helper built on the generic succinct prover session API.

use crate::{
    WorldSuccinctProver, aggregation_artifact_from_sp1_proof, range_artifact_from_sp1_proof,
};
use alloy_primitives::B256;
use anyhow::{Context, bail};
use world_chain_proof_core::artifacts::{AggregationProofArtifact, RangeProofArtifact};
use world_chain_proof_kona_host::online::{
    OnlineHostConfig, RangeMetadata, RangeWitnessRequest, build_range_input,
    fetch_l1_header_by_hash, resolve_l1_head,
};
use world_chain_proof_sp1_types::{AggregationSessionRequest, RangeProofRequest, Sp1ProofRequest};

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
    /// Number of contiguous range proofs to split the request into.
    pub split_count: u64,
}

struct BuiltRangeRequest {
    request: RangeProofRequest,
    metadata: RangeMetadata,
}

/// Builds, proves, and aggregates an SP1 validity proof over one or more contiguous ranges.
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

    let split_count = request.split_count.max(1);
    let total_blocks = request.end_block - request.start_block;
    if split_count > total_blocks {
        bail!("cannot split {total_blocks} block(s) into {split_count} range proofs");
    }

    // Pin every sub-range to one L1 head so a single header covers the aggregation.
    let l1_head = match request.l1_head {
        Some(hash) => hash,
        None => resolve_l1_head(
            &reqwest::Client::new(),
            &host.l2_rpc,
            &host.l1_rpc,
            request.end_block,
        )
        .await
        .context("failed to resolve L1 head for range proofs")?,
    };

    // Build and submit every range first; network backends then prove them concurrently.
    let mut sessions = Vec::with_capacity(split_count as usize);
    for (range_start, range_end) in
        split_boundaries(request.start_block, request.end_block, split_count)
    {
        let range_input = build_range_request(host, &request, range_start, range_end, l1_head)
            .await
            .with_context(|| {
                format!("failed to build range proof request for ({range_start}, {range_end}]")
            })?;
        let session_id = prover
            .submit(Sp1ProofRequest::Range(range_input.request))
            .await
            .with_context(|| {
                format!("failed to submit range proof for ({range_start}, {range_end}]")
            })?;
        sessions.push((session_id, range_input.metadata));
    }

    let mut ranges = Vec::with_capacity(sessions.len());
    let mut metadatas = Vec::with_capacity(sessions.len());
    for (session_id, metadata) in sessions {
        let range = wait_for_range(prover, session_id).await.with_context(|| {
            format!(
                "failed to complete range proof for ({}, {}]",
                metadata.start_block, metadata.end_block
            )
        })?;
        validate_range_artifact(&metadata, &range)?;
        ranges.push(range);
        metadatas.push(metadata);
    }

    let aggregation_request = build_aggregation_request(host, l1_head, &ranges)
        .await
        .context("failed to build aggregation proof request")?;
    let aggregation_session_id = prover
        .submit(Sp1ProofRequest::Aggregation(aggregation_request))
        .await
        .context("failed to submit aggregation proof")?;
    let aggregation = wait_for_aggregation(prover, aggregation_session_id)
        .await
        .context("failed to complete aggregation proof")?;

    let (Some(first), Some(last)) = (metadatas.first(), metadatas.last()) else {
        bail!("no range proofs were produced");
    };
    validate_aggregation_artifact(first, last, &aggregation)?;

    Ok(aggregation)
}

/// Splits `(start_block, end_block]` into `split_count` contiguous ranges of near-equal size,
/// with the remainder distributed to the earliest ranges.
fn split_boundaries(start_block: u64, end_block: u64, split_count: u64) -> Vec<(u64, u64)> {
    let total = end_block - start_block;
    let base = total / split_count;
    let remainder = total % split_count;
    let mut boundaries = Vec::with_capacity(split_count as usize);
    let mut cursor = start_block;
    for index in 0..split_count {
        let size = base + u64::from(index < remainder);
        boundaries.push((cursor, cursor + size));
        cursor += size;
    }
    boundaries
}

async fn build_range_request(
    host: &OnlineHostConfig,
    request: &ValidityProofRequest,
    start_block: u64,
    end_block: u64,
    l1_head: B256,
) -> anyhow::Result<BuiltRangeRequest> {
    let input = build_range_input(
        host,
        RangeWitnessRequest {
            start_block,
            end_block,
            l1_head: Some(l1_head),
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
    l1_head: B256,
    ranges: &[RangeProofArtifact],
) -> anyhow::Result<AggregationSessionRequest> {
    let l1_header = fetch_l1_header_by_hash(&reqwest::Client::new(), &host.l1_rpc, l1_head)
        .await
        .context("failed to fetch L1 header for aggregation proof")?;
    let l1_headers_cbor =
        serde_cbor::to_vec(&vec![l1_header]).context("failed to encode aggregation L1 headers")?;

    Ok(AggregationSessionRequest {
        transition_public_values: ranges
            .iter()
            .map(|range| range.transition_public_values.clone())
            .collect(),
        latest_l1_checkpoint_head: l1_head,
        l1_headers_cbor,
        range_proofs: ranges.iter().map(|range| range.proof.clone()).collect(),
    })
}

async fn wait_for_range<P>(prover: &P, session_id: String) -> anyhow::Result<RangeProofArtifact>
where
    P: WorldSuccinctProver + Sync,
{
    let proof = prover
        .wait(&session_id)
        .await
        .context("failed to wait for STARK proof")?;
    range_artifact_from_sp1_proof(&proof)
}

async fn wait_for_aggregation<P>(
    prover: &P,
    session_id: String,
) -> anyhow::Result<AggregationProofArtifact>
where
    P: WorldSuccinctProver + Sync,
{
    let proof = prover
        .wait(&session_id)
        .await
        .context("failed to wait for SNARK proof")?;
    aggregation_artifact_from_sp1_proof(&proof)
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

/// Checks the aggregated transition against the first range's pre state and the last range's
/// post state.
fn validate_aggregation_artifact(
    first: &RangeMetadata,
    last: &RangeMetadata,
    artifact: &AggregationProofArtifact,
) -> anyhow::Result<()> {
    let transition = &artifact.public_values.transitionPublicValues;

    if transition.l2PreRoot != first.l2_pre_root {
        bail!(
            "aggregation pre root mismatch: expected {:?}, got {:?}",
            first.l2_pre_root,
            transition.l2PreRoot
        );
    }

    if transition.l2PreBlockNumber != first.start_block {
        bail!(
            "aggregation pre block mismatch: expected {}, got {}",
            first.start_block,
            transition.l2PreBlockNumber
        );
    }

    if transition.l2PostRoot != last.l2_post_root {
        bail!(
            "aggregation post root mismatch: expected {:?}, got {:?}",
            last.l2_post_root,
            transition.l2PostRoot
        );
    }

    if transition.l2PostBlockNumber != last.end_block {
        bail!(
            "aggregation block mismatch: expected {}, got {}",
            last.end_block,
            transition.l2PostBlockNumber
        );
    }

    if transition.l1Head != last.l1_head {
        bail!(
            "aggregation l1 head mismatch: expected {:?}, got {:?}",
            last.l1_head,
            transition.l1Head
        );
    }

    if transition.rollupConfigHash != last.rollup_config_hash {
        bail!(
            "aggregation rollup config hash mismatch: expected {:?}, got {:?}",
            last.rollup_config_hash,
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

        let error = validate_aggregation_artifact(&metadata, &metadata, &artifact).unwrap_err();

        assert!(error.to_string().contains("aggregation post root mismatch"));
    }

    #[test]
    fn aggregation_validation_spans_first_and_last_range() {
        let mut first = metadata();
        let mut last = metadata();
        first.end_block = 15;
        first.l2_post_root = B256::repeat_byte(0x66);
        last.start_block = 15;
        last.l2_pre_root = B256::repeat_byte(0x66);

        let mut artifact = aggregation_artifact(&last);
        artifact.public_values.transitionPublicValues.l2PreRoot = first.l2_pre_root;
        artifact
            .public_values
            .transitionPublicValues
            .l2PreBlockNumber = first.start_block;

        validate_aggregation_artifact(&first, &last, &artifact).unwrap();
    }

    #[test]
    fn split_boundaries_tile_the_interval_evenly() {
        assert_eq!(split_boundaries(10, 20, 1), vec![(10, 20)]);
        assert_eq!(split_boundaries(10, 20, 2), vec![(10, 15), (15, 20)]);
        assert_eq!(
            split_boundaries(10, 20, 3),
            vec![(10, 14), (14, 17), (17, 20)]
        );
        assert_eq!(split_boundaries(0, 3, 3), vec![(0, 1), (1, 2), (2, 3)]);
    }
}
