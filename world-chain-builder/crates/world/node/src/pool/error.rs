use reth::transaction_pool::error::{InvalidPoolTransactionError, PoolTransactionError};
use reth::transaction_pool::{PoolTransaction, TransactionValidationOutcome};
use reth_db::{DatabaseError, DatabaseWriteOperation};
use reth_provider::ProviderError;

use crate::pbh::external_nullifier::ExternalNullifierParsingError;

#[derive(Debug, thiserror::Error)]
pub enum WorldChainTransactionPoolInvalid {
    #[error("nullifier has already been seen")]
    NullifierAlreadyExists,
    #[error("invalid external nullifier - {0}")]
    InvalidExternalNullifier(ExternalNullifierParsingError),
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
    #[error("duplicate tx hash")]
    DuplicateTxHash,
    #[error("invalid root")]
    InvalidRoot,
}

#[derive(Debug, thiserror::Error)]
pub enum WorldChainTransactionPoolError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error(transparent)]
    RootProvider(#[from] ProviderError),
}

impl PoolTransactionError for WorldChainTransactionPoolInvalid {
    fn is_bad_transaction(&self) -> bool {
        true
    }
}

impl From<WorldChainTransactionPoolInvalid> for Box<dyn PoolTransactionError> {
    fn from(e: WorldChainTransactionPoolInvalid) -> Self {
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

impl From<WorldChainTransactionPoolInvalid> for TransactionValidationError {
    fn from(e: WorldChainTransactionPoolInvalid) -> Self {
        TransactionValidationError::Invalid(InvalidPoolTransactionError::Other(e.into()))
    }
}

impl From<WorldChainTransactionPoolError> for TransactionValidationError {
    fn from(e: WorldChainTransactionPoolError) -> Self {
        TransactionValidationError::Error(Box::new(e))
    }
}

impl From<DatabaseError> for TransactionValidationError {
    fn from(e: DatabaseError) -> Self {
        match e {
            DatabaseError::Write(write) => {
                if let DatabaseWriteOperation::CursorInsert = write.operation {
                    WorldChainTransactionPoolInvalid::DuplicateTxHash.into()
                } else {
                    WorldChainTransactionPoolError::Database(DatabaseError::Write(write)).into()
                }
            }
            e => WorldChainTransactionPoolError::Database(e).into(),
        }
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
