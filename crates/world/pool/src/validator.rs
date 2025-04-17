//! World Chain transaction pool types
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::Arc;

use super::root::WorldChainRootValidator;
use super::tx::WorldChainPoolTransaction;
use crate::bindings::IPBHEntryPoint;
use crate::bindings::IPBHEntryPoint::PBHPayload;
use crate::error::WorldChainTransactionPoolError;
use crate::tx::WorldChainPoolTransactionError;
use alloy_eips::BlockId;
use alloy_primitives::Address;
use alloy_sol_types::{SolCall, SolValue};
use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};
use reth::transaction_pool::validate::ValidTransaction;
use reth::transaction_pool::{
    TransactionOrigin, TransactionValidationOutcome, TransactionValidator,
};
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::OpTransactionValidator;
use reth_optimism_primitives::OpTransactionSigned;
use reth_primitives::{Block, SealedBlock};
use reth_provider::{BlockReaderIdExt, ChainSpecProvider, StateProviderFactory};
use revm_primitives::U256;
use tracing::{info, warn};
use world_chain_builder_pbh::payload::{PBHPayload as PbhPayload, PBHValidationError};

/// The slot of the `pbh_gas_limit` in the PBHEntryPoint contract.
pub const PBH_GAS_LIMIT_SLOT: U256 = U256::from_limbs([53, 0, 0, 0]);

/// The slot of the `pbh_nonce_limit` in the PBHEntryPoint contract.
pub const PBH_NONCE_LIMIT_SLOT: U256 = U256::from_limbs([50, 0, 0, 0]);

/// The offset in bits of the `PBH_NONCE_LIMIT_SLOT` containing the u16 nonce limit.
pub const PBH_NONCE_LIMIT_OFFSET: u32 = 160;

/// Max u16
pub const MAX_U16: U256 = U256::from_limbs([0xFFFF, 0, 0, 0]);

/// Validator for World Chain transactions.
#[derive(Debug, Clone)]
pub struct WorldChainTransactionValidator<Client, Tx>
where
    Client: StateProviderFactory + BlockReaderIdExt,
{
    /// The inner transaction validator.
    inner: OpTransactionValidator<Client, Tx>,
    /// Validates World ID proofs contain a valid root in the WorldID account.
    root_validator: WorldChainRootValidator<Client>,
    /// The maximum number of PBH transactions a single World ID can execute in a given month.
    max_pbh_nonce: Arc<AtomicU16>,
    /// The maximum amount of gas a single PBH transaction can consume.
    max_pbh_gas_limit: Arc<AtomicU64>,
    /// The address of the entrypoint for all PBH transactions.
    pbh_entrypoint: Address,
    /// The address of the World ID PBH signature aggregator.
    pbh_signature_aggregator: Address,
}

