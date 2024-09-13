use reth_transaction_pool::error::PoolTransactionError;

#[derive(Debug, thiserror::Error)]
pub enum WcTransactionPoolError {
    #[error("nullifier has already been seen")]
    NullifierAlreadyExists,
}

impl PoolTransactionError for WcTransactionPoolError {
    fn is_bad_transaction(&self) -> bool {
        true
    }
}

impl From<WcTransactionPoolError> for Box<dyn PoolTransactionError> {
    fn from(e: WcTransactionPoolError) -> Self {
        Box::new(e)
    }
}
