//! World Chain transaction pool types
use std::sync::Arc;

use reth::transaction_pool::{
    Pool, TransactionOrigin, TransactionValidationOutcome, TransactionValidationTaskExecutor,
    TransactionValidator,
};
use reth_db::cursor::DbCursorRW;
use reth_db::transaction::{DbTx, DbTxMut};
use reth_db::{Database, DatabaseEnv, DatabaseError};
use reth_optimism_node::txpool::OpTransactionValidator;
use reth_primitives::{SealedBlock, TransactionSigned};
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use semaphore::hash_to_field;
use semaphore::protocol::verify_proof;

use super::error::{TransactionValidationError, WorldChainTransactionPoolInvalid};
use super::ordering::WorldChainOrdering;
use super::root::WorldChainRootValidator;
use super::tx::{WorldChainPoolTransaction, WorldChainPooledTransaction};
use crate::pbh::date_marker::DateMarker;
use crate::pbh::db::{EmptyValue, ValidatedPbhTransaction};
use crate::pbh::external_nullifier::ExternalNullifier;
use crate::pbh::payload::{PbhPayload, TREE_DEPTH};

/// Type alias for World Chain transaction pool
pub type WorldChainTransactionPool<Client, S> = Pool<
    TransactionValidationTaskExecutor<
        WorldChainTransactionValidator<Client, WorldChainPooledTransaction>,
    >,
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
    root_validator: WorldChainRootValidator<Client>,
    pub(crate) pbh_db: Arc<DatabaseEnv>,
    num_pbh_txs: u16,
}

impl<Client, Tx> WorldChainTransactionValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt<Block = reth_primitives::Block>,
    Tx: WorldChainPoolTransaction<Consensus = TransactionSigned>,
{
    /// Create a new [`WorldChainTransactionValidator`].
    pub fn new(
        inner: OpTransactionValidator<Client, Tx>,
        root_validator: WorldChainRootValidator<Client>,
        pbh_db: Arc<DatabaseEnv>,
        num_pbh_txs: u16,
    ) -> Self {
        Self {
            inner,
            root_validator,
            pbh_db,
            num_pbh_txs,
        }
    }

    /// Get a reference to the inner transaction validator.
    pub fn inner(&self) -> &OpTransactionValidator<Client, Tx> {
        &self.inner
    }

    pub fn set_validated(&self, pbh_payload: &PbhPayload) -> Result<(), DatabaseError> {
        let db_tx = self.pbh_db.tx_mut()?;
        let mut cursor = db_tx.cursor_write::<ValidatedPbhTransaction>()?;
        cursor.insert(pbh_payload.nullifier_hash.to_be_bytes().into(), EmptyValue)?;
        db_tx.commit()?;
        Ok(())
    }

    /// Ensure the provided root is on chain and valid
    pub fn validate_root(
        &self,
        pbh_payload: &PbhPayload,
    ) -> Result<(), TransactionValidationError> {
        let is_valid = self.root_validator.validate_root(pbh_payload.root);
        if !is_valid {
            return Err(WorldChainTransactionPoolInvalid::InvalidRoot.into());
        }
        Ok(())
    }

    /// External nullifiers must be of the form
    /// `<prefix>-<periodId>-<PbhNonce>`.
    /// example:
    /// `v1-012025-11`
    pub fn validate_external_nullifier(
        &self,
        date: chrono::DateTime<chrono::Utc>,
        pbh_payload: &PbhPayload,
    ) -> Result<(), TransactionValidationError> {
        let external_nullifier: ExternalNullifier = pbh_payload
            .external_nullifier
            .parse()
            .map_err(WorldChainTransactionPoolInvalid::InvalidExternalNullifier)
            .map_err(TransactionValidationError::from)?;

        // In most cases these will be the same value, but at the month boundary
        // we'll still accept the previous month if the transaction is at most a minute late
        // or the next month if the transaction is at most a minute early
        let valid_dates = [
            DateMarker::from(date - chrono::Duration::minutes(1)),
            DateMarker::from(date),
            DateMarker::from(date + chrono::Duration::minutes(1)),
        ];
        if valid_dates
            .iter()
            .all(|d| external_nullifier.date_marker != *d)
        {
            return Err(WorldChainTransactionPoolInvalid::InvalidExternalNullifierPeriod.into());
        }

        if external_nullifier.nonce >= self.num_pbh_txs {
            return Err(WorldChainTransactionPoolInvalid::InvalidExternalNullifierNonce.into());
        }

        Ok(())
    }

    pub fn validate_pbh_payload(
        &self,
        transaction: &Tx,
        payload: &PbhPayload,
    ) -> Result<(), TransactionValidationError> {
        self.validate_root(payload)?;
        let date = chrono::Utc::now();
        self.validate_external_nullifier(date, payload)?;

        // Create db transaction and insert the nullifier hash
        // We do this first to prevent repeatedly validating the same transaction
        let db_tx = self.pbh_db.tx_mut()?;
        let mut cursor = db_tx.cursor_write::<ValidatedPbhTransaction>()?;
        cursor.insert(payload.nullifier_hash.to_be_bytes().into(), EmptyValue)?;

        let res = verify_proof(
            payload.root,
            payload.nullifier_hash,
            hash_to_field(transaction.hash().as_ref()),
            hash_to_field(payload.external_nullifier.as_bytes()),
            &payload.proof.0,
            TREE_DEPTH,
        );

        match res {
            Ok(true) => {
                // Only commit if the proof is valid
                db_tx.commit()?;
                Ok(())
            }
            Ok(false) => Err(WorldChainTransactionPoolInvalid::InvalidSemaphoreProof.into()),
            Err(e) => Err(TransactionValidationError::Error(e.into())),
        }
    }

    pub fn validate_one(
        &self,
        origin: TransactionOrigin,
        transaction: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        let validation_outcome = self.inner.validate_one(origin, transaction.clone());

        if let Some(pbh_payload) = transaction.pbh_payload() {
            if let Err(e) = self.validate_pbh_payload(&transaction, pbh_payload) {
                return e.to_outcome(transaction);
            }
        };

        validation_outcome
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
    Client: StateProviderFactory + BlockReaderIdExt<Block = reth_primitives::Block>,
    Tx: WorldChainPoolTransaction<Consensus = TransactionSigned>,
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
        self.inner.on_new_head_block(new_tip_block);
        // TODO: Handle reorgs
        self.root_validator.on_new_block(new_tip_block);
    }
}

