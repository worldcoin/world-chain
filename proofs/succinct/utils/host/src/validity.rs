//! End-to-end validity proving for one L2 block range.
//!
//! Splits the range into sub-ranges, builds each witness over RPC, proves every sub-range with
//! a [`WorldSuccinctProver`] backend, and aggregates the results into a single
//! [`AggregationProofArtifact`] whose outputs commit to the claimed output root.

use alloy_primitives::{Address, B256};
use anyhow::{Context, bail};
use reqwest::blocking::Client;
use world_chain_proof_core::{artifacts::AggregationProofArtifact, types::AggregationInputs};
use world_chain_proof_succinct_utils::{
    AggregationProofRequest, RangeProofRequest, WorldSuccinctProver,
};

use crate::{
    L2BlockRange,
    online::{
        OnlineHostConfig, RangeWitnessRequest, build_range_input, fetch_l1_header_by_hash,
        resolve_l1_head,
    },
    split_range,
};

/// One validity-proof job covering L2 blocks `(start_block, end_block]`.
#[derive(Clone, Copy, Debug)]
pub struct ValidityProofRequest {
    /// Exclusive lower bound: the agreed parent block.
    pub start_block: u64,
    /// Inclusive upper bound: the claimed block.
    pub end_block: u64,
    /// L1 head pinning every range witness and the aggregation checkpoint. Resolved from
    /// finalized L1 when `None`.
    pub l1_head: Option<B256>,
    /// Allow proving blocks newer than the finalized L2 head.
    pub allow_unfinalized: bool,
    /// Number of sub-ranges proved independently before aggregation, clamped to the range
    /// length.
    pub split_count: u64,
    /// Prover address committed by the aggregation guest for on-chain attribution.
    pub prover_address: Address,
}

impl ValidityProofRequest {
    pub fn new(
        start: u64,
        end: u64,
        l1_head: Option<B256>,
        allow_unfinalized: bool,
        split_count: u64,
        prover_address: Address,
    ) -> Self {
        Self {
            start_block: start,
            end_block: end,
            l1_head,
            allow_unfinalized,
            split_count,
            prover_address,
        }
    }
}

/// Proves the transition over `(start_block, end_block]` and aggregates it into one artifact.
///
/// Synchronous and long-running (witness generation plus proving); it must run on a
/// blocking-capable thread (use `tokio::task::spawn_blocking` from async code).
///
/// Sub-ranges are built and proved sequentially. A single CPU proof already saturates the
/// host, so intra-job parallelism would only thrash it; cross-job parallelism is handled one
/// level up by the worker's concurrency permits. Sequential proving also keeps only one
/// witness alive at a time, bounding memory, and avoids sharing the prover's runtime across
/// threads.
pub fn prove_validity<P>(
    host: &OnlineHostConfig,
    prover: &P,
    request: ValidityProofRequest,
) -> anyhow::Result<AggregationProofArtifact>
where
    P: WorldSuccinctProver,
    P::Error: Into<anyhow::Error>,
{
    let range = L2BlockRange::new(request.start_block, request.end_block)?;
    // Sub-ranges are half-open [start, end) bounds; each is proved as (start, end] with
    // `start` as the agreed parent block, matching the factory's block-interval convention.
    let ranges = split_range(range, request.split_count)?;

    let client = Client::new();
    let l1_head = match request.l1_head {
        Some(hash) => hash,
        None => resolve_l1_head(&client, &host.l2_rpc, &host.l1_rpc, request.end_block)?,
    };

    let mut boot_infos = Vec::with_capacity(ranges.len());
    let mut range_proofs = Vec::with_capacity(ranges.len());
    for sub_range in &ranges {
        tracing::info!(
            start = sub_range.start + 1,
            end = sub_range.end,
            "building range witness"
        );
        let input = build_range_input(
            host,
            RangeWitnessRequest {
                start_block: sub_range.start,
                end_block: sub_range.end,
                l1_head: Some(l1_head),
                allow_unfinalized: request.allow_unfinalized,
            },
        )?;

        tracing::info!(
            start = sub_range.start + 1,
            end = sub_range.end,
            "proving range"
        );
        let range_request = RangeProofRequest::from_witness_data(&input.witness, None)
            .context("failed to serialize range witness")?;
        let artifact = prover.prove_range(range_request).map_err(Into::into)?;

        if artifact.boot_info.l2PostRoot != input.metadata.l2_post_root {
            bail!(
                "range proof post root mismatch at block {}: witness {:?}, proof {:?}",
                input.metadata.end_block,
                input.metadata.l2_post_root,
                artifact.boot_info.l2PostRoot,
            );
        }

        boot_infos.push(artifact.boot_info);
        range_proofs.push(artifact.proof);
    }

    let l1_header = fetch_l1_header_by_hash(&client, &host.l1_rpc, l1_head)?;
    let l1_headers_cbor =
        serde_cbor::to_vec(&vec![l1_header]).context("CBOR-encoding L1 header failed")?;

    tracing::info!(ranges = ranges.len(), "aggregating range proofs");
    prover
        .prove_aggregation(AggregationProofRequest {
            inputs: AggregationInputs {
                boot_infos,
                latest_l1_checkpoint_head: l1_head,
                multi_block_vkey: prover.multi_block_vkey(),
                prover_address: request.prover_address,
            },
            l1_headers_cbor,
            range_proofs,
        })
        .map_err(Into::into)
}
