//! Proving backend invoked by the worker for each leased job.

use alloy_primitives::Address;
use anyhow::Context;
use world_chain_proof_core::artifacts::AggregationProofArtifact;
use world_chain_proof_succinct_host_utils::{
    online::OnlineHostConfig,
    validity::{ValidityProofRequest, prove_validity},
};
use world_chain_proof_succinct_utils::WorldSuccinctProver;
use world_chain_prover_service::ProofRequest;

/// Turns one leased [`ProofRequest`] into an aggregated validity proof.
///
/// Synchronous and long-running (witness generation plus proving); the worker invokes it on a
/// blocking thread.
pub trait ValidityProofBackend: Send + Sync + 'static {
    /// Proves the transition the request defends and returns the aggregated artifact.
    fn prove(&self, request: &ProofRequest) -> anyhow::Result<AggregationProofArtifact>;
}

/// Configuration for [`Sp1Backend`].
#[derive(Clone, Debug)]
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

/// [`ValidityProofBackend`] that builds witnesses over RPC and proves them with a
/// [`WorldSuccinctProver`] (the sp1-sdk env prover in production).
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

impl<P> ValidityProofBackend for Sp1Backend<P>
where
    P: WorldSuccinctProver + Send + Sync + 'static,
    P::Error: Into<anyhow::Error>,
{
    fn prove(&self, request: &ProofRequest) -> anyhow::Result<AggregationProofArtifact> {
        let start_block = request
            .l2_block_number
            .checked_sub(self.config.block_interval)
            .with_context(|| {
                format!(
                    "l2 block number {} is below the block interval {}",
                    request.l2_block_number, self.config.block_interval
                )
            })?;

        prove_validity(
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
        )
    }
}
