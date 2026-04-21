use reth_db::DatabaseError;
use reth_provider::ProviderError;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorldChainTransactionPoolInvalid {
    #[error("invalid external nullifier period")]
    InvalidExternalNullifierPeriod,
    #[error("invalid external nullifier nonce")]
    InvalidExternalNullifierNonce,
    #[error("invalid semaphore proof")]
    InvalidSemaphoreProof,
    #[error("duplicate tx hash")]
    DuplicateTxHash,
    #[error("invalid root")]
    InvalidRoot,
    #[error(transparent)]
    MalformedSignature(#[from] alloy_rlp::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum WorldChainTransactionPoolError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("invalid entrypoint - {0}")]
    Initialization(String),
}