impl<Client, Tx> WorldChainTransactionValidator<Client, Tx>
where
    Client: ChainSpecProvider<ChainSpec: OpHardforks>
        + StateProviderFactory
        + BlockReaderIdExt<Block = reth_primitives::Block<OpTransactionSigned>>,
    Tx: WorldChainPoolTransaction,
{
    /// Create a new [`WorldChainTransactionValidator`].
    pub fn new(
        inner: OpTransactionValidator<Client, Tx>,
        root_validator: WorldChainRootValidator<Client>,
        pbh_entrypoint: Address,
        pbh_signature_aggregator: Address,
    ) -> Result<Self, WorldChainTransactionPoolError> {
        let state = inner.client().state_by_block_id(BlockId::latest())?;
        // The `num_pbh_txs` storage is in a packed slot at a 160 bit offset consuming 16 bits.
        let max_pbh_nonce: u16 = ((state
            .storage(pbh_entrypoint, PBH_NONCE_LIMIT_SLOT.into())?
            .unwrap_or_default()
            >> PBH_NONCE_LIMIT_OFFSET)
            & MAX_U16)
            .to();
        let max_pbh_gas_limit: u64 = state
            .storage(pbh_entrypoint, PBH_GAS_LIMIT_SLOT.into())?
            .unwrap_or_default()
            .to();

        if max_pbh_nonce == 0 && max_pbh_gas_limit == 0 {
            warn!(
                %pbh_entrypoint,
                %pbh_signature_aggregator,
                "WorldChainTransactionValidator Initialized with PBH Disabled - Failed to fetch PBH nonce and gas limit from PBHEntryPoint. Defaulting to 0."
            )
        } else {
            info!(
                %max_pbh_gas_limit,
                %max_pbh_nonce,
                %pbh_entrypoint,
                %pbh_signature_aggregator,
                "WorldChainTransactionValidator Initialized with PBH Enabled"
            )
        }
        Ok(Self {
            inner,
            root_validator,
            max_pbh_nonce: Arc::new(AtomicU16::new(max_pbh_nonce)),
            max_pbh_gas_limit: Arc::new(AtomicU64::new(max_pbh_gas_limit)),
            pbh_entrypoint,
            pbh_signature_aggregator,
        })
    }

    /// Get a reference to the inner transaction validator.
    pub fn inner(&self) -> &OpTransactionValidator<Client, Tx> {
        &self.inner
    }

    /// Validates a PBH bundle transaction
    ///
    /// If the transaction is valid marks it for priority inclusion
    pub async fn validate_pbh_bundle(
        &self,
        origin: TransactionOrigin,
        tx: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        // Ensure that the tx is a valid OP transaction and return early if invalid
        let mut tx_outcome = self.inner.validate_one(origin, tx.clone()).await;

        // Decode the calldata and check that all UserOp specify the PBH signature aggregator
        let Ok(calldata) = IPBHEntryPoint::handleAggregatedOpsCall::abi_decode(tx.input(), true)
        else {
            return WorldChainPoolTransactionError::from(PBHValidationError::InvalidCalldata)
                .to_outcome(tx);
        };

        if !calldata
            ._0
            .iter()
            .all(|aggregator| aggregator.aggregator == self.pbh_signature_aggregator)
        {
            return WorldChainPoolTransactionError::from(
                PBHValidationError::InvalidSignatureAggregator,
            )
            .to_outcome(tx);
        }

        // Validate all proofs associated with each UserOp
        let mut aggregated_payloads = vec![];
        for aggregated_ops in calldata._0 {
            let buff = aggregated_ops.signature.as_ref();
            let pbh_payloads = match <Vec<PBHPayload>>::abi_decode(buff, true) {
                Ok(pbh_payloads) => pbh_payloads,
                Err(_) => {
                    return WorldChainPoolTransactionError::from(
                        PBHValidationError::InvalidCalldata,
                    )
                    .to_outcome(tx)
                }
            };

            if pbh_payloads.len() != aggregated_ops.userOps.len() {
                return WorldChainPoolTransactionError::from(PBHValidationError::MissingPbhPayload)
                    .to_outcome(tx);
            }

            let valid_roots = self.root_validator.roots();

            match pbh_payloads
                .into_par_iter()
                .zip(aggregated_ops.userOps)
                .map(|(payload, op)| {
                    let signal = crate::eip4337::hash_user_op(&op);
                    let Ok(payload) = PbhPayload::try_from(payload) else {
                        return Err(PBHValidationError::InvalidCalldata.into());
                    };
                    payload.validate(
                        signal,
                        &valid_roots,
                        self.max_pbh_nonce.load(Ordering::Relaxed),
                    )?;

                    Ok::<PbhPayload, WorldChainPoolTransactionError>(payload)
                })
                .collect::<Result<Vec<PbhPayload>, WorldChainPoolTransactionError>>()
            {
                Ok(payloads) => {
                    aggregated_payloads.extend(payloads);
                }
                Err(err) => return err.to_outcome(tx),
            }
        }

        if let TransactionValidationOutcome::Valid {
            transaction: ValidTransaction::Valid(tx),
            ..
        } = &mut tx_outcome
        {
            tx.set_pbh_payloads(aggregated_payloads);
        }

        tx_outcome
    }

    pub async fn validate_pbh(
        &self,
        origin: TransactionOrigin,
        tx: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        if tx.gas_limit() > self.max_pbh_gas_limit.load(Ordering::Relaxed) {
            return WorldChainPoolTransactionError::from(PBHValidationError::PbhGasLimitExceeded)
                .to_outcome(tx);
        }

        let function_signature: [u8; 4] = tx
            .input()
            .get(..4)
            .and_then(|bytes| bytes.try_into().ok())
            .unwrap_or_default();

        match function_signature {
            IPBHEntryPoint::handleAggregatedOpsCall::SELECTOR => {
                self.validate_pbh_bundle(origin, tx).await
            }
            _ => self.inner.validate_one(origin, tx.clone()).await,
        }
    }
}

