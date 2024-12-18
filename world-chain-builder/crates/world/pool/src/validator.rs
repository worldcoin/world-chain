//! World Chain transaction pool types
use alloy_primitives::{Address, U256};
use alloy_rlp::Decodable;
use alloy_sol_types::SolCall;
use rayon::iter::{IndexedParallelIterator, IntoParallelRefIterator, ParallelIterator};
use reth::transaction_pool::{
    Pool, TransactionOrigin, TransactionValidationOutcome, TransactionValidationTaskExecutor,
    TransactionValidator,
};
use reth_optimism_node::txpool::OpTransactionValidator;
use reth_primitives::{Block, SealedBlock, TransactionSigned};
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use semaphore::protocol::verify_proof;
use world_chain_builder_pbh::date_marker::DateMarker;
use world_chain_builder_pbh::payload::{PbhPayload, TREE_DEPTH};

use super::error::{TransactionValidationError, WorldChainTransactionPoolInvalid};
use super::ordering::WorldChainOrdering;
use super::root::WorldChainRootValidator;
use super::tx::{WorldChainPoolTransaction, WorldChainPooledTransaction};
use crate::bindings::IPBHValidator;

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
    num_pbh_txs: u16,
    pbh_validator: Address,
    pbh_signature_aggregator: Address,
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
        num_pbh_txs: u16,
        pbh_validator: Address,
        pbh_signature_aggregator: Address,
    ) -> Self {
        Self {
            inner,
            root_validator,
            num_pbh_txs,
            pbh_validator,
            pbh_signature_aggregator,
        }
    }

    /// Get a reference to the inner transaction validator.
    pub fn inner(&self) -> &OpTransactionValidator<Client, Tx> {
        &self.inner
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
            .all(|d| pbh_payload.external_nullifier.date_marker != *d)
        {
            return Err(WorldChainTransactionPoolInvalid::InvalidExternalNullifierPeriod.into());
        }

        if pbh_payload.external_nullifier.nonce >= self.num_pbh_txs {
            return Err(WorldChainTransactionPoolInvalid::InvalidExternalNullifierNonce.into());
        }

        Ok(())
    }

    pub fn validate_pbh_payload(
        &self,
        payload: &PbhPayload,
        signal: U256,
    ) -> Result<(), TransactionValidationError> {
        self.validate_root(payload)?;
        let date = chrono::Utc::now();
        self.validate_external_nullifier(date, payload)?;

        let res = verify_proof(
            payload.root,
            payload.nullifier_hash,
            signal,
            payload.external_nullifier.hash(),
            &payload.proof.0,
            TREE_DEPTH,
        );

        match res {
            Ok(true) => Ok(()),
            Ok(false) => Err(WorldChainTransactionPoolInvalid::InvalidSemaphoreProof.into()),
            Err(e) => Err(TransactionValidationError::Error(e.into())),
        }
    }

    pub fn is_valid_eip4337_pbh_bundle(
        &self,
        tx: &Tx,
    ) -> Option<IPBHValidator::handleAggregatedOpsCall> {
        if !tx
            .input()
            .starts_with(&IPBHValidator::handleAggregatedOpsCall::SELECTOR)
        {
            return None;
        }

        let Ok(decoded) = IPBHValidator::handleAggregatedOpsCall::abi_decode(tx.input(), true)
        else {
            return None;
        };

        let are_aggregators_valid = decoded
            ._0
            .iter()
            .cloned()
            .all(|per_aggregator| per_aggregator.aggregator == self.pbh_signature_aggregator);

        if are_aggregators_valid {
            Some(decoded)
        } else {
            None
        }
    }

    pub fn validate_pbh_bundle(
        &self,
        transaction: &mut Tx,
    ) -> Result<(), TransactionValidationError> {
        let Some(calldata) = self.is_valid_eip4337_pbh_bundle(transaction) else {
            return Ok(());
        };

        for aggregated_ops in calldata._0 {
            let mut buff = aggregated_ops.signature.as_ref();
            let pbh_payloads = <Vec<PbhPayload>>::decode(&mut buff)
                .map_err(WorldChainTransactionPoolInvalid::from)
                .map_err(TransactionValidationError::from)?;

            if pbh_payloads.len() != aggregated_ops.userOps.len() {
                Err(WorldChainTransactionPoolInvalid::MissingPbhPayload)?;
            }

            pbh_payloads
                .par_iter()
                .zip(aggregated_ops.userOps)
                .try_for_each(|(payload, op)| {
                    let signal = crate::eip4337::hash_user_op(&op);

                    self.validate_pbh_payload(&payload, signal)?;

                    Ok::<(), TransactionValidationError>(())
                })?;
        }

        transaction.set_valid_pbh();

        Ok(())
    }
}

