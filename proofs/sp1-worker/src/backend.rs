//! SP1 validity-proof backend for the defender's [`ProofWorker`].

use alloy_primitives::{Address, B256};
use alloy_sol_types::SolValue;
use anyhow::{Context, bail};
use reqwest::blocking::Client;
use world_chain_proof_core::{artifacts::AggregationProofArtifact, types::AggregationInputs};
use world_chain_proof_kona_host_utils::online::{
    OnlineHostConfig, RangeWitnessRequest, build_range_input, fetch_l1_header_by_hash,
};
use world_chain_proof_succinct_host_utils::validity::{ValidityProofRequest, prove_validity};
use world_chain_proof_succinct_utils::{
    AggregationProofRequest, RangeProofArtifact, RangeProofRequest, WorldSuccinctProver,
};
use world_chain_proof_worker::ProofJobBackend;
use world_chain_prover_service::{
    BackendProofId, BackendProofState, BackendUpdate, ProofBackend, ProofData, ProofRequest,
};

/// Configuration for [`Sp1Backend`].
#[derive(Clone, Copy, Debug)]
pub struct Sp1BackendConfig {
    /// L2 blocks between a proposal's parent and its claimed block (the proof system's
    /// `blockInterval` domain constant). The proved range is
    /// `(l2_block_number - block_interval, l2_block_number]`.
    pub block_interval: u64,
    /// Number of equal-length sub-ranges proved independently before aggregation.
    pub split_count: u64,
    /// Prover address committed by the aggregation guest for on-chain attribution.
    pub prover_address: Address,
    /// Allow proving blocks newer than the finalized L2 head.
    pub allow_unfinalized: bool,
}

/// [`ProofJobBackend`] for the [`ProofBackend::Sp1`] lane: builds witnesses over RPC and proves
/// them with a [`WorldSuccinctProver`] (the sp1-sdk env prover in production).
pub struct Sp1Backend<P> {
    host: OnlineHostConfig,
    prover: P,
    config: Sp1BackendConfig,
}

impl<P> Sp1Backend<P> {
    /// Creates a backend over the given RPC host config and SP1 prover.
    pub const fn new(host: OnlineHostConfig, prover: P, config: Sp1BackendConfig) -> Self {
        Self {
            host,
            prover,
            config,
        }
    }
}

impl<P> ProofJobBackend for Sp1Backend<P>
where
    P: WorldSuccinctProver + Send + Sync + 'static,
    P::Error: Into<anyhow::Error>,
{
    fn lane(&self) -> ProofBackend {
        ProofBackend::Sp1
    }

    fn start(&self, request: &ProofRequest) -> anyhow::Result<BackendUpdate> {
        let start_block = self.start_block(request)?;

        if self.prover.supports_async_requests() {
            if self.config.split_count != 1 {
                bail!(
                    "SP1 network durable workflow currently supports exactly one range, got {}",
                    self.config.split_count
                );
            }

            let range_request = self.range_request(request, start_block)?;
            let id = self
                .prover
                .request_range(range_request)
                .map_err(Into::into)
                .context("requesting SP1 range proof")?;
            return Ok(BackendUpdate::Pending {
                state: BackendProofState::Range {
                    id: BackendProofId(id),
                },
            });
        }

        let artifact = prove_validity(
            &self.host,
            &self.prover,
            ValidityProofRequest::new(
                start_block,
                request.l2_block_number,
                Some(request.l1_head),
                self.config.allow_unfinalized,
                self.config.split_count,
                self.config.prover_address,
            ),
        )?;

        check_artifact(request, &artifact)?;

        Ok(BackendUpdate::Complete(ProofData::Sp1 {
            proof: artifact.proof.into(),
            public_values: artifact.outputs.abi_encode().into(),
        }))
    }

    fn advance(
        &self,
        request: &ProofRequest,
        state: BackendProofState,
    ) -> anyhow::Result<BackendUpdate> {
        match state {
            BackendProofState::Range { id } => {
                let Some(range_artifact) = self.prover.poll_range(id.0).map_err(Into::into)? else {
                    return Ok(BackendUpdate::Noop);
                };
                let aggregation_request = self.aggregation_request(request, range_artifact)?;
                let id = self
                    .prover
                    .request_aggregation(aggregation_request)
                    .map_err(Into::into)
                    .context("requesting SP1 aggregation proof")?;
                Ok(BackendUpdate::Pending {
                    state: BackendProofState::Aggregation {
                        id: BackendProofId(id),
                    },
                })
            }
            BackendProofState::Aggregation { id } => {
                let Some(artifact) = self.prover.poll_aggregation(id.0).map_err(Into::into)? else {
                    return Ok(BackendUpdate::Noop);
                };
                check_artifact(request, &artifact)?;
                Ok(BackendUpdate::Complete(ProofData::Sp1 {
                    proof: artifact.proof.into(),
                    public_values: artifact.outputs.abi_encode().into(),
                }))
            }
            BackendProofState::Single { .. } => {
                bail!("SP1 backend does not support single-phase backend state")
            }
        }
    }
}

