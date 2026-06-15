//! SP1 validity-proof backend for the defender's [`ProofWorker`].

use alloy_primitives::{Address, B256};
use alloy_sol_types::SolValue;
use anyhow::Context;
use world_chain_proof_core::artifacts::AggregationProofArtifact;
use world_chain_proof_kona_utils::online::OnlineHostConfig;
use world_chain_proof_succinct_host_utils::validity::{ValidityProofRequest, prove_validity};
use world_chain_proof_succinct_utils::WorldSuccinctProver;
use world_chain_proof_worker::ProofJobBackend;
use world_chain_prover_service::{ProofBackend, ProofData, ProofRequest};

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

    fn prove(&self, request: &ProofRequest) -> anyhow::Result<ProofData> {
        let start_block = request
            .l2_block_number
            .checked_sub(self.config.block_interval)
            .with_context(|| {
                format!(
                    "l2 block number {} is below the block interval {}",
                    request.l2_block_number, self.config.block_interval
                )
            })?;

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

        Ok(ProofData::Sp1 {
            proof: artifact.proof.into(),
            public_values: artifact.outputs.abi_encode().into(),
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