impl<Client, Tx> TransactionValidator for WorldChainTransactionValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt<Block = Block>,
    Tx: WorldChainPoolTransaction<Consensus = TransactionSigned>,
{
    type Transaction = Tx;

    async fn validate_transaction(
        &self,
        origin: TransactionOrigin,
        mut transaction: Self::Transaction,
    ) -> TransactionValidationOutcome<Self::Transaction> {
        if transaction.to().unwrap_or_default() == self.pbh_validator {
            if let Err(e) = self.validate_pbh_bundle(&mut transaction) {
                return e.to_outcome(transaction);
            }
        };

        self.inner.validate_one(origin, transaction.clone())
    }

    fn on_new_head_block(&self, new_tip_block: &SealedBlock) {
        self.inner.on_new_head_block(new_tip_block);
        // TODO: Handle reorgs
        self.root_validator.on_new_block(new_tip_block);
    }
}

#[cfg(test)]
pub mod tests {
    use alloy_primitives::Address;
    use alloy_sol_types::SolCall;
    use chrono::{TimeZone, Utc};
    use ethers_core::rand::rngs::SmallRng;
    use ethers_core::rand::{Rng, SeedableRng};
    use ethers_core::types::U256;
    use reth::transaction_pool::blobstore::InMemoryBlobStore;
    use reth::transaction_pool::{Pool, TransactionPool, TransactionValidator};
    use reth_primitives::{BlockBody, SealedBlock, SealedHeader};
    use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
    use semaphore::Field;
    use test_case::test_case;
    use world_chain_builder_pbh::date_marker::DateMarker;
    use world_chain_builder_pbh::external_nullifier::ExternalNullifier;
    use world_chain_builder_pbh::payload::{PbhPayload, Proof};

    use super::WorldChainTransactionValidator;
    use crate::ordering::WorldChainOrdering;
    use crate::root::{LATEST_ROOT_SLOT, OP_WORLD_ID};
    use crate::test_utils::{self, world_chain_validator, PBH_TEST_VALIDATOR};
    use crate::tx::WorldChainPooledTransaction;

    async fn setup() -> Pool<
        WorldChainTransactionValidator<MockEthProvider, WorldChainPooledTransaction>,
        WorldChainOrdering<WorldChainPooledTransaction>,
        InMemoryBlobStore,
    > {
        let validator = world_chain_validator();

        // Fund 10 test accounts
        for acc in 0..10 {
            let account_address = test_utils::account(acc);

            validator.inner.client().add_account(
                account_address,
                ExtendedAccount::new(0, alloy_primitives::U256::MAX),
            );
        }

        // Prep a merkle tree with first 5 accounts
        let tree = test_utils::tree();
        let root = tree.root();

        // Insert a world id root into the OpWorldId Account
        validator.inner.client().add_account(
            OP_WORLD_ID,
            ExtendedAccount::new(0, alloy_primitives::U256::ZERO)
                .extend_storage(vec![(LATEST_ROOT_SLOT.into(), root)]),
        );

        let header = SealedHeader::default();
        let body = BlockBody::default();
        let block = SealedBlock::new(header, body);

        // Propogate the block to the root validator
        validator.on_new_head_block(&block);

        let ordering = WorldChainOrdering::default();

        let pool = Pool::new(
            validator,
            ordering,
            InMemoryBlobStore::default(),
            Default::default(),
        );

        pool
    }

    #[tokio::test]
    async fn validate_noop_non_pbh() {
        const ACC: u32 = 0;

        let pool = setup().await;

        let account = test_utils::account(ACC);
        let tx = test_utils::eip1559().to(account).call();
        let tx = test_utils::eth_tx(ACC, tx).await;

        pool.add_external_transaction(tx.clone().into())
            .await
            .expect("Failed to add transaction");
    }

    #[tokio::test]
    async fn validate_no_duplicates() {
        const ACC: u32 = 0;

        let pool = setup().await;

        let account = test_utils::account(ACC);
        let tx = test_utils::eip1559().to(account).call();
        let tx = test_utils::eth_tx(ACC, tx).await;

        pool.add_external_transaction(tx.clone().into())
            .await
            .expect("Failed to add transaction");

        let res = pool.add_external_transaction(tx.clone().into()).await;

        assert!(res.is_err());
    }

