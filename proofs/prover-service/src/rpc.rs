use crate::{
    error::{ProofJobQueueError, ProofRequestError},
    service::ProverService,
    traits::{ProofJobQueue, ProofRequester},
    types::{
        BackendProofState, BackendUpdate, LeaseToken, LeasedBackendProofWork, LeasedProofRequest,
        ProofBackend, ProofRequest, ProofRequestId, ProofResponse, ProofStatus,
        ProofSubmissionLease,
    },
};
use jsonrpsee::{
    core::{RpcResult, async_trait, client::Error as ClientError},
    http_client::{HttpClient, HttpClientBuilder},
    proc_macros::rpc,
    server::{Server, ServerHandle},
    types::{ErrorObject, ErrorObjectOwned, error::INTERNAL_ERROR_CODE},
};
use std::{net::SocketAddr, sync::Arc};
use tracing::info;

/// JSON-RPC error codes returned by the `prover-service`.
pub mod error_code {
    /// The queue for the requested backend is at capacity.
    pub const QUEUE_FULL: i32 = -32001;
    /// No proof request with the given id is known.
    pub const NOT_FOUND: i32 = -32002;
    /// The proof is not ready yet; the error data holds the [`crate::ProofStatus`].
    pub const PENDING: i32 = -32003;
    /// The proof request permanently failed; the error data holds the reason.
    pub const FAILED: i32 = -32004;
    /// No proof job with the given id is known.
    pub const UNKNOWN_JOB: i32 = -32011;
    /// No backend proof job with the given id is known.
    pub const UNKNOWN_BACKEND_JOB: i32 = -32013;
    /// A worker tried to update a row using an expired or superseded lease.
    pub const STALE_LEASE: i32 = -32014;
    /// The submitted proof does not match the requested job;
    /// the error data holds the proof id and reason.
    pub const INVALID_PROOF: i32 = -32012;
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct InvalidProofErrorData {
    id: ProofRequestId,
    reason: String,
}

/// The `prover-service` JSON-RPC API, covering both the defender-facing
/// [`ProofRequester`] surface and the worker-facing [`ProofJobQueue`] surface.
#[rpc(server, client, namespace = "prover")]
pub trait ProverServiceApi {
    /// Queue a proof request, returning its deterministic id.
    #[method(name = "requestProof")]
    async fn request_proof(&self, proof_request: ProofRequest) -> RpcResult<ProofRequestId>;

    /// Get the current status of a proof request.
    #[method(name = "proofStatus")]
    async fn proof_status(&self, proof_id: ProofRequestId) -> RpcResult<ProofStatus>;

    /// Get a completed proof.
    #[method(name = "getProof")]
    async fn get_proof(&self, proof_id: ProofRequestId) -> RpcResult<ProofResponse>;

    /// Lease the next queued proof request for the given backend.
    #[method(name = "getNextProof")]
    async fn get_next_proof(&self, backend: ProofBackend) -> RpcResult<Option<LeasedProofRequest>>;

    /// Persist durable backend work created while starting a proof job.
    #[method(name = "submitBackendProofState")]
    async fn submit_backend_proof_state(
        &self,
        proof_id: ProofRequestId,
        backend_proof_state: BackendProofState,
        lease_token: LeaseToken,
    ) -> RpcResult<()>;

    /// Lease the next durable backend proof job for the given backend.
    #[method(name = "getNextBackendProof")]
    async fn get_next_backend_proof(
        &self,
        backend: ProofBackend,
    ) -> RpcResult<Option<LeasedBackendProofWork>>;

    /// Apply a durable backend proof update.
    #[method(name = "completeBackendProofJob")]
    async fn complete_backend_proof_job(
        &self,
        backend_job_id: i64,
        lease_token: LeaseToken,
        next_update: BackendUpdate,
    ) -> RpcResult<()>;

    /// Report that advancing a durable backend proof job failed for this attempt.
    #[method(name = "failBackendProofJob")]
    async fn fail_backend_proof_job(
        &self,
        backend_job_id: i64,
        reason: String,
        lease_token: LeaseToken,
    ) -> RpcResult<()>;

    /// Submit a generated proof.
    #[method(name = "submitProof")]
    async fn submit_proof(
        &self,
        proof: ProofResponse,
        lease: ProofSubmissionLease,
    ) -> RpcResult<()>;

