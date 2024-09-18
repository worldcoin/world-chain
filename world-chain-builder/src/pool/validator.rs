//! World Chain transaction pool types
use chrono::Datelike;
use reth_db::cursor::DbCursorRW;
use reth_db::transaction::{DbTx, DbTxMut};
use semaphore::hash_to_field;
use semaphore::protocol::verify_proof;
use std::str::FromStr as _;
use std::sync::Arc;

use reth_db::{Database, DatabaseEnv, DatabaseError, DatabaseWriteOperation};
use reth_node_optimism::txpool::OpTransactionValidator;
use reth_primitives::{SealedBlock, TxHash};
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use reth_transaction_pool::{
    EthTransactionValidator, Pool, TransactionOrigin, TransactionValidationOutcome,
    TransactionValidationTaskExecutor, TransactionValidator,
};

use crate::pbh::db::{ExecutedPbhNullifierTable, ValidatedPbhTransactionTable};
use crate::pbh::semaphore::SemaphoreProof;
use crate::pbh::tx::Prefix;

use super::error::{
    TransactionValidationError, WorldChainTransactionPoolError, WorldChainTransactionPoolInvalid,
};
use super::ordering::WorldChainOrdering;
use super::tx::{WorldChainPoolTransaction, WorldChainPooledTransaction};

/// Type alias for World Chain transaction pool
pub type WorldChainTransactionPool<Client, S> = Pool<
    TransactionValidationTaskExecutor<
        WorldChainTransactionValidator<Client, WorldChainPooledTransaction>,
    >,
    // TODO: Modify this ordering
    WorldChainOrdering<WorldChainPooledTransaction>,
    S,
>;

/// Validator for World Chain transactions.
#[derive(Debug, Clone)]
pub struct WorldChainTransactionValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt,
{
    inner: OpTransactionValidator<Client, Tx>,
    database_env: Arc<DatabaseEnv>,
    _tmp_workaround: EthTransactionValidator<Client, Tx>,
    num_pbh_txs: u16,
}