    #[tokio::test]
    async fn validate_pbh_bundle() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        let (user_op, proof) = test_utils::user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(
                ExternalNullifier::builder()
                    .date_marker(DateMarker::from(chrono::Utc::now()))
                    .build(),
            )
            .call();
        let bundle = test_utils::pbh_bundle(vec![user_op], vec![proof]);
        let calldata = bundle.abi_encode();

        let tx = test_utils::eip1559()
            .to(PBH_TEST_VALIDATOR)
            .input(calldata)
            .call();

        let tx = test_utils::eth_tx(BUNDLER_ACCOUNT, tx).await;

        pool.add_external_transaction(tx.clone().into())
            .await
            .expect("Failed to add transaction");
    }

    #[tokio::test]
    async fn validate_bundle_no_pbh() {
        let mut rng = SmallRng::seed_from_u64(42);

        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        // NOTE: We're ignoring the proof here
        let (user_op, _proof) = test_utils::user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(
                ExternalNullifier::builder()
                    .date_marker(DateMarker::from(chrono::Utc::now()))
                    .build(),
            )
            .call();

        let bundle = test_utils::pbh_bundle(vec![user_op], vec![]);

        let calldata = bundle.abi_encode();

        let tx = test_utils::eip1559()
            // NOTE: Random receiving account
            .to(rng.gen::<Address>())
            .input(calldata)
            .call();

        let tx = test_utils::eth_tx(USER_ACCOUNT, tx).await;

        pool.add_external_transaction(tx.clone().into())
            .await
            .expect(
                "Validation should succeed - PBH data is invalid, but this is not a PBH bundle",
            );
    }

    #[tokio::test]
    async fn validate_pbh_missing_proof_for_user_op() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        // NOTE: We're ignoring the proof here
        let (user_op, _proof) = test_utils::user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(
                ExternalNullifier::builder()
                    .date_marker(DateMarker::from(chrono::Utc::now()))
                    .build(),
            )
            .call();

        let bundle = test_utils::pbh_bundle(vec![user_op], vec![]);

        let calldata = bundle.abi_encode();

        let tx = test_utils::eip1559()
            .to(PBH_TEST_VALIDATOR)
            .input(calldata)
            .call();

        let tx = test_utils::eth_tx(BUNDLER_ACCOUNT, tx).await;

        let err = pool
            .add_external_transaction(tx.clone().into())
            .await
            .expect_err("Validation should fail because of missing proof");

        assert!(err
            .to_string()
            .contains("one or more user ops are missing pbh payloads"),);
    }

    #[tokio::test]
    async fn validate_date_marker_outdated() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        let now = chrono::Utc::now();
        let month_in_the_past = now - chrono::Months::new(1);

        // NOTE: We're ignoring the proof here
        let (user_op, proof) = test_utils::user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(
                ExternalNullifier::builder()
                    .date_marker(DateMarker::from(month_in_the_past))
                    .build(),
            )
            .call();

        let bundle = test_utils::pbh_bundle(vec![user_op], vec![proof]);

        let calldata = bundle.abi_encode();

        let tx = test_utils::eip1559()
            .to(PBH_TEST_VALIDATOR)
            .input(calldata)
            .call();

        let tx = test_utils::eth_tx(BUNDLER_ACCOUNT, tx).await;

        let err = pool
            .add_external_transaction(tx.clone().into())
            .await
            .expect_err("Validation should fail because of missing proof");

        assert!(err
            .to_string()
            .contains("invalid external nullifier period"),);
    }

    #[tokio::test]
    async fn validate_date_marker_in_the_future() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        let now = chrono::Utc::now();
        let month_in_the_future = now + chrono::Months::new(1);

        // NOTE: We're ignoring the proof here
        let (user_op, proof) = test_utils::user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(
                ExternalNullifier::builder()
                    .date_marker(DateMarker::from(month_in_the_future))
                    .build(),
            )
            .call();

        let bundle = test_utils::pbh_bundle(vec![user_op], vec![proof]);

        let calldata = bundle.abi_encode();

        let tx = test_utils::eip1559()
            .to(PBH_TEST_VALIDATOR)
            .input(calldata)
            .call();

        let tx = test_utils::eth_tx(BUNDLER_ACCOUNT, tx).await;

        let err = pool
            .add_external_transaction(tx.clone().into())
            .await
            .expect_err("Validation should fail because of missing proof");

        assert!(err
            .to_string()
            .contains("invalid external nullifier period"),);
    }

    #[tokio::test]
    async fn invalid_external_nullifier_nonce() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        let (user_op, proof) = test_utils::user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(
                ExternalNullifier::builder()
                    .date_marker(DateMarker::from(chrono::Utc::now()))
                    .nonce(255)
                    .build(),
            )
            .call();

        let bundle = test_utils::pbh_bundle(vec![user_op], vec![proof]);

        let calldata = bundle.abi_encode();

        let tx = test_utils::eip1559()
            .to(PBH_TEST_VALIDATOR)
            .input(calldata)
            .call();

        let tx = test_utils::eth_tx(BUNDLER_ACCOUNT, tx).await;

        let err = pool
            .add_external_transaction(tx.clone().into())
            .await
            .expect_err("Validation should fail because of missing proof");

        assert!(err.to_string().contains("invalid external nullifier nonce"),);
    }

    #[test]
    fn valid_root() {
        let mut validator = world_chain_validator();
        let root = Field::from(1u64);
        let proof = Proof(semaphore::protocol::Proof(
            (U256::from(1u64), U256::from(2u64)),
            (
                [U256::from(3u64), U256::from(4u64)],
                [U256::from(5u64), U256::from(6u64)],
            ),
            (U256::from(7u64), U256::from(8u64)),
        ));
        let payload = PbhPayload {
            external_nullifier: ExternalNullifier::v1(1, 2025, 11),
            nullifier_hash: Field::from(10u64),
            root,
            proof,
        };
        let header = SealedHeader::default();
        let body = BlockBody::default();
        let block = SealedBlock::new(header, body);
        let client = MockEthProvider::default();
        // Insert a world id root into the OpWorldId Account
        client.add_account(
            OP_WORLD_ID,
            ExtendedAccount::new(0, alloy_primitives::U256::ZERO)
                .extend_storage(vec![(LATEST_ROOT_SLOT.into(), Field::from(1u64))]),
        );
        validator.root_validator.set_client(client);
        validator.on_new_head_block(&block);
        let res = validator.validate_root(&payload);
        assert!(res.is_ok());
    }

    #[test]
    fn invalid_root() {
        let mut validator = world_chain_validator();
        let root = Field::from(0);
        let proof = Proof(semaphore::protocol::Proof(
            (U256::from(1u64), U256::from(2u64)),
            (
                [U256::from(3u64), U256::from(4u64)],
                [U256::from(5u64), U256::from(6u64)],
            ),
            (U256::from(7u64), U256::from(8u64)),
        ));
        let payload = PbhPayload {
            external_nullifier: ExternalNullifier::v1(1, 2025, 11),
            nullifier_hash: Field::from(10u64),
            root,
            proof,
        };
        let header = SealedHeader::default();
        let body = BlockBody::default();
        let block = SealedBlock::new(header, body);
        let client = MockEthProvider::default();
        // Insert a world id root into the OpWorldId Account
        client.add_account(
            OP_WORLD_ID,
            ExtendedAccount::new(0, alloy_primitives::U256::ZERO)
                .extend_storage(vec![(LATEST_ROOT_SLOT.into(), Field::from(1u64))]),
        );
        validator.root_validator.set_client(client);
        validator.on_new_head_block(&block);
        let res = validator.validate_root(&payload);
        assert!(res.is_err());
    }

    #[test_case("v1-012025-0")]
    #[test_case("v1-012025-1")]
    #[test_case("v1-012025-29")]
    fn validate_external_nullifier_valid(external_nullifier: &str) {
        let validator = world_chain_validator();
        let date = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

        let payload = PbhPayload {
            external_nullifier: external_nullifier.parse().unwrap(),
            nullifier_hash: Field::ZERO,
            root: Field::ZERO,
            proof: Default::default(),
        };

        validator
            .validate_external_nullifier(date, &payload)
            .unwrap();
    }

    #[test_case("v1-012025-0", "2024-12-31 23:59:30Z" ; "a minute early")]
    #[test_case("v1-012025-0", "2025-02-01 00:00:30Z" ; "a minute late")]
    fn validate_external_nullifier_at_time(external_nullifier: &str, time: &str) {
        let validator = world_chain_validator();
        let date: chrono::DateTime<Utc> = time.parse().unwrap();

        let payload = PbhPayload {
            external_nullifier: external_nullifier.parse().unwrap(),
            nullifier_hash: Field::ZERO,
            root: Field::ZERO,
            proof: Default::default(),
        };

        validator
            .validate_external_nullifier(date, &payload)
            .unwrap();
    }
}
