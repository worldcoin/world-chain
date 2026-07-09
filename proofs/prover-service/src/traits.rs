use crate::{
    error::{ProofJobQueueError, ProofRequestError},
    types::{
        GetNextProofRequest, GetNextProofResponse, GetProofSessionRequest, GetProofSessionResponse,
        HeartbeatRequest, HeartbeatResponse, ProofRequest, ProofRequestId, ProofResponse,
        ProofStatus, RecordProofSessionRequest, RecordProofSessionResponse, SubmitProofRequest,
        SubmitProofResponse,
    },
};
use async_trait::async_trait;
use auto_impl::auto_impl;

/// It contains all methods needed for a defender to request a proof
/// to the `prover-service`.
#[async_trait]
#[auto_impl(Arc)]
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
#[auto_impl(Arc)]
pub trait ProofJobQueue {
    /// Look for a new proof request to start on the given backend.
    ///
    /// Returns `None` when no work is available. Returned jobs are locked:
    /// a job that is neither submitted nor failed before the lock expires
    /// is re-queued.
    async fn get_next_proof(
        &self,
        request: GetNextProofRequest,
    ) -> Result<GetNextProofResponse, ProofJobQueueError>;

    /// Submit a final proof response to the `prover-service`.
    async fn submit_proof(
        &self,
        request: SubmitProofRequest,
    ) -> Result<SubmitProofResponse, ProofJobQueueError>;

    /// Get a backend session that matches the provided proof_id and
    /// session_type if it exists.
    async fn get_proof_session(
        &self,
        request: GetProofSessionRequest,
    ) -> Result<GetProofSessionResponse, ProofJobQueueError>;

    /// Record a backend session tied to the provided proof_id
    /// and session_type.
    async fn record_proof_session(
        &self,
        request: RecordProofSessionRequest,
    ) -> Result<RecordProofSessionResponse, ProofJobQueueError>;

    /// Ping the `prover-service` to signal that a proof worker tied
    /// to the provided `worker_id` and `lock_id` is still working on
    /// the provided `proof_id` job.
    async fn heartbeat(
        &self,
        request: HeartbeatRequest,
    ) -> Result<HeartbeatResponse, ProofJobQueueError>;
}
