use crate::{
    error::{ProofJobQueueError, ProofRequestError},
    types::{
        BackendSession, BackendSessionStatus, LockId, LockedProofRequest, ProofBackend,
        ProofRequest, ProofRequestId, ProofResponse, ProofStatus, SessionType,
        SucceededProofResponse,
    },
};
use async_trait::async_trait;

/// It contains all methods needed for a defender to request a proof
/// to the `prover-service`.
#[async_trait]
pub trait ProofRequester {
    /// Send a proof request to the `prover-service`.
    ///
    /// Requests are deduplicated by their deterministic id: re-requesting
    /// a proof that is already queued, in progress, or completed is a no-op
    /// returning the same id, while re-requesting a failed proof re-queues it.
    async fn request_proof(
        &self,
        proof_request: ProofRequest,
    ) -> Result<ProofRequestId, ProofRequestError>;

    /// Get the current status of a proof request.
    async fn proof_status(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofStatus, ProofRequestError>;

    /// Get the actual proof from the `prover-service`.
    async fn get_proof(&self, proof_id: ProofRequestId)
    -> Result<ProofResponse, ProofRequestError>;
}

/// It contains all methods needed for a prover worker to get
/// new proof requests and submits proof responses.
#[async_trait]
pub trait ProofJobQueue {
    /// Look for a new proof request to start on the given backend.
    ///
    /// Returns `None` when no work is available. Returned jobs are locked:
    /// a job that is neither submitted nor failed before the lock expires
    /// is re-queued.
    async fn get_next_proof(
        &self,
        backend: ProofBackend,
        worker_id: String,
    ) -> Result<Option<LockedProofRequest>, ProofJobQueueError>;

    /// Submit a final proof response to the `prover-service`.
    async fn submit_proof(
        &self,
        proof: SucceededProofResponse,
        worker_id: String,
        lock: LockId,
    ) -> Result<(), ProofJobQueueError>;

    /// Get a backend session that matches the provided proof_id and
    /// session_type if it exists.
    async fn get_proof_session(
        &self,
        proof_id: ProofRequestId,
        session_type: SessionType,
    ) -> Result<Option<BackendSession>, ProofJobQueueError>;

    /// Record a backend session tied to the provided proof_id
    /// and session_type.
    async fn record_proof_session(
        &self,
        proof_id: ProofRequestId,
        session_type: SessionType,
        worker_id: String,
        lock_id: LockId,
        backend_session_id: String,
        state: BackendSessionStatus,
    ) -> Result<(), ProofJobQueueError>;

    /// Ping the `prover-service` to signal that a proof worker tied
    /// to the provided `worker_id` and `lock` is still working on
    /// the provided `proof_id` job.
    async fn heartbeat(
        &self,
        proof_id: ProofRequestId,
        worker_id: String,
        lock: LockId,
    ) -> Result<(), ProofJobQueueError>;
}
