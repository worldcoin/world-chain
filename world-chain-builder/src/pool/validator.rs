//! World Chain transaction pool types
use chrono::Datelike;
use reth_db::transaction::DbTx;
use reth_transaction_pool::error::InvalidPoolTransactionError;
use semaphore::hash_to_field;
use std::io::Read;
use std::str::FromStr as _;
use std::sync::Arc;

use reth_db::{Database, DatabaseEnv};
use reth_node_optimism::txpool::OpTransactionValidator;
use reth_primitives::{SealedBlock, TxHash};
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use reth_transaction_pool::{
    CoinbaseTipOrdering, EthTransactionValidator, Pool, TransactionOrigin,
    TransactionValidationOutcome, TransactionValidationTaskExecutor, TransactionValidator,
};

use crate::pbh::db::ExecutedPbhNullifierTable;
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
    _tmp_workaround: EthTransactionValidator<Client, Tx>,
    num_pbh_txs: u16,
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
        num_pbh_txs: u16,
    ) -> Self {
        Self {
            inner,
            database_env,
            _tmp_workaround: tmp_workaround,
            num_pbh_txs,
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
        // For now, we just check that it's a valid prefix
        // Maybe in future use as some sort of versioning?
        if Prefix::from_str(split[0]).is_err() {
            return Err(TransactionValidationError::Invalid(
                InvalidPoolTransactionError::Other(
                    WcTransactionPoolError::InvalidExternalNullifierPrefix.into(),
                ),
            ));
        }

        // TODO: Handle edge case where we are at the end of the month
        if split[1] != current_period_id() {
            return Err(TransactionValidationError::Invalid(
                InvalidPoolTransactionError::Other(
                    WcTransactionPoolError::InvalidExternalNullifierPeriod.into(),
                ),
            ));
        }

        match split[2].parse::<u16>() {
            Ok(nonce) if nonce < self.num_pbh_txs => {}
            _ => {
                return Err(TransactionValidationError::Invalid(
                    InvalidPoolTransactionError::Other(
                        WcTransactionPoolError::InvalidExternalNullifierNonce.into(),
                    ),
                ));
            }
        }

        Ok(())
    }

    pub fn validate_nullifier(
        &self,
        semaphore_proof: &SemaphoreProof,
    ) -> Result<(), TransactionValidationError> {
        let tx = self.database_env.tx().unwrap();
        match tx
            .get::<ExecutedPbhNullifierTable>(semaphore_proof.nullifier_hash.to_be_bytes().into())
        {
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
        Ok(())
    }

    pub fn validate_nullifier_hash(
        &self,
        semaphore_proof: &SemaphoreProof,
    ) -> Result<(), TransactionValidationError> {
        let expected = hash_to_field(semaphore_proof.external_nullifier.as_bytes());
        if semaphore_proof.nullifier_hash != expected {
            return Err(TransactionValidationError::Invalid(
                InvalidPoolTransactionError::Other(
                    WcTransactionPoolError::InvalidNullifierHash.into(),
                ),
            ));
        }
        Ok(())
    }

    pub fn validate_signal_hash(
        &self,
        semaphore_proof: &SemaphoreProof,
        tx_hash: &TxHash,
    ) -> Result<(), TransactionValidationError> {
        // TODO: we probably don't need to hash the hash.
        let expected = hash_to_field(tx_hash.as_slice());
        if semaphore_proof.signal_hash != expected {
            return Err(TransactionValidationError::Invalid(
                InvalidPoolTransactionError::Other(
                    WcTransactionPoolError::InvalidSignalHash.into(),
                ),
            ));
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
            if let Err(e) = self.validate_nullifier(semaphore_proof) {
                return e.to_outcome(transaction);
            }
            if let Err(e) = self.validate_nullifier_hash(semaphore_proof) {
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
