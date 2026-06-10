use crate::{
    error::{ProofJobQueueError, ProofRequestError},
    types::{ProofBackend, ProofRequest, ProofRequestId, ProofResponse, ProofStatus},
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
    /// Look for a new proof request to process on the given backend.
    ///
    /// Returns `None` when no work is available. Returned jobs are leased:
    /// a job that is neither submitted nor failed before the lease expires
    /// is re-queued.
    async fn get_next_proof(
        &self,
        backend: ProofBackend,
    ) -> Result<Option<ProofRequest>, ProofJobQueueError>;

    /// Submit a proof response to the `prover-service`.
    async fn submit_proof(&self, proof: ProofResponse) -> Result<(), ProofJobQueueError>;

    /// Report that proving failed for the given job.
    ///
    /// The job is re-queued until its attempts are exhausted, after which
    /// it is marked as permanently failed.
    async fn fail_proof(
        &self,
        proof_id: ProofRequestId,
        reason: String,
    ) -> Result<(), ProofJobQueueError>;
}