impl<P> Sp1Backend<P>
where
    P: WorldSuccinctProver,
    P::Error: Into<anyhow::Error>,
{
    fn start_block(&self, request: &ProofRequest) -> anyhow::Result<u64> {
        request
            .l2_block_number
            .checked_sub(self.config.block_interval)
            .with_context(|| {
                format!(
                    "l2 block number {} is below the block interval {}",
                    request.l2_block_number, self.config.block_interval
                )
            })
    }

    fn range_request(
        &self,
        request: &ProofRequest,
        start_block: u64,
    ) -> anyhow::Result<RangeProofRequest> {
        tracing::info!(
            start = start_block + 1,
            end = request.l2_block_number,
            "building range witness"
        );
        let input = build_range_input(
            &self.host,
            RangeWitnessRequest {
                start_block,
                end_block: request.l2_block_number,
                l1_head: Some(request.l1_head),
                allow_unfinalized: self.config.allow_unfinalized,
            },
        )?;

        RangeProofRequest::from_witness_data(&input.witness, None)
            .context("failed to serialize range witness")
    }

    fn aggregation_request(
        &self,
        request: &ProofRequest,
        range_artifact: RangeProofArtifact,
    ) -> anyhow::Result<AggregationProofRequest> {
        let client = Client::new();
        let l1_header = fetch_l1_header_by_hash(&client, &self.host.l1_rpc, request.l1_head)?;
        let l1_headers_cbor =
            serde_cbor::to_vec(&vec![l1_header]).context("CBOR-encoding L1 header failed")?;

        Ok(AggregationProofRequest {
            inputs: AggregationInputs {
                boot_infos: vec![range_artifact.boot_info],
                latest_l1_checkpoint_head: request.l1_head,
                multi_block_vkey: self.prover.multi_block_vkey(),
                prover_address: self.config.prover_address,
            },
            l1_headers_cbor,
            range_proofs: vec![range_artifact.proof],
        })
    }
}

/// A proof artifact whose committed outputs do not defend the requested root.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
enum ArtifactMismatch {
    #[error("aggregation post root {actual:?} does not match root claim {expected:?}")]
    PostRoot { expected: B256, actual: B256 },
    #[error("aggregation block number {actual} does not match request {expected}")]
    BlockNumber { expected: u64, actual: u64 },
    #[error("aggregation l1 head {actual:?} does not match request {expected:?}")]
    L1Head { expected: B256, actual: B256 },
}

/// Checks that the aggregation outputs defend exactly the requested root.
fn check_artifact(
    request: &ProofRequest,
    artifact: &AggregationProofArtifact,
) -> Result<(), ArtifactMismatch> {
    let outputs = &artifact.outputs;
    if outputs.l2PostRoot != request.root_claim {
        return Err(ArtifactMismatch::PostRoot {
            expected: request.root_claim,
            actual: outputs.l2PostRoot,
        });
    }
    if outputs.l2BlockNumber != request.l2_block_number {
        return Err(ArtifactMismatch::BlockNumber {
            expected: request.l2_block_number,
            actual: outputs.l2BlockNumber,
        });
    }
    if outputs.l1Head != request.l1_head {
        return Err(ArtifactMismatch::L1Head {
            expected: request.l1_head,
            actual: outputs.l1Head,
        });
    }
    Ok(())
}
