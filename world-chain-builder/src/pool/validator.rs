//! World Chain transaction pool types
use reth_db::transaction::DbTx;
use reth_transaction_pool::error::InvalidPoolTransactionError;
use std::sync::Arc;

use reth_db::{Database, DatabaseEnv};
use reth_node_optimism::txpool::OpTransactionValidator;
use reth_primitives::SealedBlock;
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use reth_transaction_pool::{
    CoinbaseTipOrdering, EthTransactionValidator, Pool, PoolTransaction, TransactionOrigin,
    TransactionValidationOutcome, TransactionValidationTaskExecutor, TransactionValidator,
};

use crate::pbh::db::NullifierTable;

use super::error::WcTransactionPoolError;
use super::tx::{WcPoolTransaction, WcPooledTransaction};

/// Type alias for World Chain transaction pool
pub type WorldChainTransactionPool<Client, S> = Pool<
    TransactionValidationTaskExecutor<WcTransactionValidator<Client, WcPooledTransaction>>,
    CoinbaseTipOrdering<WcPooledTransaction>,
    S,
>;

/// Validator for World Chain transactions.
#[derive(Debug, Clone)]
pub struct WcTransactionValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt,
{
    inner: OpTransactionValidator<Client, Tx>,
    database_env: Arc<DatabaseEnv>,
    tmp_workaround: EthTransactionValidator<Client, Tx>,
}

impl<Client, Tx> WcTransactionValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt,
    Tx: WcPoolTransaction,
{
    /// Create a new [`WorldChainTransactionValidator`].
    pub fn new(
        inner: OpTransactionValidator<Client, Tx>,
        database_env: Arc<DatabaseEnv>,
        tmp_workaround: EthTransactionValidator<Client, Tx>,
    ) -> Self {
        Self {
            inner,
            database_env,
            tmp_workaround,
        }
    }

    pub fn validate_one(
        &self,
        origin: TransactionOrigin,
        transaction: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        if let Some(proof) = transaction.semaphore_proof() {
            let tx = self.database_env.tx().unwrap();
            match tx.get::<NullifierTable>(proof.nullifier_hash.to_be_bytes().into()) {
                Ok(Some(_)) => {
                    return TransactionValidationOutcome::Invalid(
                        transaction,
                        InvalidPoolTransactionError::Other(
                            WcTransactionPoolError::NullifierAlreadyExists.into(),
                        ),
                    );
                }
                Ok(None) => {}
                Err(e) => {
                    return TransactionValidationOutcome::Error(
                        *transaction.hash(),
                        format!("Error while fetching nullifier from database: {}", e).into(),
                    );
                }
            }
        }

        self.inner.validate_one(origin, transaction)
    }
}

impl<Client, Tx> TransactionValidator for WcTransactionValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt,
    Tx: WcPoolTransaction,
{
    type Transaction = Tx;

    async fn validate_transaction(
        &self,
        origin: TransactionOrigin,
        transaction: Self::Transaction,
    ) -> TransactionValidationOutcome<Self::Transaction> {
        self.validate_one(origin, transaction)
    }

    async fn validate_transactions(
        &self,
        transactions: Vec<(TransactionOrigin, Self::Transaction)>,
    ) -> Vec<TransactionValidationOutcome<Self::Transaction>> {
        // TODO: do we want multithreading here?
        transactions
            .into_iter()
            .map(|(origin, tx)| self.validate_one(origin, tx))
            .collect()
    }

    fn on_new_head_block(&self, new_tip_block: &SealedBlock) {
        self.inner.on_new_head_block(new_tip_block)
    }
}