impl<Client, Tx> TransactionValidator for WorldChainTransactionValidator<Client, Tx>
where
    Client: ChainSpecProvider<ChainSpec: OpHardforks>
        + StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>,
    Tx: WorldChainPoolTransaction<Consensus = OpTransactionSigned>,
{
    type Transaction = Tx;

    async fn validate_transaction(
        &self,
        origin: TransactionOrigin,
        transaction: Self::Transaction,
    ) -> TransactionValidationOutcome<Self::Transaction> {
        if transaction.to().unwrap_or_default() != self.pbh_entrypoint {
            return self.inner.validate_one(origin, transaction.clone()).await;
        }

        self.validate_pbh(origin, transaction).await
    }

    fn on_new_head_block<B>(&self, new_tip_block: &SealedBlock<B>)
    where
        B: reth_primitives_traits::Block,
    {
        if self.max_pbh_gas_limit.load(Ordering::Relaxed) == 0
            && self.max_pbh_nonce.load(Ordering::Relaxed) == 0
        {
            // Try and fetch the max pbh nonce and gas limit from the state at the latest block
            if let Ok(state) = self.inner.client().state_by_block_id(BlockId::latest()) {
                if let Some(max_pbh_nonce) = state
                    .storage(self.pbh_entrypoint, PBH_NONCE_LIMIT_SLOT.into())
                    .ok()
                    .flatten()
                {
                    let max_pbh_nonce = (max_pbh_nonce >> PBH_NONCE_LIMIT_OFFSET) & MAX_U16;
                    self.max_pbh_nonce
                        .store(max_pbh_nonce.to(), Ordering::Relaxed);
                }

                if let Some(max_pbh_gas_limit) = state
                    .storage(self.pbh_entrypoint, PBH_GAS_LIMIT_SLOT.into())
                    .ok()
                    .flatten()
                {
                    self.max_pbh_gas_limit
                        .store(max_pbh_gas_limit.to(), Ordering::Relaxed);
                }
            }
        }
        self.inner.on_new_head_block(new_tip_block);
        self.root_validator.on_new_block(new_tip_block);
    }
}

#[cfg(test)]
pub mod tests {
    use alloy_consensus::{Block, Header};
    use alloy_primitives::Address;
    use alloy_sol_types::SolCall;
    use ethers_core::rand::rngs::SmallRng;
    use ethers_core::rand::{Rng, SeedableRng};
    use reth::transaction_pool::blobstore::InMemoryBlobStore;
    use reth::transaction_pool::{Pool, TransactionPool, TransactionValidator};
    use reth_optimism_primitives::OpTransactionSigned;
    use reth_primitives::{BlockBody, SealedBlock};
    use world_chain_builder_pbh::date_marker::DateMarker;
    use world_chain_builder_pbh::external_nullifier::ExternalNullifier;
    use world_chain_builder_test_utils::utils::{
        account, eip1559, eth_tx, pbh_bundle, pbh_multicall, user_op, TREE,
    };
    use world_chain_builder_test_utils::{DEV_WORLD_ID, PBH_DEV_ENTRYPOINT};

