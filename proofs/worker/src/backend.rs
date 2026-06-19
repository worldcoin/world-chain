//! The backend abstraction the worker dispatches leased jobs to.

use world_chain_prover_service::{BackendProofState, BackendUpdate, ProofBackend, ProofRequest};

/// A proving backend for one [`ProofBackend`] lane.
///
/// The backend starts or advances lane-specific proving work and returns a generic
/// [`BackendUpdate`] that the worker submits verbatim.
///
/// `start` and `advance` are synchronous and may be long-running; the worker runs them on a
/// blocking thread. Backends are responsible for validating that final proofs actually defend the
/// requested root.
pub trait ProofJobBackend: Send + Sync + 'static {
    /// The queue lane this backend leases jobs from.
    fn lane(&self) -> ProofBackend;

    /// Starts backend work for a user-facing proof request.
    fn start(&self, request: &ProofRequest) -> anyhow::Result<BackendUpdate>;

    /// Advances durable backend work.
    fn advance(
        &self,
        request: &ProofRequest,
        state: BackendProofState,
    ) -> anyhow::Result<BackendUpdate>;
}