#[cfg(test)]
pub mod tests {
    // use chrono::{TimeZone, Utc};
    // use ethers_core::types::U256;
    // use reth::transaction_pool::blobstore::InMemoryBlobStore;
    // use reth::transaction_pool::{
    //     Pool, PoolTransaction as _, TransactionPool, TransactionValidator,
    // };
    // use reth_primitives::{BlockBody, SealedBlock, SealedHeader};
    // use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
    // use semaphore::Field;
    // use test_case::test_case;

    // use crate::pbh::payload::{PbhPayload, Proof};
    // use crate::pool::ordering::WorldChainOrdering;
    // use crate::pool::root::{LATEST_ROOT_SLOT, OP_WORLD_ID};
    // use crate::test::{get_pbh_transaction, world_chain_validator};

    // #[tokio::test]
    // async fn validate_pbh_transaction() {
    //     let validator = world_chain_validator();
    //     let transaction = get_pbh_transaction(0);
    //     validator.inner.client().add_account(
    //         transaction.sender(),
    //         ExtendedAccount::new(transaction.nonce(), alloy_primitives::U256::MAX),
    //     );
    //     // Insert a world id root into the OpWorldId Account
    //     validator.inner.client().add_account(
    //         OP_WORLD_ID,
    //         ExtendedAccount::new(0, alloy_primitives::U256::ZERO).extend_storage(vec![(
    //             LATEST_ROOT_SLOT.into(),
    //             transaction.pbh_payload.clone().unwrap().root,
    //         )]),
    //     );
    //     let header = SealedHeader::default();
    //     let body = BlockBody::default();
    //     let block = SealedBlock::new(header, body);

