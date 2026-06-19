use crate::{
    error::{ProofJobQueueError, ProofRequestError},
    types::{
        BackendProofState, BackendUpdate, LeaseToken, LeasedBackendProofWork, LeasedProofRequest,
        ProofBackend, ProofRequest, ProofRequestId, ProofResponse, ProofStatus,
        ProofSubmissionLease,
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
    /// Returns `None` when no work is available. Returned jobs are leased:
    /// a job that is neither submitted nor failed before the lease expires
    /// is re-queued.
    async fn get_next_proof(
        &self,
        backend: ProofBackend,
    ) -> Result<Option<LeasedProofRequest>, ProofJobQueueError>;

    /// Persist durable backend work created while starting a proof job.
    async fn submit_backend_proof_state(
        &self,
        proof_id: ProofRequestId,
        backend_proof_state: BackendProofState,
        lease_token: LeaseToken,
    ) -> Result<(), ProofJobQueueError>;

    /// Lease durable backend work that is due for polling or advancement.
    async fn get_next_backend_proof(
        &self,
        backend: ProofBackend,
    ) -> Result<Option<LeasedBackendProofWork>, ProofJobQueueError>;

    /// Apply a non-final update produced while advancing a durable backend job.
    async fn complete_backend_proof_job(
        &self,
        backend_job_id: i64,
        lease_token: LeaseToken,
        next_update: BackendUpdate,
    ) -> Result<(), ProofJobQueueError>;

    /// Submit a final proof response to the `prover-service`.
    async fn submit_proof(
        &self,
        proof: ProofResponse,
        lease: ProofSubmissionLease,
    ) -> Result<(), ProofJobQueueError>;

    /// Report that proving failed for the given job.
    ///
    /// The job is re-queued until its attempts are exhausted, after which
    /// it is marked as permanently failed.
    async fn fail_proof(
        &self,
        proof_id: ProofRequestId,
        reason: String,
        lease_token: LeaseToken,
    ) -> Result<(), ProofJobQueueError>;
}