impl<Client, Tx> WorldChainTransactionValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt,
    Tx: WorldChainPoolTransaction,
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

    fn set_validated(
        &self,
        tx: &Tx,
        semaphore_proof: &SemaphoreProof,
    ) -> Result<(), DatabaseError> {
        let db_tx = self.database_env.tx_mut()?;
        let mut cursor = db_tx.cursor_write::<ValidatedPbhTransactionTable>()?;
        cursor.insert(
            *tx.hash(),
            semaphore_proof.nullifier_hash.to_be_bytes().into(),
        )?;
        db_tx.commit()?;
        Ok(())
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
            return Err(WorldChainTransactionPoolInvalid::InvalidExternalNullifier.into());
        }

        // TODO: Figure out what we actually want to do with the prefix
        // For now, we just check that it's a valid prefix
        // Maybe in future use as some sort of versioning?
        if Prefix::from_str(split[0]).is_err() {
            return Err(WorldChainTransactionPoolInvalid::InvalidExternalNullifierPrefix.into());
        }

        // TODO: Handle edge case where we are at the end of the month
        if split[1] != current_period_id() {
            return Err(WorldChainTransactionPoolInvalid::InvalidExternalNullifierPeriod.into());
        }

        match split[2].parse::<u16>() {
            Ok(nonce) if nonce < self.num_pbh_txs => {}
            _ => {
                return Err(WorldChainTransactionPoolInvalid::InvalidExternalNullifierNonce.into());
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
                return Err(WorldChainTransactionPoolInvalid::NullifierAlreadyExists.into());
            }
            Ok(None) => return Ok(()),
            Err(e) => {
                return Err(TransactionValidationError::Error(
                    format!("Error while fetching nullifier from database: {}", e).into(),
                ));
            }
        }
    }

    pub fn validate_nullifier_hash(
        &self,
        semaphore_proof: &SemaphoreProof,
    ) -> Result<(), TransactionValidationError> {
        let expected = hash_to_field(semaphore_proof.external_nullifier.as_bytes());
        if semaphore_proof.nullifier_hash != expected {
            return Err(WorldChainTransactionPoolInvalid::InvalidNullifierHash.into());
        }
        Ok(())
    }

    pub fn validate_signal_hash(
        &self,
        tx_hash: &TxHash,
        semaphore_proof: &SemaphoreProof,
    ) -> Result<(), TransactionValidationError> {
        // TODO: we probably don't need to hash the hash.
        let expected = hash_to_field(tx_hash.as_slice());
        if semaphore_proof.signal_hash != expected {
            return Err(WorldChainTransactionPoolInvalid::InvalidSignalHash.into());
        }
        Ok(())
    }

    pub fn validate_semaphore_proof(
        &self,
        transaction: &Tx,
        semaphore_proof: &SemaphoreProof,
    ) -> Result<(), TransactionValidationError> {
        self.validate_external_nullifier(semaphore_proof)?;
        self.validate_nullifier(semaphore_proof)?;
        self.validate_nullifier_hash(semaphore_proof)?;
        self.validate_signal_hash(transaction.hash(), semaphore_proof)?;

        // TODO: Think about DOS mitigation.
        let res = verify_proof(
            semaphore_proof.root,
            semaphore_proof.nullifier_hash,
            semaphore_proof.signal_hash,
            semaphore_proof.external_nullifier_hash,
            &semaphore_proof.proof.0,
            30,
        );

        match res {
            Ok(true) => Ok(()),
            Ok(false) => Err(WorldChainTransactionPoolInvalid::InvalidSemaphoreProof.into()),
            Err(e) => Err(TransactionValidationError::Error(e.into())),
        }
    }

    pub fn validate_one(
        &self,
        origin: TransactionOrigin,
        transaction: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        if let Some(semaphore_proof) = transaction.semaphore_proof() {
            if let Err(e) = self.validate_semaphore_proof(&transaction, semaphore_proof) {
                return e.to_outcome(transaction);
            }
            match self.set_validated(&transaction, semaphore_proof) {
                Ok(_) => {}
                Err(DatabaseError::Write(write)) => {
                    if let DatabaseWriteOperation::CursorInsert = write.operation {
                        return Into::<TransactionValidationError>::into(
                            WorldChainTransactionPoolInvalid::DuplicateTxHash,
                        )
                        .to_outcome(transaction);
                    } else {
                        return Into::<TransactionValidationError>::into(
                            WorldChainTransactionPoolError::Database(DatabaseError::Write(write)),
                        )
                        .to_outcome(transaction);
                    }
                }
                Err(e) => {
                    return Into::<TransactionValidationError>::into(
                        WorldChainTransactionPoolError::Database(e),
                    )
                    .to_outcome(transaction);
                }
            }
        }
        self.inner.validate_one(origin, transaction)
    }

    /// Validates all given transactions.
    ///
    /// Returns all outcomes for the given transactions in the same order.
    ///
    /// See also [`Self::validate_one`]
    pub fn validate_all(
        &self,
        transactions: Vec<(TransactionOrigin, Tx)>,
    ) -> Vec<TransactionValidationOutcome<Tx>> {
        transactions
            .into_iter()
            .map(|(origin, tx)| self.validate_one(origin, tx))
            .collect()
    }
}

impl<Client, Tx> TransactionValidator for WorldChainTransactionValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt,
    Tx: WorldChainPoolTransaction,
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
        self.validate_all(transactions)
    }

    fn on_new_head_block(&self, new_tip_block: &SealedBlock) {
        self.inner.on_new_head_block(new_tip_block)
    }
}

fn current_period_id() -> String {
    let current_date = chrono::Utc::now();
    format!("{:0>2}{}", current_date.month(), current_date.year())
}