    //     // Propogate the block to the root validator
    //     validator.on_new_head_block(&block);

    //     let ordering = WorldChainOrdering::default();

    //     let pool = Pool::new(
    //         validator,
    //         ordering,
    //         InMemoryBlobStore::default(),
    //         Default::default(),
    //     );

    //     let start = chrono::Utc::now();
    //     let res = pool.add_external_transaction(transaction.clone()).await;
    //     let first_insert = chrono::Utc::now() - start;
    //     println!("first_insert: {first_insert:?}");

    //     assert!(res.is_ok());
    //     let tx = pool.get(transaction.hash());
    //     assert!(tx.is_some());

    //     let start = chrono::Utc::now();
    //     let res = pool.add_external_transaction(transaction.clone()).await;

    //     let second_insert = chrono::Utc::now() - start;
    //     println!("second_insert: {second_insert:?}");

    //     // Check here that we're properly caching the transaction
    //     assert!(first_insert > second_insert * 10);
    //     assert!(res.is_err());
    // }

    // #[tokio::test]
    // async fn invalid_external_nullifier_hash() {
    //     let validator = world_chain_validator();
    //     let transaction = get_pbh_transaction(0);

    //     validator.inner.client().add_account(
    //         transaction.sender(),
    //         ExtendedAccount::new(transaction.nonce(), alloy_primitives::U256::MAX),
    //     );

    //     let ordering = WorldChainOrdering::default();

    //     let pool = Pool::new(
    //         validator,
    //         ordering,
    //         InMemoryBlobStore::default(),
    //         Default::default(),
    //     );

    //     let res = pool.add_external_transaction(transaction.clone()).await;
    //     assert!(res.is_err());
    // }

    // #[tokio::test]
    // async fn invalid_signal_hash() {
    //     let validator = world_chain_validator();
    //     let transaction = get_pbh_transaction(0);

    //     validator.inner.client().add_account(
    //         transaction.sender(),
    //         ExtendedAccount::new(transaction.nonce(), alloy_primitives::U256::MAX),
    //     );

    //     let ordering = WorldChainOrdering::default();

    //     let pool = Pool::new(
    //         validator,
    //         ordering,
    //         InMemoryBlobStore::default(),
    //         Default::default(),
    //     );

    //     let res = pool.add_external_transaction(transaction.clone()).await;
    //     assert!(res.is_err());
    // }

    // #[test]
    // fn test_validate_root() {
    //     let mut validator = world_chain_validator();
    //     let root = Field::from(1u64);
    //     let proof = Proof(semaphore::protocol::Proof(
    //         (U256::from(1u64), U256::from(2u64)),
    //         (
    //             [U256::from(3u64), U256::from(4u64)],
    //             [U256::from(5u64), U256::from(6u64)],
    //         ),
    //         (U256::from(7u64), U256::from(8u64)),
    //     ));
    //     let payload = PbhPayload {
    //         external_nullifier: "0-012025-11".to_string(),
    //         nullifier_hash: Field::from(10u64),
    //         root,
    //         proof,
    //     };
    //     let header = SealedHeader::default();
    //     let body = BlockBody::default();
    //     let block = SealedBlock::new(header, body);
    //     let client = MockEthProvider::default();
    //     // Insert a world id root into the OpWorldId Account
    //     client.add_account(
    //         OP_WORLD_ID,
    //         ExtendedAccount::new(0, alloy_primitives::U256::ZERO)
    //             .extend_storage(vec![(LATEST_ROOT_SLOT.into(), Field::from(1u64))]),
    //     );
    //     validator.root_validator.set_client(client);
    //     validator.on_new_head_block(&block);
    //     let res = validator.validate_root(&payload);
    //     assert!(res.is_ok());
    // }

