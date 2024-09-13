use reth_transaction_pool::error::{InvalidPoolTransactionError, PoolTransactionError};
use reth_transaction_pool::{PoolTransaction, TransactionValidationOutcome};

#[derive(Debug, thiserror::Error)]
pub enum WcTransactionPoolInvalid {
    #[error("nullifier has already been seen")]
    NullifierAlreadyExists,
    #[error("invalid external nullifier")]
    InvalidExternalNullifier,
    #[error("invalid external nullifier prefix")]
    InvalidExternalNullifierPrefix,
    #[error("invalid external nullifier period")]
    InvalidExternalNullifierPeriod,
    #[error("invalid external nullifier nonce")]
    InvalidExternalNullifierNonce,
    #[error("invalid nullifier hash")]
    InvalidNullifierHash,
    #[error("invalid signal hash")]
    InvalidSignalHash,
    #[error("invalid semaphore proof")]
    InvalidSemaphoreProof,
}

impl PoolTransactionError for WcTransactionPoolInvalid {
    fn is_bad_transaction(&self) -> bool {
        true
    }
}

impl From<WcTransactionPoolInvalid> for Box<dyn PoolTransactionError> {
    fn from(e: WcTransactionPoolInvalid) -> Self {
        Box::new(e)
    }
}

/// A Result type returned after checking a transaction's validity.
#[derive(Debug)]
pub enum TransactionValidationError {
    /// The transaction is considered invalid indefinitely: It violates constraints that prevent
    /// this transaction from ever becoming valid.
    Invalid(InvalidPoolTransactionError),
    /// An error occurred while trying to validate the transaction
    Error(Box<dyn std::error::Error + Send + Sync>),
}

impl From<WcTransactionPoolInvalid> for TransactionValidationError {
    fn from(e: WcTransactionPoolInvalid) -> Self {
        TransactionValidationError::Invalid(InvalidPoolTransactionError::Other(e.into()))
    }
}

impl TransactionValidationError {
    pub fn to_outcome<T: PoolTransaction>(self, tx: T) -> TransactionValidationOutcome<T> {
        match self {
            TransactionValidationError::Invalid(e) => TransactionValidationOutcome::Invalid(tx, e),
            TransactionValidationError::Error(e) => {
                TransactionValidationOutcome::Error(*tx.hash(), e)
            }
        }
    }
}
