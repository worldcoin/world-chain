use crate::{ProofBackend, ProofData, ProofJobStatus, types::ProofRequestId};
use alloy_primitives::BlockNumber;
use jsonrpsee::core::client::Error as JsonRpseeError;
use sqlx::migrate::MigrateError;
use std::sync::Arc;
use thiserror::Error;

/// Invalid `prover-service` configuration.
#[derive(Error, Debug)]
#[error("invalid prover-service config: {0}")]
pub struct InvalidConfigError(pub &'static str);

/// Error raised while initializing a Postgres-backed `ProverService`.
#[derive(Debug, Error)]
pub enum ProverServiceInitError {
    /// Invalid service configuration.
    #[error(transparent)]
    InvalidConfig(#[from] InvalidConfigError),
    /// Failed to connect to Postgres.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// Failed to run migrations.
    #[error(transparent)]
    Migration(#[from] MigrateError),
}

/// Error returned to a defender by the `prover-service`.
#[derive(Error, Debug)]
pub enum ProofRequestError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error("proof id {0}: proof request row missing after insert conflict. Retry request_proof.")]
    RowMissingAfterConflict(ProofRequestId),
    #[error("{0}")]
    UnknownProofStatus(String),
    #[error(
        "This proof request {proof_id} has been retried more than max_retries = {max_retries}."
    )]
    TooManyRetries {
        proof_id: ProofRequestId,
        max_retries: u32,
    },
    #[error("Proof request {0} not found.")]
    ProofIdNotFound(ProofRequestId),
    #[error(transparent)]
    ProofEncoding(#[from] serde_json::Error),
    #[error("The block number {0} exceeds i64 max value.")]
    BlockNumberExceedsI64(BlockNumber),
}

/// Data describing a proof job status mismatch.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ProofJobStatusErrorData {
    /// The proof request id.
    pub proof_id: ProofRequestId,
    /// The expected job status.
    pub expected: ProofJobStatus,
    /// The actual stored job status.
    pub actual: ProofJobStatus,
}

impl std::fmt::Display for ProofJobStatusErrorData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Invalid proof job status for proof id {}: expected {}, got {}.",
            self.proof_id, self.expected, self.actual
        )
    }
}

/// Data describing a submitted proof backend mismatch.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct BackendMismatchErrorData {
    /// The proof request id.
    pub proof_id: ProofRequestId,
    /// The backend expected by the stored proof request.
    pub expected: ProofBackend,
    /// The backend that produced the submitted proof.
    pub actual: ProofBackend,
}

impl std::fmt::Display for BackendMismatchErrorData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "proof job {} backend mismatch: expected {}, got {}.",
            self.proof_id, self.expected, self.actual
        )
    }
}

/// Data describing a repeated proof submission with different proof data.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ProofMismatchErrorData {
    /// The proof request id.
    pub proof_id: ProofRequestId,
    /// The proof data already stored by the prover-service.
    pub expected: ProofData,
    /// The proof data submitted by the worker.
    pub actual: ProofData,
}

impl std::fmt::Display for ProofMismatchErrorData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "proof {} proof data mismatch: expected {:?}, got {:?}.",
            self.proof_id, self.expected, self.actual
        )
    }
}

/// Error returned to a prover worker by the `prover-service`.
#[derive(Error, Debug)]
pub enum ProofJobQueueError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error("block number is negative: {0}.")]
    NegativeBlockNumber(i64),
    #[error("{0}")]
    UnknownProofBackend(String),
    #[error("{0}")]
    UnknownBackendSessionStatus(String),
    #[error("{0}")]
    UnknownProofJobStatus(String),
    #[error("The provided value has {0} bytes, expected 20.")]
    MalformedAddress(usize),
    #[error("The provided value has {0} bytes, expected 32.")]
    MalformedB256(usize),
    #[error("Proof request {0} not found.")]
    ProofIdNotFound(ProofRequestId),
    #[error("{0}")]
    ProofJobStatusNotClaimed(ProofJobStatusErrorData),
    #[error("Stale lock for given proof id: {0}.")]
    StaleLock(ProofRequestId),
    #[error("Expired lock for given proof id: {0}.")]
    LockExpired(ProofRequestId),
    #[error("{0}")]
    BackendMismatch(BackendMismatchErrorData),
    #[error("The proof {0} has already reached a terminal job status.")]
    AlreadyTerminal(ProofRequestId),
    #[error(transparent)]
    ProofEncoding(#[from] serde_json::Error),
    #[error("{0}")]
    ProofMismatch(Box<ProofMismatchErrorData>),
    #[error(
        "proof {0}: the diagnostic read did not identify a stable reason for the proof submission failure."
    )]
    Unknown(ProofRequestId),
    #[error("remote prover-service internal error.")]
    RemoteInternal,
    #[error("remote prover-service storage error.")]
    RemoteSqlx,
    #[error("RPC request timed out.")]
    RpcRequestTimeout,
    #[error("RPC transport error: {0}.")]
    RpcTransport(#[from] jsonrpsee::core::BoxError),
    #[error("RPC client restart needed: {0}.")]
    RpcRestartNeeded(#[source] Arc<JsonRpseeError>),
    #[error("RPC service disconnected.")]
    RpcServiceDisconnected,
    #[error("RPC client error: {0}.")]
    RpcClient(#[source] JsonRpseeError),
}

impl ProofJobQueueError {
    /// Whether retrying the same operation can plausibly succeed.
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Sqlx(_)
                | Self::RemoteSqlx
                | Self::RpcRequestTimeout
                | Self::RpcTransport(_)
                | Self::RpcRestartNeeded(_)
                | Self::RpcServiceDisconnected
        )
    }
}
