use crate::{
    error::{
        BackendMismatchErrorData, BackendSessionAlreadyTerminalErrorData, ProofJobQueueError,
        ProofJobStatusErrorData, ProofMismatchErrorData, ProofRequestError,
        TooManyRetriesErrorData,
    },
    service::ProverService,
    traits::{ProofJobQueue, ProofRequester},
    types::{
        GetNextProofRequest, GetNextProofResponse, GetProofSessionRequest, GetProofSessionResponse,
        HeartbeatRequest, HeartbeatResponse, ProofRequest, ProofRequestId, ProofResponse,
        ProofStatus, RecordProofSessionRequest, RecordProofSessionResponse, SubmitProofRequest,
        SubmitProofResponse,
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
    /// No proof request with the given id is known.
    pub const NOT_FOUND: i32 = -32002;
    /// The proof request has been retried more than the configured maximum.
    pub const TOO_MANY_RETRIES: i32 = -32005;
    /// A conflicting row vanished after an insert conflict; the request is safe to retry.
    pub const RETRYABLE_CONFLICT: i32 = -32006;
    /// No proof job with the given proof request id is known.
    pub const PROOF_ID_NOT_FOUND: i32 = -32011;
    /// The proof job was not in the expected claimed status.
    pub const PROOF_JOB_STATUS_NOT_CLAIMED: i32 = -32012;
    /// A worker tried to update a row using an expired or superseded lock.
    pub const STALE_LOCK: i32 = -32014;
    /// A worker tried to update a row using an expired lock.
    pub const LOCK_EXPIRED: i32 = -32015;
    /// The submitted proof was produced by a different backend than the job expects.
    pub const BACKEND_MISMATCH: i32 = -32016;
    /// The proof job has already reached a terminal status.
    pub const ALREADY_TERMINAL: i32 = -32017;
    /// A replayed submit carried proof data that differs from the stored proof.
    pub const PROOF_MISMATCH: i32 = -32018;
    /// Temporary prover-service storage failure.
    pub const SQLX: i32 = -32019;
    /// A worker tried to record a backend session that already reached a
    /// conflicting terminal status.
    pub const BACKEND_SESSION_ALREADY_TERMINAL: i32 = -32020;
}

/// The `prover-service` JSON-RPC API, covering both the defender-facing
/// [`ProofRequester`] surface and the worker-facing [`ProofJobQueue`] surface.
#[rpc(server, client, namespace = "prover")]
pub trait ProverServiceApi {
    /// Queue a proof request, returning its deterministic id.
    ///
    /// The `prover-service` itself is backend-agnostic and does not validate `root_claim`
    /// against the real L2 chain here; backend-specific workers perform their own
    /// fast-fail sanity checks before starting expensive proof generation (for example,
    /// the Nitro worker rejects requests whose `root_claim` doesn't match the L2 rollup
    /// node's `optimism_outputAtBlock` result before dispatching to the enclave, see
    /// `world_chain_nitro_worker::NitroBackend`).
    #[method(name = "requestProof")]
    async fn request_proof(&self, proof_request: ProofRequest) -> RpcResult<ProofRequestId>;

    /// Get the current status of a proof request.
    #[method(name = "proofStatus")]
    async fn proof_status(&self, proof_id: ProofRequestId) -> RpcResult<ProofStatus>;

    /// Get a completed proof.
    #[method(name = "getProof")]
    async fn get_proof(&self, proof_id: ProofRequestId) -> RpcResult<ProofResponse>;

    /// Lock the next queued proof request for the given backend.
    #[method(name = "getNextProof")]
    async fn get_next_proof(&self, request: GetNextProofRequest)
    -> RpcResult<GetNextProofResponse>;

    /// Submit a generated proof.
    #[method(name = "submitProof")]
    async fn submit_proof(&self, request: SubmitProofRequest) -> RpcResult<SubmitProofResponse>;

    /// Get a proof session if any.
    #[method(name = "getProofSession")]
    async fn get_proof_session(
        &self,
        request: GetProofSessionRequest,
    ) -> RpcResult<GetProofSessionResponse>;

    /// Record a new proof session.
    #[method(name = "recordProofSession")]
    async fn record_proof_session(
        &self,
        request: RecordProofSessionRequest,
    ) -> RpcResult<RecordProofSessionResponse>;

    /// Heartbeat call.
    #[method(name = "heartbeat")]
    async fn heartbeat(&self, request: HeartbeatRequest) -> RpcResult<HeartbeatResponse>;
}

impl From<ProofRequestError> for ErrorObjectOwned {
    fn from(err: ProofRequestError) -> Self {
        let message = err.to_string();
        match err {
            ProofRequestError::ProofIdNotFound(proof_id) => {
                ErrorObject::owned(error_code::NOT_FOUND, message, Some(proof_id))
            }
            ProofRequestError::RowMissingAfterConflict(proof_id) => {
                ErrorObject::owned(error_code::RETRYABLE_CONFLICT, message, Some(proof_id))
            }
            ProofRequestError::TooManyRetries(data) => {
                ErrorObject::owned(error_code::TOO_MANY_RETRIES, message, Some(data))
            }
            ProofRequestError::Sqlx(_) => ErrorObject::owned(error_code::SQLX, message, None::<()>),
            ProofRequestError::UnknownProofStatus(_)
            | ProofRequestError::ProofEncoding(_)
            | ProofRequestError::BlockNumberExceedsI64(_)
            | ProofRequestError::RemoteInternal
            | ProofRequestError::RemoteSqlx
            | ProofRequestError::RpcRequestTimeout
            | ProofRequestError::RpcTransport(_)
            | ProofRequestError::RpcRestartNeeded(_)
            | ProofRequestError::RpcServiceDisconnected
            | ProofRequestError::RpcClient(_) => {
                ErrorObject::owned(INTERNAL_ERROR_CODE, message, None::<()>)
            }
        }
    }
}

impl From<ProofJobQueueError> for ErrorObjectOwned {
    fn from(err: ProofJobQueueError) -> Self {
        let message = err.to_string();
        match err {
            ProofJobQueueError::ProofIdNotFound(proof_id) => {
                ErrorObject::owned(error_code::PROOF_ID_NOT_FOUND, message, Some(proof_id))
            }
            ProofJobQueueError::ProofJobStatusNotClaimed(data) => ErrorObject::owned(
                error_code::PROOF_JOB_STATUS_NOT_CLAIMED,
                message,
                Some(data),
            ),
            ProofJobQueueError::StaleLock(proof_id) => {
                ErrorObject::owned(error_code::STALE_LOCK, message, Some(proof_id))
            }
            ProofJobQueueError::LockExpired(proof_id) => {
                ErrorObject::owned(error_code::LOCK_EXPIRED, message, Some(proof_id))
            }
            ProofJobQueueError::BackendMismatch(data) => {
                ErrorObject::owned(error_code::BACKEND_MISMATCH, message, Some(data))
            }
            ProofJobQueueError::AlreadyTerminal(proof_id) => {
                ErrorObject::owned(error_code::ALREADY_TERMINAL, message, Some(proof_id))
            }
            ProofJobQueueError::BackendSessionAlreadyTerminal(data) => ErrorObject::owned(
                error_code::BACKEND_SESSION_ALREADY_TERMINAL,
                message,
                Some(data),
            ),
            ProofJobQueueError::ProofMismatch(data) => {
                ErrorObject::owned(error_code::PROOF_MISMATCH, message, Some(data))
            }
            ProofJobQueueError::Sqlx(_) => {
                ErrorObject::owned(error_code::SQLX, message, None::<()>)
            }
            ProofJobQueueError::NegativeBlockNumber(_)
            | ProofJobQueueError::UnknownProofBackend(_)
            | ProofJobQueueError::UnknownBackendSessionStatus(_)
            | ProofJobQueueError::UnknownProofJobStatus(_)
            | ProofJobQueueError::MalformedAddress(_)
            | ProofJobQueueError::MalformedB256(_)
            | ProofJobQueueError::ProofEncoding(_)
            | ProofJobQueueError::Unknown(_)
            | ProofJobQueueError::RemoteInternal
            | ProofJobQueueError::RemoteSqlx
            | ProofJobQueueError::RpcRequestTimeout
            | ProofJobQueueError::RpcTransport(_)
            | ProofJobQueueError::RpcRestartNeeded(_)
            | ProofJobQueueError::RpcServiceDisconnected
            | ProofJobQueueError::RpcClient(_) => {
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

    async fn get_next_proof(
        &self,
        request: GetNextProofRequest,
    ) -> RpcResult<GetNextProofResponse> {
        Ok(self.service.get_next_proof(request).await?)
    }

    async fn submit_proof(&self, request: SubmitProofRequest) -> RpcResult<SubmitProofResponse> {
        Ok(self.service.submit_proof(request).await?)
    }

    async fn get_proof_session(
        &self,
        request: GetProofSessionRequest,
    ) -> RpcResult<GetProofSessionResponse> {
        Ok(self.service.get_proof_session(request).await?)
    }

    async fn record_proof_session(
        &self,
        request: RecordProofSessionRequest,
    ) -> RpcResult<RecordProofSessionResponse> {
        Ok(self.service.record_proof_session(request).await?)
    }

    async fn heartbeat(&self, request: HeartbeatRequest) -> RpcResult<HeartbeatResponse> {
        Ok(self.service.heartbeat(request).await?)
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

fn map_request_error(err: ClientError, id: ProofRequestId) -> ProofRequestError {
    match err {
        ClientError::RequestTimeout => ProofRequestError::RpcRequestTimeout,
        ClientError::Transport(err) => ProofRequestError::RpcTransport(err),
        ClientError::RestartNeeded(err) => ProofRequestError::RpcRestartNeeded(err),
        ClientError::ServiceDisconnect => ProofRequestError::RpcServiceDisconnected,
        ClientError::Call(err) => map_request_call_error(err, id),
        err => ProofRequestError::RpcClient(err),
    }
}

fn map_request_call_error(err: ErrorObjectOwned, fallback_id: ProofRequestId) -> ProofRequestError {
    match err.code() {
        error_code::NOT_FOUND => {
            ProofRequestError::ProofIdNotFound(error_data(&err).unwrap_or(fallback_id))
        }
        error_code::TOO_MANY_RETRIES => {
            if let Some(data) = error_data::<TooManyRetriesErrorData>(&err) {
                ProofRequestError::TooManyRetries(data)
            } else {
                ProofRequestError::RemoteInternal
            }
        }
        error_code::RETRYABLE_CONFLICT => {
            ProofRequestError::RowMissingAfterConflict(error_data(&err).unwrap_or(fallback_id))
        }
        error_code::SQLX => ProofRequestError::RemoteSqlx,
        INTERNAL_ERROR_CODE => ProofRequestError::RemoteInternal,
        _ => ProofRequestError::RpcClient(ClientError::Call(err)),
    }
}

fn map_job_error(err: ClientError, id: ProofRequestId) -> ProofJobQueueError {
    match err {
        ClientError::RequestTimeout => ProofJobQueueError::RpcRequestTimeout,
        ClientError::Transport(err) => ProofJobQueueError::RpcTransport(err),
        ClientError::RestartNeeded(err) => ProofJobQueueError::RpcRestartNeeded(err),
        ClientError::ServiceDisconnect => ProofJobQueueError::RpcServiceDisconnected,
        ClientError::Call(err) => map_job_call_error(err, id),
        err => ProofJobQueueError::RpcClient(err),
    }
}

fn map_job_call_error(err: ErrorObjectOwned, fallback_id: ProofRequestId) -> ProofJobQueueError {
    match err.code() {
        error_code::PROOF_ID_NOT_FOUND => {
            ProofJobQueueError::ProofIdNotFound(error_data(&err).unwrap_or(fallback_id))
        }
        error_code::PROOF_JOB_STATUS_NOT_CLAIMED => {
            if let Some(data) = error_data::<ProofJobStatusErrorData>(&err) {
                ProofJobQueueError::ProofJobStatusNotClaimed(data)
            } else {
                ProofJobQueueError::RemoteInternal
            }
        }
        error_code::STALE_LOCK => {
            ProofJobQueueError::StaleLock(error_data(&err).unwrap_or(fallback_id))
        }
        error_code::LOCK_EXPIRED => {
            ProofJobQueueError::LockExpired(error_data(&err).unwrap_or(fallback_id))
        }
        error_code::BACKEND_MISMATCH => {
            if let Some(data) = error_data::<BackendMismatchErrorData>(&err) {
                ProofJobQueueError::BackendMismatch(data)
            } else {
                ProofJobQueueError::RemoteInternal
            }
        }
        error_code::ALREADY_TERMINAL => {
            ProofJobQueueError::AlreadyTerminal(error_data(&err).unwrap_or(fallback_id))
        }
        error_code::BACKEND_SESSION_ALREADY_TERMINAL => {
            if let Some(data) = error_data::<BackendSessionAlreadyTerminalErrorData>(&err) {
                ProofJobQueueError::BackendSessionAlreadyTerminal(data)
            } else {
                ProofJobQueueError::RemoteInternal
            }
        }
        error_code::PROOF_MISMATCH => {
            if let Some(data) = error_data::<Box<ProofMismatchErrorData>>(&err) {
                ProofJobQueueError::ProofMismatch(data)
            } else {
                ProofJobQueueError::RemoteInternal
            }
        }
        error_code::SQLX => ProofJobQueueError::RemoteSqlx,
        INTERNAL_ERROR_CODE => ProofJobQueueError::RemoteInternal,
        _ => ProofJobQueueError::RpcClient(ClientError::Call(err)),
    }
}

#[async_trait]
impl ProofRequester for RpcProverServiceClient {
    async fn request_proof(
        &self,
        proof_request: ProofRequest,
    ) -> Result<ProofRequestId, ProofRequestError> {
        let id = proof_request.id();
        ProverServiceApiClient::request_proof(&self.client, proof_request)
            .await
            .map_err(|err| map_request_error(err, id))
    }

    async fn proof_status(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofStatus, ProofRequestError> {
        ProverServiceApiClient::proof_status(&self.client, proof_id)
            .await
            .map_err(|err| map_request_error(err, proof_id))
    }

    async fn get_proof(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofResponse, ProofRequestError> {
        ProverServiceApiClient::get_proof(&self.client, proof_id)
            .await
            .map_err(|err| map_request_error(err, proof_id))
    }
}

#[async_trait]
impl ProofJobQueue for RpcProverServiceClient {
    async fn get_next_proof(
        &self,
        request: GetNextProofRequest,
    ) -> Result<GetNextProofResponse, ProofJobQueueError> {
        ProverServiceApiClient::get_next_proof(&self.client, request)
            .await
            .map_err(|err| map_job_error(err, ProofRequestId(Default::default())))
    }

    async fn submit_proof(
        &self,
        request: SubmitProofRequest,
    ) -> Result<SubmitProofResponse, ProofJobQueueError> {
        let id = request.proof.id;
        ProverServiceApiClient::submit_proof(&self.client, request)
            .await
            .map_err(|err| map_job_error(err, id))
    }

    async fn get_proof_session(
        &self,
        request: GetProofSessionRequest,
    ) -> Result<GetProofSessionResponse, ProofJobQueueError> {
        let proof_id = request.proof_id;
        ProverServiceApiClient::get_proof_session(&self.client, request)
            .await
            .map_err(|err| map_job_error(err, proof_id))
    }

    async fn record_proof_session(
        &self,
        request: RecordProofSessionRequest,
    ) -> Result<RecordProofSessionResponse, ProofJobQueueError> {
        let proof_id = request.proof_id;
        ProverServiceApiClient::record_proof_session(&self.client, request)
            .await
            .map_err(|err| map_job_error(err, proof_id))
    }

    async fn heartbeat(
        &self,
        request: HeartbeatRequest,
    ) -> Result<HeartbeatResponse, ProofJobQueueError> {
        let proof_id = request.proof_id;
        ProverServiceApiClient::heartbeat(&self.client, request)
            .await
            .map_err(|err| map_job_error(err, proof_id))
    }
}
