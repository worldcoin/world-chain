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

pub trait ClaimedProofJobHandler: Send + Sync + 'static {
    /// Error returned while handling a claimed proof job.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Returns whether this worker should attempt to claim a job now.
    async fn ready_to_claim(&self, _worker_id: &str) -> bool {
        true
    }

    /// Handles a claimed proof job.
    async fn handle_claimed_job(&self, job: ProofJob) -> Result<(), Self::Error>;

    /// Signals backend-specific spawned work to stop during shutdown.
    fn shutdown(&self) {}
}