    // #[test]
    // fn test_invalidate_root() {
    //     let mut validator = world_chain_validator();
    //     let root = Field::from(0);
    //     let proof = Proof(semaphore::protocol::Proof(
    //         (U256::from(1u64), U256::from(2u64)),
    //         (
    //             [U256::from(3u64), U256::from(4u64)],
    //             [U256::from(5u64), U256::from(6u64)],
    //         ),
    //         (U256::from(7u64), U256::from(8u64)),
    //     ));
    //     let payload = PbhPayload {
    //         external_nullifier: "0-012025-11".to_string(),
    //         nullifier_hash: Field::from(10u64),
    //         root,
    //         proof,
    //     };
    //     let header = SealedHeader::default();
    //     let body = BlockBody::default();
    //     let block = SealedBlock::new(header, body);
    //     let client = MockEthProvider::default();
    //     // Insert a world id root into the OpWorldId Account
    //     client.add_account(
    //         OP_WORLD_ID,
    //         ExtendedAccount::new(0, alloy_primitives::U256::ZERO)
    //             .extend_storage(vec![(LATEST_ROOT_SLOT.into(), Field::from(1u64))]),
    //     );
    //     validator.root_validator.set_client(client);
    //     validator.on_new_head_block(&block);
    //     let res = validator.validate_root(&payload);
    //     assert!(res.is_err());
    // }

    // #[test_case("v1-012025-0")]
    // #[test_case("v1-012025-1")]
    // #[test_case("v1-012025-29")]
    // fn validate_external_nullifier_valid(external_nullifier: &str) {
    //     let validator = world_chain_validator();
    //     let date = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

    //     let payload = PbhPayload {
    //         external_nullifier: external_nullifier.to_string(),
    //         nullifier_hash: Field::ZERO,
    //         root: Field::ZERO,
    //         proof: Default::default(),
    //     };

    //     validator
    //         .validate_external_nullifier(date, &payload)
    //         .unwrap();
    // }

    // #[test_case("v1-012025-0", "2024-12-31 23:59:30Z" ; "a minute early")]
    // #[test_case("v1-012025-0", "2025-02-01 00:00:30Z" ; "a minute late")]
    // fn validate_external_nullifier_at_time(external_nullifier: &str, time: &str) {
    //     let validator = world_chain_validator();
    //     let date: chrono::DateTime<Utc> = time.parse().unwrap();

    //     let payload = PbhPayload {
    //         external_nullifier: external_nullifier.to_string(),
    //         nullifier_hash: Field::ZERO,
    //         root: Field::ZERO,
    //         proof: Default::default(),
    //     };

    //     validator
    //         .validate_external_nullifier(date, &payload)
    //         .unwrap();
    // }

    // #[test_case("v0-012025-0")]
    // #[test_case("v1-022025-0")]
    // #[test_case("v1-122024-0")]
    // #[test_case("v1-002025-0")]
    // #[test_case("v1-012025-30")]
    // #[test_case("v1-012025")]
    // #[test_case("12025-0")]
    // #[test_case("v1-012025-0-0")]
    // fn validate_external_nullifier_invalid(external_nullifier: &str) {
    //     let validator = world_chain_validator();
    //     let date = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();

    //     let payload = PbhPayload {
    //         external_nullifier: external_nullifier.to_string(),
    //         nullifier_hash: Field::ZERO,
    //         root: Field::ZERO,
    //         proof: Default::default(),
    //     };

    //     let res = validator.validate_external_nullifier(date, &payload);
    //     assert!(res.is_err());
    // }

    // #[test]
    // fn test_set_validated() {
    //     let validator = world_chain_validator();

    //     let proof = Proof(semaphore::protocol::Proof(
    //         (U256::from(1u64), U256::from(2u64)),
    //         (
    //             [U256::from(3u64), U256::from(4u64)],
    //             [U256::from(5u64), U256::from(6u64)],
    //         ),
    //         (U256::from(7u64), U256::from(8u64)),
    //     ));
    //     let payload = PbhPayload {
    //         external_nullifier: "0-012025-11".to_string(),
    //         nullifier_hash: Field::from(10u64),
    //         root: Field::from(12u64),
    //         proof,
    //     };

    //     validator.set_validated(&payload).unwrap();
    // }
}
