//! The backend abstraction the worker dispatches leased jobs to.

use world_chain_prover_service::{ProofBackend, ProofData, ProofRequest};

/// A proving backend for one [`ProofBackend`] lane.
///
/// The backend turns a leased [`ProofRequest`] into the lane-shaped [`ProofData`] the worker
/// submits verbatim. It is responsible for validating that the proof actually defends the
/// requested root — how a proof binds to a root is lane-specific (a validity proof commits to
/// it in its public values, an attestation signs over it) — and returning an error otherwise.
///
/// `prove` is synchronous and long-running (witness generation plus proving); the worker runs
/// it on a blocking thread.
pub trait ProofJobBackend: Send + Sync + 'static {
    /// The queue lane this backend leases jobs from.
    fn lane(&self) -> ProofBackend;

    /// Proves the transition the request defends and returns the lane-shaped proof.
    fn prove(&self, request: &ProofRequest) -> anyhow::Result<ProofData>;
}