    /// Report that proving failed for the given job.
    #[method(name = "failProof")]
    async fn fail_proof(
        &self,
        proof_id: ProofRequestId,
        reason: String,
        lease_token: LeaseToken,
    ) -> RpcResult<()>;
}

impl From<ProofRequestError> for ErrorObjectOwned {
    fn from(err: ProofRequestError) -> Self {
        let message = err.to_string();
        match err {
            ProofRequestError::QueueFull(_) => {
                ErrorObject::owned(error_code::QUEUE_FULL, message, None::<()>)
            }
            ProofRequestError::NotFound(_) => {
                ErrorObject::owned(error_code::NOT_FOUND, message, None::<()>)
            }
            ProofRequestError::Pending { status, .. } => {
                ErrorObject::owned(error_code::PENDING, message, Some(status))
            }
            ProofRequestError::Failed { reason, .. } => {
                ErrorObject::owned(error_code::FAILED, message, Some(reason))
            }
            ProofRequestError::Internal(_) => {
                ErrorObject::owned(INTERNAL_ERROR_CODE, message, None::<()>)
            }
            ProofRequestError::Rpc(_) => {
                ErrorObject::owned(INTERNAL_ERROR_CODE, message, None::<()>)
            }
        }
    }
}

impl From<ProofJobQueueError> for ErrorObjectOwned {
    fn from(err: ProofJobQueueError) -> Self {
        let message = err.to_string();
        match err {
            ProofJobQueueError::UnknownJob(_) => {
                ErrorObject::owned(error_code::UNKNOWN_JOB, message, None::<()>)
            }
            ProofJobQueueError::UnknownBackendJob(_) => {
                ErrorObject::owned(error_code::UNKNOWN_BACKEND_JOB, message, None::<()>)
            }
            ProofJobQueueError::StaleLease => {
                ErrorObject::owned(error_code::STALE_LEASE, message, None::<()>)
            }
            ProofJobQueueError::InvalidProof { id, reason } => ErrorObject::owned(
                error_code::INVALID_PROOF,
                message,
                Some(InvalidProofErrorData { id, reason }),
            ),
            ProofJobQueueError::Internal(_) => {
                ErrorObject::owned(INTERNAL_ERROR_CODE, message, None::<()>)
            }
            ProofJobQueueError::Rpc(_) => {
                ErrorObject::owned(INTERNAL_ERROR_CODE, message, None::<()>)
            }
        }
    }
}

/// JSON-RPC server implementation backed by a [`ProverService`].
#[derive(Debug)]
pub struct ProverServiceRpc {
    service: Arc<ProverService>,
}

impl ProverServiceRpc {
    /// Create a new RPC handler wrapping the given service.
    pub const fn new(service: Arc<ProverService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ProverServiceApiServer for ProverServiceRpc {
    async fn request_proof(&self, proof_request: ProofRequest) -> RpcResult<ProofRequestId> {
        Ok(self.service.request_proof(proof_request).await?)
    }

    async fn proof_status(&self, proof_id: ProofRequestId) -> RpcResult<ProofStatus> {
        Ok(self.service.proof_status(proof_id).await?)
    }

    async fn get_proof(&self, proof_id: ProofRequestId) -> RpcResult<ProofResponse> {
        Ok(self.service.get_proof(proof_id).await?)
    }

    async fn get_next_proof(&self, backend: ProofBackend) -> RpcResult<Option<LeasedProofRequest>> {
        Ok(self.service.get_next_proof(backend).await?)
    }

    async fn submit_backend_proof_state(
        &self,
        proof_id: ProofRequestId,
        backend_proof_state: BackendProofState,
        lease_token: LeaseToken,
    ) -> RpcResult<()> {
        Ok(self
            .service
            .submit_backend_proof_state(proof_id, backend_proof_state, lease_token)
            .await?)
    }

    async fn get_next_backend_proof(
        &self,
        backend: ProofBackend,
    ) -> RpcResult<Option<LeasedBackendProofWork>> {
        Ok(self.service.get_next_backend_proof(backend).await?)
    }

    async fn complete_backend_proof_job(
        &self,
        backend_job_id: i64,
        lease_token: LeaseToken,
        next_update: BackendUpdate,
    ) -> RpcResult<()> {
        Ok(self
            .service
            .complete_backend_proof_job(backend_job_id, lease_token, next_update)
            .await?)
    }

    async fn fail_backend_proof_job(
        &self,
        backend_job_id: i64,
        reason: String,
        lease_token: LeaseToken,
    ) -> RpcResult<()> {
        Ok(self
            .service
            .fail_backend_proof_job(backend_job_id, reason, lease_token)
            .await?)
    }

    async fn submit_proof(
        &self,
        proof: ProofResponse,
        lease: ProofSubmissionLease,
    ) -> RpcResult<()> {
        Ok(self.service.submit_proof(proof, lease).await?)
    }

    async fn fail_proof(
        &self,
        proof_id: ProofRequestId,
        reason: String,
        lease_token: LeaseToken,
    ) -> RpcResult<()> {
        Ok(self
            .service
            .fail_proof(proof_id, reason, lease_token)
            .await?)
    }
}

/// Start the `prover-service` JSON-RPC server on `addr`.
///
/// Returns the bound address (useful when `addr` uses port 0) and the
/// server handle; the server runs until the handle is stopped or dropped.
pub async fn start_rpc_server(
    addr: SocketAddr,
    service: Arc<ProverService>,
) -> std::io::Result<(SocketAddr, ServerHandle)> {
    let server = Server::builder().build(addr).await?;
    let local_addr = server.local_addr()?;
    let handle = server.start(ProverServiceRpc::new(service).into_rpc());
    info!(%local_addr, "prover-service RPC server started");
    Ok((local_addr, handle))
}

/// JSON-RPC client for the `prover-service`.
///
/// Implements both [`ProofRequester`] (for defenders) and [`ProofJobQueue`]
/// (for workers) on top of the [`ProverServiceApiClient`], so callers can
/// depend on the traits without knowing about the transport.
#[derive(Debug, Clone)]
pub struct RpcProverServiceClient {
    client: HttpClient,
}

impl RpcProverServiceClient {
    /// Connect to a `prover-service` RPC server at `url`.
    pub fn new(url: impl AsRef<str>) -> Result<Self, ClientError> {
        let client = HttpClientBuilder::default().build(url)?;
        Ok(Self { client })
    }
}

/// Extract the typed error `data` payload from a JSON-RPC error object.
fn error_data<T: serde::de::DeserializeOwned>(err: &ErrorObjectOwned) -> Option<T> {
    err.data()
        .and_then(|raw| serde_json::from_str(raw.get()).ok())
}

fn invalid_proof_reason(err: &ErrorObjectOwned) -> Option<String> {
    error_data::<InvalidProofErrorData>(err)
        .map(|data| data.reason)
        .or_else(|| error_data(err))
}

fn map_request_error(
    err: ClientError,
    id: ProofRequestId,
    backend: Option<ProofBackend>,
) -> ProofRequestError {
    let ClientError::Call(err) = err else {
        return ProofRequestError::Rpc(err.to_string());
    };
    match (err.code(), backend) {
        (error_code::QUEUE_FULL, Some(backend)) => ProofRequestError::QueueFull(backend),
        (error_code::NOT_FOUND, _) => ProofRequestError::NotFound(id),
        (error_code::PENDING, _) => ProofRequestError::Pending {
            id,
            status: error_data(&err).unwrap_or(ProofStatus::Queued),
        },
        (error_code::FAILED, _) => ProofRequestError::Failed {
            id,
            reason: error_data(&err).unwrap_or_else(|| err.message().to_string()),
        },
        _ => ProofRequestError::Rpc(format!("{} (code {})", err.message(), err.code())),
    }
}

fn map_job_error(err: ClientError, id: ProofRequestId) -> ProofJobQueueError {
    let ClientError::Call(err) = err else {
        return ProofJobQueueError::Rpc(err.to_string());
    };
    match err.code() {
        error_code::UNKNOWN_JOB => ProofJobQueueError::UnknownJob(id),
        error_code::STALE_LEASE => ProofJobQueueError::StaleLease,
        error_code::INVALID_PROOF => ProofJobQueueError::InvalidProof {
            id,
            reason: invalid_proof_reason(&err).unwrap_or_else(|| err.message().to_string()),
        },
        _ => ProofJobQueueError::Rpc(format!("{} (code {})", err.message(), err.code())),
    }
}

fn map_backend_job_error(err: ClientError, backend_job_id: i64) -> ProofJobQueueError {
    let ClientError::Call(err) = err else {
        return ProofJobQueueError::Rpc(err.to_string());
    };
    match err.code() {
        error_code::UNKNOWN_BACKEND_JOB => ProofJobQueueError::UnknownBackendJob(backend_job_id),
        error_code::STALE_LEASE => ProofJobQueueError::StaleLease,
        error_code::INVALID_PROOF => {
            if let Some(data) = error_data::<InvalidProofErrorData>(&err) {
                ProofJobQueueError::InvalidProof {
                    id: data.id,
                    reason: data.reason,
                }
            } else {
                ProofJobQueueError::Rpc(format!("{} (code {})", err.message(), err.code()))
            }
        }
        _ => ProofJobQueueError::Rpc(format!("{} (code {})", err.message(), err.code())),
    }
}

#[async_trait]
impl ProofRequester for RpcProverServiceClient {
    async fn request_proof(
        &self,
        proof_request: ProofRequest,
    ) -> Result<ProofRequestId, ProofRequestError> {
        let id = proof_request.id();
        let backend = proof_request.backend;
        ProverServiceApiClient::request_proof(&self.client, proof_request)
            .await
            .map_err(|err| map_request_error(err, id, Some(backend)))
    }

    async fn proof_status(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofStatus, ProofRequestError> {
        ProverServiceApiClient::proof_status(&self.client, proof_id)
            .await
            .map_err(|err| map_request_error(err, proof_id, None))
    }

    async fn get_proof(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofResponse, ProofRequestError> {
        ProverServiceApiClient::get_proof(&self.client, proof_id)
            .await
            .map_err(|err| map_request_error(err, proof_id, None))
    }
}

#[async_trait]
impl ProofJobQueue for RpcProverServiceClient {
    async fn get_next_proof(
        &self,
        backend: ProofBackend,
    ) -> Result<Option<LeasedProofRequest>, ProofJobQueueError> {
        ProverServiceApiClient::get_next_proof(&self.client, backend)
            .await
            .map_err(|err| ProofJobQueueError::Rpc(err.to_string()))
    }

    async fn submit_backend_proof_state(
        &self,
        proof_id: ProofRequestId,
        backend_proof_state: BackendProofState,
        lease_token: LeaseToken,
    ) -> Result<(), ProofJobQueueError> {
        ProverServiceApiClient::submit_backend_proof_state(
            &self.client,
            proof_id,
            backend_proof_state,
            lease_token,
        )
        .await
        .map_err(|err| map_job_error(err, proof_id))
    }

    async fn get_next_backend_proof(
        &self,
        backend: ProofBackend,
    ) -> Result<Option<LeasedBackendProofWork>, ProofJobQueueError> {
        ProverServiceApiClient::get_next_backend_proof(&self.client, backend)
            .await
            .map_err(|err| ProofJobQueueError::Rpc(err.to_string()))
    }

    async fn complete_backend_proof_job(
        &self,
        backend_job_id: i64,
        lease_token: LeaseToken,
        next_update: BackendUpdate,
    ) -> Result<(), ProofJobQueueError> {
        ProverServiceApiClient::complete_backend_proof_job(
            &self.client,
            backend_job_id,
            lease_token,
            next_update,
        )
        .await
        .map_err(|err| map_backend_job_error(err, backend_job_id))
    }

    async fn fail_backend_proof_job(
        &self,
        backend_job_id: i64,
        reason: String,
        lease_token: LeaseToken,
    ) -> Result<(), ProofJobQueueError> {
        ProverServiceApiClient::fail_backend_proof_job(
            &self.client,
            backend_job_id,
            reason,
            lease_token,
        )
        .await
        .map_err(|err| map_backend_job_error(err, backend_job_id))
    }

    async fn submit_proof(
        &self,
        proof: ProofResponse,
        lease: ProofSubmissionLease,
    ) -> Result<(), ProofJobQueueError> {
        let id = proof.id;
        ProverServiceApiClient::submit_proof(&self.client, proof, lease)
            .await
            .map_err(|err| map_job_error(err, id))
    }

    async fn fail_proof(
        &self,
        proof_id: ProofRequestId,
        reason: String,
        lease_token: LeaseToken,
    ) -> Result<(), ProofJobQueueError> {
        ProverServiceApiClient::fail_proof(&self.client, proof_id, reason, lease_token)
            .await
            .map_err(|err| map_job_error(err, proof_id))
    }
}