    use crate::mock::{ExtendedAccount, MockEthProvider};
    use crate::ordering::WorldChainOrdering;
    use crate::root::LATEST_ROOT_SLOT;
    use crate::test_utils::world_chain_validator;
    use crate::tx::WorldChainPooledTransaction;

    use super::WorldChainTransactionValidator;

    async fn setup() -> Pool<
        WorldChainTransactionValidator<MockEthProvider, WorldChainPooledTransaction>,
        WorldChainOrdering<WorldChainPooledTransaction>,
        InMemoryBlobStore,
    > {
        let validator = world_chain_validator();

        // Fund 10 test accounts
        for acc in 0..10 {
            let account_address = account(acc);

            validator.inner.client().add_account(
                account_address,
                ExtendedAccount::new(0, alloy_primitives::U256::MAX),
            );
        }

        // Prep a merkle tree with first 5 accounts
        let root = TREE.root();

        // Insert a world id root into the OpWorldId Account
        validator.inner.client().add_account(
            DEV_WORLD_ID,
            ExtendedAccount::new(0, alloy_primitives::U256::ZERO)
                .extend_storage(vec![(LATEST_ROOT_SLOT.into(), root)]),
        );

        let header = Header {
            gas_limit: 20000000,
            ..Default::default()
        };
        let body = BlockBody::<OpTransactionSigned>::default();
        let block = SealedBlock::seal_slow(Block { header, body });

        // Propogate the block to the root validator
        validator.on_new_head_block(&block);

        let ordering = WorldChainOrdering::default();

        Pool::new(
            validator,
            ordering,
            InMemoryBlobStore::default(),
            Default::default(),
        )
    }

    #[tokio::test]
    async fn validate_noop_non_pbh() {
        const ACC: u32 = 0;

        let pool = setup().await;

        let account = account(ACC);
        let tx = eip1559().to(account).call();
        let tx = eth_tx(ACC, tx).await;

        pool.add_external_transaction(tx.into())
            .await
            .expect("Failed to add transaction");
    }

    #[tokio::test]
    async fn validate_no_duplicates() {
        const ACC: u32 = 0;

        let pool = setup().await;

        let account = account(ACC);
        let tx = eip1559().to(account).call();
        let tx = eth_tx(ACC, tx).await;

        pool.add_external_transaction(tx.clone().into())
            .await
            .expect("Failed to add transaction");

        let res = pool.add_external_transaction(tx.into()).await;

        assert!(res.is_err());
    }

    #[tokio::test]
    async fn validate_pbh_bundle() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        let (user_op, proof) = user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(ExternalNullifier::with_date_marker(
                DateMarker::from(chrono::Utc::now()),
                0,
            ))
            .call();
        let bundle = pbh_bundle(vec![user_op], vec![proof.into()]);
        let calldata = bundle.abi_encode();

        let tx = eip1559().to(PBH_DEV_ENTRYPOINT).input(calldata).call();

        let tx = eth_tx(BUNDLER_ACCOUNT, tx).await;

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
        let (user_op, _proof) = user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(ExternalNullifier::with_date_marker(
                DateMarker::from(chrono::Utc::now()),
                0,
            ))
            .call();

        let bundle = pbh_bundle(vec![user_op], vec![]);

        let calldata = bundle.abi_encode();

        let tx = eip1559()
            // NOTE: Random receiving account
            .to(rng.gen::<Address>())
            .input(calldata)
            .call();

        let tx = eth_tx(USER_ACCOUNT, tx).await;

