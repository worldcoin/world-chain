use crate::{
    error::{ProofJobQueueError, ProofRequestError},
    types::{ProofRequest, ProofRequestId, ProofResponse},
};
use async_trait::async_trait;

/// It contains all methods needed for a defender to request a proof
/// to the `prover-service`.
#[async_trait]
pub trait ProofRequester {
    /// Send a proof request to the `prover-service`.
    async fn request_proof(&self, proof_request: ProofRequest) -> Result<(), ProofRequestError>;

    /// Get the actual proof from the `prover-service`.
    async fn get_proof(&self, proof_id: ProofRequestId)
    -> Result<ProofResponse, ProofRequestError>;
}

/// It contains all methods needed for a prover worker to get
/// new proof requests and submits proof responses.
#[async_trait]
pub trait ProofJobQueue {
    /// Look for a new proof requests to process.
    async fn get_next_proof(&self) -> Result<ProofRequest, ProofJobQueueError>;

    /// Submit a proof response to the `prover-service`.
    async fn submit_proof(&self, proof: ProofResponse) -> Result<(), ProofJobQueueError>;
}
