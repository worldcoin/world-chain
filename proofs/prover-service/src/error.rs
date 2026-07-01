use crate::{
    ProofBackend, ProofJobStatus,
    types::{ProofRequestId, ProofStatus},
};
use sqlx::migrate::MigrateError;
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
    /// No proof request with the given id is known.
    #[error("proof request {0} not found")]
    NotFound(ProofRequestId),
    /// Conflicting row disappeared after insert conflict. Safe to retry.
    #[error("proof_id {id}: proof request row missing after insert conflict; retry request_proof")]
    RowMissingAfterConflict {
        /// Proof request id that was expected to exist.
        id: ProofRequestId,
    },
    #[error("The proof request {0} has been retried too many times")]
    TooManyRetries(ProofRequestId),
    /// The proof is not ready yet.
    #[error("proof request {id} is not ready yet (status: {status})")]
    Pending {
        /// The proof request id.
        id: ProofRequestId,
        /// The current status of the request.
        status: ProofStatus,
    },
    /// The proof request permanently failed.
    #[error("proof request {id} failed: {reason}")]
    Failed {
        /// The proof request id.
        id: ProofRequestId,
        /// The reason reported on the last attempt.
        reason: String,
    },
    /// Internal service or storage error.
    #[error("prover-service error: {0}")]
    Internal(String),
    /// RPC transport error.
    #[error("RPC error: {0}")]
    Rpc(String),
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
    #[error("Invalid proof job status for proof id {proof_id}: expected {expected}, got {actual}.")]
    ProofJobStatusNotClaimed {
        proof_id: ProofRequestId,
        expected: ProofJobStatus,
        actual: ProofJobStatus,
    },
    #[error("Stale lock for given proof id: {0}.")]
    StaleLock(ProofRequestId),
    #[error("Expired lock for given proof id: {0}.")]
    LockExpired(ProofRequestId),
    #[error("proof job {proof_id} backend mismatch: expected {expected}, got {actual}.")]
    BackendMismatch {
        proof_id: ProofRequestId,
        expected: ProofBackend,
        actual: ProofBackend,
    },
    #[error("The proof {0} has already reached a terminal job status.")]
    AlreadyTerminal(ProofRequestId),
    #[error(transparent)]
    ProofEncoding(#[from] serde_json::Error),
}