        pool.add_external_transaction(tx.clone().into())
            .await
            .expect(
                "Validation should succeed - PBH data is invalid, but this is not a PBH bundle",
            );
    }

    #[tokio::test]
    async fn validate_pbh_bundle_missing_proof_for_user_op() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        // NOTE: We're ignoring the proof here
        let (user_op, _proof) = user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(ExternalNullifier::with_date_marker(
                DateMarker::from(chrono::Utc::now()),
                0,
            ))
            .call();

        let bundle = pbh_bundle(vec![user_op], vec![]);

        let calldata = bundle.abi_encode();

        let tx = eip1559().to(PBH_DEV_ENTRYPOINT).input(calldata).call();

        let tx = eth_tx(BUNDLER_ACCOUNT, tx).await;

        let err = pool
            .add_external_transaction(tx.clone().into())
            .await
            .expect_err("Validation should fail because of missing proof");

        assert!(err.to_string().contains("Missing PBH Payload"),);
    }

    #[tokio::test]
    async fn validate_pbh_multicall() {
        const USER_ACCOUNT: u32 = 1;

        let pool = setup().await;

        let calldata = pbh_multicall()
            .acc(USER_ACCOUNT)
            .external_nullifier(ExternalNullifier::with_date_marker(
                DateMarker::from(chrono::Utc::now()),
                0,
            ))
            .call();
        let calldata = calldata.abi_encode();

        let tx = eip1559().to(PBH_DEV_ENTRYPOINT).input(calldata).call();

        let tx = eth_tx(USER_ACCOUNT, tx).await;

        pool.add_external_transaction(tx.clone().into())
            .await
            .expect("Failed to add PBH multicall transaction");
    }

    #[tokio::test]
    async fn validate_date_marker_outdated() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        let now = chrono::Utc::now();
        let month_in_the_past = now - chrono::Months::new(1);

        // NOTE: We're ignoring the proof here
        let (user_op, proof) = user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(ExternalNullifier::with_date_marker(
                DateMarker::from(month_in_the_past),
                0,
            ))
            .call();

        let bundle = pbh_bundle(vec![user_op], vec![proof.into()]);

        let calldata = bundle.abi_encode();

        let tx = eip1559().to(PBH_DEV_ENTRYPOINT).input(calldata).call();

        let tx = eth_tx(BUNDLER_ACCOUNT, tx).await;

        let err = pool
            .add_external_transaction(tx.clone().into())
            .await
            .expect_err("Validation should fail because of missing proof");
        assert!(err
            .to_string()
            .contains("Invalid external nullifier period"),);
    }

    #[tokio::test]
    async fn validate_date_marker_in_the_future() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        let now = chrono::Utc::now();
        let month_in_the_future = now + chrono::Months::new(1);

        // NOTE: We're ignoring the proof here
        let (user_op, proof) = user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(ExternalNullifier::with_date_marker(
                DateMarker::from(month_in_the_future),
                0,
            ))
            .call();

        let bundle = pbh_bundle(vec![user_op], vec![proof.into()]);

        let calldata = bundle.abi_encode();

        let tx = eip1559().to(PBH_DEV_ENTRYPOINT).input(calldata).call();

        let tx = eth_tx(BUNDLER_ACCOUNT, tx).await;

        let err = pool
            .add_external_transaction(tx.clone().into())
            .await
            .expect_err("Validation should fail because of missing proof");

        assert!(err
            .to_string()
            .contains("Invalid external nullifier period"),);
    }

    #[tokio::test]
    async fn invalid_external_nullifier_nonce() {
        const BUNDLER_ACCOUNT: u32 = 9;
        const USER_ACCOUNT: u32 = 0;

        let pool = setup().await;

        let (user_op, proof) = user_op()
            .acc(USER_ACCOUNT)
            .external_nullifier(ExternalNullifier::with_date_marker(
                DateMarker::from(chrono::Utc::now()),
                u16::MAX,
            ))
            .call();

        let bundle = pbh_bundle(vec![user_op], vec![proof.into()]);

        let calldata = bundle.abi_encode();

        let tx = eip1559().to(PBH_DEV_ENTRYPOINT).input(calldata).call();

        let tx = eth_tx(BUNDLER_ACCOUNT, tx).await;

        let err = pool
            .add_external_transaction(tx.clone().into())
            .await
            .expect_err("Validation should fail because of missing proof");

        assert!(err.to_string().contains("Invalid external nullifier nonce"),);
    }
}
