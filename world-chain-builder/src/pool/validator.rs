//! World Chain transaction pool types
use chrono::Datelike;
use reth_db::transaction::DbTx;
use reth_transaction_pool::error::InvalidPoolTransactionError;
use std::str::FromStr as _;
use std::sync::Arc;

use reth_db::{Database, DatabaseEnv};
use reth_node_optimism::txpool::OpTransactionValidator;
use reth_primitives::{SealedBlock, TxHash};
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use reth_transaction_pool::{
    CoinbaseTipOrdering, EthTransactionValidator, Pool, PoolTransaction, TransactionOrigin,
    TransactionValidationOutcome, TransactionValidationTaskExecutor, TransactionValidator,
};

use crate::pbh::db::NullifierTable;
use crate::pbh::semaphore::SemaphoreProof;
use crate::pbh::tx::Prefix;

use super::error::{TransactionValidationError, WcTransactionPoolError};
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

    /// External nullifiers must be of the form
    /// `<prefix>-<periodId>-<PbhNonce>`.
    /// example:
    /// `0-012025-11`
    pub fn validate_external_nullifier(
        &self,
        semaphore_proof: &SemaphoreProof,
    ) -> Result<(), TransactionValidationError> {
        let split = semaphore_proof
            .external_nullifier
            .split('-')
            .collect::<Vec<&str>>();

        if split.len() != 3 {
            return Err(TransactionValidationError::Invalid(
                InvalidPoolTransactionError::Other(
                    WcTransactionPoolError::InvalidExternalNullifier.into(),
                ),
            ));
        }

        // TODO: Figure out what we actually want to do with the prefix
        if Prefix::from_str(split[0]).is_err() {
            return Err(TransactionValidationError::Invalid(
                InvalidPoolTransactionError::Other(
                    WcTransactionPoolError::InvalidExternalNullifierPrefix.into(),
                ),
            ));
        }

        if split[1] != current_period_id() {
            return Err(TransactionValidationError::Invalid(
                InvalidPoolTransactionError::Other(
                    WcTransactionPoolError::InvalidExternalNullifierPeriod.into(),
                ),
            ));
        }

        Ok(())
    }

    pub fn validate_nullifier(&self, transaction: &Tx) -> Result<(), TransactionValidationError> {
        if let Some(proof) = transaction.semaphore_proof() {
            let tx = self.database_env.tx().unwrap();
            match tx.get::<NullifierTable>(proof.nullifier_hash.to_be_bytes().into()) {
                Ok(Some(_)) => {
                    return Err(TransactionValidationError::Invalid(
                        InvalidPoolTransactionError::Other(
                            WcTransactionPoolError::NullifierAlreadyExists.into(),
                        ),
                    ));
                }
                Ok(None) => {}
                Err(e) => {
                    return Err(TransactionValidationError::Error(
                        format!("Error while fetching nullifier from database: {}", e).into(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn validate_one(
        &self,
        origin: TransactionOrigin,
        transaction: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        if let Some(semaphore_proof) = transaction.semaphore_proof() {
            if let Err(e) = self.validate_external_nullifier(semaphore_proof) {
                return e.to_outcome(transaction);
            }
            if let Err(e) = self.validate_nullifier(&transaction) {
                return e.to_outcome(transaction);
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

fn current_period_id() -> String {
    let current_date = chrono::Utc::now();
    format!("{:0>2}{}", current_date.month(), current_date.year())
}
