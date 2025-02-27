//! World Chain transaction pool types
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
use semaphore::hash_to_field;
use world_chain_builder_pbh::payload::PBHPayload as PbhPayload;

/// The slot of the `pbh_gas_limit` in the PBHEntryPoint contract.
pub const PBH_GAS_LIMIT_SLOT: U256 = U256::from_limbs([305, 0, 0, 0]);

/// The slot of the `pbh_nonce_limit` in the PBHEntryPoint contract.
pub const PBH_NONCE_LIMIT_SLOT: U256 = U256::from_limbs([302, 0, 0, 0]);

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
    max_pbh_nonce: u16,
    /// The maximum amount of gas a single PBH transaction can consume.
    max_pbh_gas_limit: U256,
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
        let max_pbh_nonce: alloy_primitives::Uint<256, 4> = (state
            .storage(pbh_entrypoint, PBH_NONCE_LIMIT_SLOT.into())?
            .ok_or(WorldChainTransactionPoolError::Initialization(
                "PBH nonce limit not found in PBHEntryPoint contract".to_string(),
            ))?
            >> PBH_NONCE_LIMIT_OFFSET)
            & MAX_U16;

        let max_pbh_gas_limit = state
            .storage(pbh_entrypoint, PBH_GAS_LIMIT_SLOT.into())?
            .ok_or(WorldChainTransactionPoolError::Initialization(
                "PBH gas limit not found in PBHEntryPoint contract".to_string(),
            ))?;

        Ok(Self {
            inner,
            root_validator,
            max_pbh_nonce: max_pbh_nonce.to(),
            max_pbh_gas_limit,
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
    pub fn validate_pbh_bundle(
        &self,
        origin: TransactionOrigin,
        tx: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        // Ensure that the tx is a valid OP transaction and return early if invalid
        let mut tx_outcome = self.inner.validate_one(origin, tx.clone());

        // Decode the calldata and check that all UserOp specify the PBH signature aggregator
        let Ok(calldata) = IPBHEntryPoint::handleAggregatedOpsCall::abi_decode(tx.input(), true)
        else {
            return WorldChainPoolTransactionError::InvalidCalldata.to_outcome(tx);
        };

        if !calldata
            ._0
            .iter()
            .all(|aggregator| aggregator.aggregator == self.pbh_signature_aggregator)
        {
            return WorldChainPoolTransactionError::InvalidSignatureAggregator.to_outcome(tx);
        }

        // Validate all proofs associated with each UserOp
        for aggregated_ops in calldata._0 {
            let buff = aggregated_ops.signature.as_ref();
            let pbh_payloads = match <Vec<PBHPayload>>::abi_decode(buff, true) {
                Ok(pbh_payloads) => pbh_payloads,
                Err(_) => return WorldChainPoolTransactionError::InvalidCalldata.to_outcome(tx),
            };

            if pbh_payloads.len() != aggregated_ops.userOps.len() {
                return WorldChainPoolTransactionError::MissingPbhPayload.to_outcome(tx);
            }

            let valid_roots = self.root_validator.roots();
            if let Err(err) = pbh_payloads
                .into_par_iter()
                .zip(aggregated_ops.userOps)
                .try_for_each(|(payload, op)| {
                    let signal = crate::eip4337::hash_user_op(&op);
                    let Ok(payload) = PbhPayload::try_from(payload) else {
                        return Err(WorldChainPoolTransactionError::InvalidCalldata);
                    };
                    payload.validate(signal, &valid_roots, self.max_pbh_nonce)?;
                    Ok::<(), WorldChainPoolTransactionError>(())
                })
            {
                return err.to_outcome(tx);
            }
        }

        if let TransactionValidationOutcome::Valid {
            transaction: ValidTransaction::Valid(tx),
            ..
        } = &mut tx_outcome
        {
            tx.set_valid_pbh();
        }

        tx_outcome
    }

    /// Validates a PBH multicall transaction
    ///
    /// If the transaction is valid marks it for priority inclusion
    pub fn validate_pbh_multicall(
        &self,
        origin: TransactionOrigin,
        tx: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        // Ensure that the tx is a valid OP transaction and return early if invalid
        let mut tx_outcome = self.inner.validate_one(origin, tx.clone());

        // Decode the calldata and extract the PBH payload
        let Ok(calldata) = IPBHEntryPoint::pbhMulticallCall::abi_decode(tx.input(), true) else {
            return WorldChainPoolTransactionError::InvalidCalldata.to_outcome(tx);
        };

        let Ok(pbh_payload) = PbhPayload::try_from(calldata.payload) else {
            return WorldChainPoolTransactionError::InvalidCalldata.to_outcome(tx);
        };

        let signal_hash: alloy_primitives::Uint<256, 4> =
            hash_to_field(&SolValue::abi_encode_packed(&(tx.sender(), calldata.calls)));

        // Verify the proof
        if let Err(err) = pbh_payload.validate(
            signal_hash,
            &self.root_validator.roots(),
            self.max_pbh_nonce,
        ) {
            return WorldChainPoolTransactionError::PbhValidationError(err).to_outcome(tx);
        }

        if let TransactionValidationOutcome::Valid {
            transaction: ValidTransaction::Valid(tx),
            ..
        } = &mut tx_outcome
        {
            tx.set_valid_pbh();
        }

        tx_outcome
    }

    pub fn validate_pbh(
        &self,
        origin: TransactionOrigin,
        tx: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        if tx.gas_limit() > self.max_pbh_gas_limit.to() {
            return WorldChainPoolTransactionError::PbhGasLimitExceeded.to_outcome(tx);
        }

        let function_signature: [u8; 4] = tx
            .input()
            .get(..4)
            .and_then(|bytes| bytes.try_into().ok())
            .unwrap_or_default();

        match function_signature {
            IPBHEntryPoint::handleAggregatedOpsCall::SELECTOR => {
                self.validate_pbh_bundle(origin, tx)
            }
            IPBHEntryPoint::pbhMulticallCall::SELECTOR => self.validate_pbh_multicall(origin, tx),
            _ => self.inner.validate_one(origin, tx.clone()),
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
            return self.inner.validate_one(origin, transaction.clone());
        }

        self.validate_pbh(origin, transaction)
    }

    fn on_new_head_block<B>(&self, new_tip_block: &SealedBlock<B>)
    where
        B: reth_primitives_traits::Block,
    {
        self.inner.on_new_head_block(new_tip_block);
        self.root_validator.on_new_block(new_tip_block);
    }
}

#[cfg(test)]
pub mod tests {
    use std::u16;

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
