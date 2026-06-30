use crate::types::{ProofBackend, ProofRequestId, ProofStatus};
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
    /// The queue for the requested backend is at capacity.
    #[error("proof queue for backend {0} is full")]
    QueueFull(ProofBackend),
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
    /// No proof request with the given id is known.
    #[error("unknown proof job {0}")]
    UnknownJob(ProofRequestId),
    /// No backend proof job with the given id is known.
    #[error("unknown backend proof job {0}")]
    UnknownBackendJob(i64),
    /// The lock token no longer owns the row being updated.
    #[error("stale lock for proof job update")]
    StaleLocked,
    /// The submitted proof does not match the requested job.
    #[error("invalid proof for job {id}: {reason}")]
    InvalidProof {
        /// The proof request id.
        id: ProofRequestId,
        /// Why the proof was rejected.
        reason: String,
    },
    /// Internal service or storage error.
    #[error("prover-service error: {0}")]
    Internal(String),
    /// RPC transport error.
    #[error("RPC error: {0}")]
    Rpc(String),
    /// No proof request with the given id is known.
    #[error("proof request {0} not found")]
    NotFound(ProofRequestId),
    #[error("validation error: {0}")]
    Validation(String),
}
