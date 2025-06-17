use alloy_rpc_types_debug::ExecutionWitness;
use alloy_signer_local::PrivateKeySigner;
use flashblocks::payload_builder_ctx::PayloadBuilderCtx;
use derive_more::with_trait::Error;
use eyre::eyre::eyre;
use op_alloy_rpc_types::OpTransactionRequest;
use op_revm::OpContext;
use reth::api::PayloadBuilderError;
use reth::payload::PayloadBuilderAttributes;
use reth::revm::database::StateProviderDatabase;
use reth::revm::witness::ExecutionWitnessRecord;
use reth::revm::State;
use reth::transaction_pool::{BestTransactionsAttributes, TransactionPool};
use reth_basic_payload_builder::{
    BuildArguments, BuildOutcome, BuildOutcomeKind, MissingPayloadBehaviour, PayloadBuilder,
    PayloadConfig,
};
use reth_chain_state::{ExecutedBlock, ExecutedBlockWithTrieUpdates, ExecutedTrieUpdates};
use reth_evm::execute::BlockBuilderOutcome;
use reth_evm::execute::{BlockBuilder, BlockExecutor};
use reth_evm::Database;
use reth_evm::precompiles::PrecompilesMap;
use reth_evm::Evm;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::estimated_da_size::DataAvailabilitySized;
use reth_optimism_node::{
    OpBuiltPayload, OpEvmConfig, OpPayloadBuilder, OpPayloadBuilderAttributes,
};
use reth_optimism_payload_builder::builder::{OpPayloadBuilderCtx, OpPayloadTransactions};
use reth_optimism_payload_builder::config::OpBuilderConfig;
use reth_optimism_payload_builder::OpPayloadAttributes;
use reth_optimism_primitives::{OpPrimitives, OpTransactionSigned};
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_primitives::{Block, Recovered, SealedHeader};
use reth_primitives_traits::SignerRecoverable;
use reth_provider::{
    BlockReaderIdExt, ChainSpecProvider, ExecutionOutcome, ProviderError, StateProvider,
    StateProviderFactory,
};
use reth_transaction_pool::BlobStore;
use revm_primitives::Address;
use std::sync::Arc;
use tracing::debug;
use world_chain_builder_pool::tx::WorldChainPooledTransaction;
use world_chain_builder_pool::WorldChainTransactionPool;

use crate::ctx::WorldChainPayloadBuilderCtx;

/// World Chain payload builder
#[derive(Debug, Clone)]
pub struct WorldChainPayloadBuilder<Client, S, Txs = ()>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + ChainSpecProvider<ChainSpec: OpHardforks>
        + Clone
        + 'static,
{
    pub inner: OpPayloadBuilder<WorldChainTransactionPool<Client, S>, Client, OpEvmConfig, Txs>,
    pub verified_blockspace_capacity: u8,
    pub pbh_entry_point: Address,
    pub pbh_signature_aggregator: Address,
    pub builder_private_key: PrivateKeySigner,
}

impl<Client, S> WorldChainPayloadBuilder<Client, S>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + ChainSpecProvider<ChainSpec: OpHardforks>
        + Clone
        + 'static,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: WorldChainTransactionPool<Client, S>,
        client: Client,
        evm_config: OpEvmConfig,
        compute_pending_block: bool,
        verified_blockspace_capacity: u8,
        pbh_entry_point: Address,
        pbh_signature_aggregator: Address,
        builder_private_key: String,
    ) -> Self {
        Self::with_builder_config(
            pool,
            client,
            evm_config,
            OpBuilderConfig::default(),
            compute_pending_block,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            builder_private_key,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_builder_config(
        pool: WorldChainTransactionPool<Client, S>,
        client: Client,
        evm_config: OpEvmConfig,
        config: OpBuilderConfig,
        compute_pending_block: bool,
        verified_blockspace_capacity: u8,
        pbh_entry_point: Address,
        pbh_signature_aggregator: Address,
        builder_private_key: String,
    ) -> Self {
        let inner = OpPayloadBuilder::with_builder_config(pool, client, evm_config, config)
            .set_compute_pending_block(compute_pending_block);

        let private_key = builder_private_key
            .parse()
            .expect("invalid builder private key");
        Self {
            inner,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            builder_private_key: private_key,
        }
    }
}

impl<Client, S> WorldChainPayloadBuilder<Client, S>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + ChainSpecProvider<ChainSpec: OpHardforks>
        + Clone
        + 'static,
{
    /// Sets the rollup's compute pending block configuration option.
    pub const fn set_compute_pending_block(mut self, compute_pending_block: bool) -> Self {
        self.inner.compute_pending_block = compute_pending_block;
        self
    }

    pub fn with_transactions<T>(
        self,
        best_transactions: T,
    ) -> WorldChainPayloadBuilder<Client, S, T> {
        let Self {
            inner,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            builder_private_key,
        } = self;

        let OpPayloadBuilder {
            compute_pending_block,
            evm_config,
            config,
            pool,
            client,
            ..
        } = inner;

        WorldChainPayloadBuilder {
            inner: OpPayloadBuilder {
                compute_pending_block,
                evm_config,
                config,
                pool,
                client,
                best_transactions,
            },
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            builder_private_key,
        }
    }

    /// Enables the rollup's compute pending block configuration option.
    pub const fn compute_pending_block(self) -> Self {
        self.set_compute_pending_block(true)
    }

    /// Returns the rollup's compute pending block configuration option.
    pub const fn is_compute_pending_block(&self) -> bool {
        self.inner.compute_pending_block
    }
}

impl<Client, S, T> WorldChainPayloadBuilder<Client, S, T>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + ChainSpecProvider<ChainSpec: OpHardforks>
        + Clone
        + 'static,
    S: BlobStore + Clone,
{
    /// Constructs an Worldchain payload from the transactions sent via the
    /// Payload attributes by the sequencer. If the `no_tx_pool` argument is passed in
    /// the payload attributes, the transaction pool will be ignored and the only transactions
    /// included in the payload will be those sent through the attributes.
    ///
    /// Given build arguments including an Optimism client, transaction pool,
    /// and configuration, this function creates a transaction payload. Returns
    /// a result indicating success with the payload or an error in case of failure.
    fn build_payload<'a, Txs>(
        &self,
        args: BuildArguments<OpPayloadBuilderAttributes<OpTransactionSigned>, OpBuiltPayload>,
        best: impl FnOnce(BestTransactionsAttributes) -> Txs + Send + Sync + 'a,
    ) -> Result<BuildOutcome<OpBuiltPayload>, PayloadBuilderError>
    where
        Txs: PayloadTransactions<Transaction = WorldChainPooledTransaction>,
    {
        let BuildArguments {
            mut cached_reads,
            config,
            cancel,
            best_payload,
        } = args;

        let ctx = WorldChainPayloadBuilderCtx {
            inner: Arc::new(OpPayloadBuilderCtx {
                evm_config: self.inner.evm_config.clone(),
                da_config: self.inner.config.da_config.clone(),
                chain_spec: self.inner.client.chain_spec(),
                config,
                cancel,
                best_payload,
            }),
            client: self.inner.client.clone(),
            verified_blockspace_capacity: self.verified_blockspace_capacity,
            pbh_entry_point: self.pbh_entry_point,
            pbh_signature_aggregator: self.pbh_signature_aggregator,
            builder_private_key: self.builder_private_key.clone(),
            pool: self.inner.pool.clone(),
        };

        let op_ctx = &ctx.inner;
        let builder = WorldChainBuilder::new(best);
        let state_provider = self
            .inner
            .client
            .state_by_block_hash(op_ctx.parent().hash())?;
        let state = StateProviderDatabase::new(&state_provider);

        if op_ctx.attributes().no_tx_pool {
            builder.build(state, &state_provider, ctx)
        } else {
            // sequencer mode we can reuse cachedreads from previous runs
            builder.build(cached_reads.as_db_mut(state), &state_provider, ctx)
        }
        .map(|out| out.with_cached_reads(cached_reads))
    }

    /// Computes the witness for the payload.
    pub fn payload_witness(
        &self,
        parent: SealedHeader,
        attributes: OpPayloadAttributes,
    ) -> Result<ExecutionWitness, PayloadBuilderError> {
        let attributes = OpPayloadBuilderAttributes::try_new(parent.hash(), attributes, 3)
            .map_err(PayloadBuilderError::other)?;

        let config = PayloadConfig {
            parent_header: Arc::new(parent),
            attributes,
        };

        let client = self.inner.client.clone();
        let ctx = WorldChainPayloadBuilderCtx {
            inner: Arc::new(OpPayloadBuilderCtx {
                evm_config: self.inner.evm_config.clone(),
                da_config: self.inner.config.da_config.clone(),
                chain_spec: self.inner.client.chain_spec(),
                config,
                cancel: Default::default(),
                best_payload: Default::default(),
            }),
            client,
            verified_blockspace_capacity: self.verified_blockspace_capacity,
            pbh_entry_point: self.pbh_entry_point,
            pbh_signature_aggregator: self.pbh_signature_aggregator,
            builder_private_key: self.builder_private_key.clone(),
            pool: self.inner.pool.clone(),
        };

        let state_provider = self
            .inner
            .client
            .state_by_block_hash(ctx.inner.parent().hash())?;

        let builder: WorldChainBuilder<'_, NoopPayloadTransactions<WorldChainPooledTransaction>> =
            WorldChainBuilder::new(|_| NoopPayloadTransactions::default());

        builder.witness(state_provider, &ctx)
    }
}

/// Implementation of the [`PayloadBuilder`] trait for [`WorldChainPayloadBuilder`].
impl<Client, S: BlobStore + Clone, Txs> PayloadBuilder for WorldChainPayloadBuilder<Client, S, Txs>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + Clone
        + 'static,
    Txs: OpPayloadTransactions<WorldChainPooledTransaction>,
{
    type Attributes = OpPayloadBuilderAttributes<OpTransactionSigned>;
    type BuiltPayload = OpBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        let pool = self.inner.pool.clone();
        self.build_payload(args, |attrs| {
            self.inner.best_transactions.best_transactions(pool, attrs)
        })
    }

    fn on_missing_payload(
        &self,
        _args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> MissingPayloadBehaviour<Self::BuiltPayload> {
        // we want to await the job that's already in progress because that should be returned as
        // is, there's no benefit in racing another job
        MissingPayloadBehaviour::AwaitInProgress
    }

    // NOTE: this should only be used for testing purposes because this doesn't have access to L1
    // system txs, hence on_missing_payload we return [MissingPayloadBehaviour::AwaitInProgress].
    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        let args = BuildArguments {
            config,
            cached_reads: Default::default(),
            cancel: Default::default(),
            best_payload: None,
        };
        self.build_payload(args, |_| {
            NoopPayloadTransactions::<WorldChainPooledTransaction>::default()
        })?
        .into_payload()
        .ok_or_else(|| PayloadBuilderError::MissingPayload)
    }
}

/// The type that builds the payload.
///
/// Payload building for optimism is composed of several steps.
/// The first steps are mandatory and defined by the protocol.
///
/// 1. first all System calls are applied.
/// 2. After canyon the forced deployed `create2deployer` must be loaded
/// 3. all sequencer transactions are executed (part of the payload attributes)
///
/// Depending on whether the node acts as a sequencer and is allowed to include additional
/// transactions (`no_tx_pool == false`):
/// 4. include additional transactions
///
/// And finally
/// 5. build the block: compute all roots (txs, state)
#[derive(derive_more::Debug)]
pub struct WorldChainBuilder<'a, Txs> {
    /// Yields the best transaction to include if transactions from the mempool are allowed.
    #[debug(skip)]
    best: Box<dyn FnOnce(BestTransactionsAttributes) -> Txs + 'a>,
}

impl<'a, Txs> WorldChainBuilder<'a, Txs> {
    fn new(best: impl FnOnce(BestTransactionsAttributes) -> Txs + Send + Sync + 'a) -> Self {
        Self {
            best: Box::new(best),
        }
    }
}

impl<Txs> WorldChainBuilder<'_, Txs> {
    /// Builds the payload on top of the state.
    pub fn build<Pool, Client>(
        self,
        db: impl Database<Error = ProviderError>,
        state_provider: impl StateProvider,
        ctx: WorldChainPayloadBuilderCtx<Client, Pool>,
    ) -> Result<BuildOutcomeKind<OpBuiltPayload<OpPrimitives>>, PayloadBuilderError>
    where
        Txs: PayloadTransactions<Transaction = WorldChainPooledTransaction>,
        Pool: TransactionPool<Transaction = WorldChainPooledTransaction>,
        Client: StateProviderFactory
            + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
            + ChainSpecProvider<ChainSpec: OpHardforks>
            + Clone,
    {
        let Self { best } = self;

        let mut state = State::builder()
            .with_database(db)
            .with_bundle_update()
            .build();

        let op_ctx = &ctx.inner;
        debug!(target: "payload_builder", id=%op_ctx.payload_id(), parent_header = ?ctx.inner.parent().hash(), parent_number = ctx.inner.parent().number, "building new payload");

        // Prepare block builder.
        let mut builder = PayloadBuilderCtx::block_builder(&ctx, &mut state)?;

        // 1. apply pre-execution changes
        builder.apply_pre_execution_changes()?;

        // 2. execute sequencer transactions
        let mut info = op_ctx.execute_sequencer_transactions(&mut builder)?;

        // 3. if mem pool transactions are requested we execute them
        if !op_ctx.attributes().no_tx_pool {
            let best_txs = best(op_ctx.best_transaction_attributes(builder.evm_mut().block()));
            // TODO: Validate gas limit
            if ctx
                .execute_best_transactions(&mut info, &mut builder, best_txs, 0)?
                .is_some()
            {
                return Ok(BuildOutcomeKind::Cancelled);
            }

            // check if the new payload is even more valuable
            if !ctx.inner.is_better_payload(info.total_fees) {
                // can skip building the block
                return Ok(BuildOutcomeKind::Aborted {
                    fees: info.total_fees,
                });
            }
        }

        let BlockBuilderOutcome {
            execution_result,
            hashed_state,
            trie_updates,
            block,
        } = builder.finish(state_provider)?;

        let sealed_block = Arc::new(block.sealed_block().clone());
        debug!(target: "payload_builder", id=%op_ctx.payload_id(), sealed_block_header = ?sealed_block.header(), "sealed built block");

        let execution_outcome = ExecutionOutcome::new(
            state.take_bundle(),
            vec![execution_result.receipts],
            block.number,
            Vec::new(),
        );

        // create the executed block data
        let executed: ExecutedBlockWithTrieUpdates<OpPrimitives> = ExecutedBlockWithTrieUpdates {
            block: ExecutedBlock {
                recovered_block: Arc::new(block),
                execution_output: Arc::new(execution_outcome),
                hashed_state: Arc::new(hashed_state),
            },
            trie: ExecutedTrieUpdates::Present(Arc::new(trie_updates)),
        };

        let no_tx_pool = op_ctx.attributes().no_tx_pool;

        let payload = OpBuiltPayload::new(
            op_ctx.payload_id(),
            sealed_block,
            info.total_fees,
            Some(executed),
        );

        if no_tx_pool {
            // if `no_tx_pool` is set only transactions from the payload attributes will be included
            // in the payload. In other words, the payload is deterministic and we can
            // freeze it once we've successfully built it.
            Ok(BuildOutcomeKind::Freeze(payload))
        } else {
            Ok(BuildOutcomeKind::Better { payload })
        }
    }

    /// Builds the payload and returns its [`ExecutionWitness`] based on the state after execution.
    pub fn witness<Pool, Client>(
        self,
        state_provider: impl StateProvider,
        ctx: &WorldChainPayloadBuilderCtx<Client, Pool>,
    ) -> Result<ExecutionWitness, PayloadBuilderError>
    where
        Txs: PayloadTransactions<Transaction = WorldChainPooledTransaction>,
        Pool: TransactionPool<Transaction = WorldChainPooledTransaction>,
        Client: StateProviderFactory
            + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
            + ChainSpecProvider<ChainSpec: OpHardforks>
            + Clone,
    {
        let Self { best } = self;

        let mut db = State::builder()
            .with_database(StateProviderDatabase::new(&state_provider))
            .with_bundle_update()
            .build();
        let mut builder = PayloadBuilderCtx::block_builder(ctx, &mut db)?;

        builder.apply_pre_execution_changes()?;
        let mut info = ctx.inner.execute_sequencer_transactions(&mut builder)?;
        if !ctx.inner.attributes().no_tx_pool {
            let best_txs = best(
                ctx.inner
                    .best_transaction_attributes(builder.evm_mut().block()),
            );
            // TODO: Validate gas limit
            ctx.execute_best_transactions(&mut info, &mut builder, best_txs, 0)?;
        }
        builder.into_executor().apply_post_execution_changes()?;

        let ExecutionWitnessRecord {
            hashed_state,
            codes,
            keys,
            ..
        } = ExecutionWitnessRecord::from_executed_state(&db);
        let state = state_provider.witness(Default::default(), hashed_state)?;
        Ok(ExecutionWitness {
            state: state.into_iter().collect(),
            codes,
            keys,
            ..Default::default()
        })
    }
}

/// Container type that holds all necessities to build a new payload.
#[derive(Debug)]
pub struct WorldChainPayloadBuilderCtx<Client>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + ChainSpecProvider<ChainSpec: OpHardforks>
        + Clone,
{
    pub inner: OpPayloadBuilderCtx<OpEvmConfig, <Client as ChainSpecProvider>::ChainSpec>,
    pub verified_blockspace_capacity: u8,
    pub pbh_entry_point: Address,
    pub pbh_signature_aggregator: Address,
    pub client: Client,
    pub builder_private_key: PrivateKeySigner,
}

impl<Client> WorldChainPayloadBuilderCtx<Client>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + ChainSpecProvider<ChainSpec: OpHardforks>
        + Clone,
{
    /// Executes the given best transactions and updates the execution info.
    ///
    /// Returns `Ok(Some(())` if the job was cancelled.
    pub fn execute_best_transactions<TXS, DB, Builder, Pool>(
        &self,
        info: &mut ExecutionInfo,
        builder: &mut Builder,
        mut best_txs: TXS,
        pool: &Pool,
    ) -> Result<Option<()>, PayloadBuilderError>
    where
        DB: Database + DatabaseCommit,
        Builder: BlockBuilder<
            Primitives = OpPrimitives,
            Executor: BlockExecutor<Evm = OpEvm<DB, NoOpInspector, PrecompilesMap>>,
        >,
        TXS: PayloadTransactions<
            Transaction: WorldChainPoolTransaction<Consensus = OpTransactionSigned>,
        >,
        Pool: TransactionPool<
            Transaction: WorldChainPoolTransaction<Consensus = OpTransactionSigned>,
        >,
    {
        let mut block_gas_limit = builder.evm_mut().block().gas_limit;
        let block_da_limit = self.inner.da_config.max_da_block_size();
        let tx_da_limit = self.inner.da_config.max_da_tx_size();
        let base_fee = builder.evm_mut().block().basefee;

        let mut invalid_txs = vec![];
        let verified_gas_limit = (self.verified_blockspace_capacity as u64 * block_gas_limit) / 100;

        let mut spent_nullifier_hashes = HashSet::new();
        while let Some(pooled_tx) = best_txs.next(()) {
            let tx_da_size = pooled_tx.estimated_da_size();
            let tx = pooled_tx.clone().into_consensus();

            if info.is_tx_over_limits(
                tx_da_size,
                block_gas_limit,
                tx_da_limit,
                block_da_limit,
                tx.gas_limit(),
            ) {
                // we can't fit this transaction into the block, so we need to mark it as
                // invalid which also removes all dependent transaction from
                // the iterator before we can continue
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            if let Some(conditional_options) = pooled_tx.conditional_options() {
                if validate_conditional_options(conditional_options, &self.client).is_err() {
                    best_txs.mark_invalid(tx.signer(), tx.nonce());
                    invalid_txs.push(*pooled_tx.hash());
                    continue;
                }
            }

            // A sequencer's block should never contain blob or deposit transactions from the pool.
            if tx.is_eip4844() || tx.is_deposit() {
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            // check if the job was cancelled, if so we can exit early
            if self.inner.cancel.is_cancelled() {
                return Ok(Some(()));
            }

            // If the transaction is verified, check if it can be added within the verified gas limit
            if let Some(payloads) = pooled_tx.pbh_payload() {
                if info.cumulative_gas_used + tx.gas_limit() > verified_gas_limit {
                    best_txs.mark_invalid(tx.signer(), tx.nonce());
                    continue;
                }

                if payloads
                    .iter()
                    .any(|payload| !spent_nullifier_hashes.insert(payload.nullifier_hash))
                {
                    best_txs.mark_invalid(tx.signer(), tx.nonce());
                    invalid_txs.push(*pooled_tx.hash());
                    continue;
                }
            }

            let gas_used = match builder.execute_transaction(tx.clone()) {
                Ok(res) => {
                    if let Some(payloads) = pooled_tx.pbh_payload() {
                        if spent_nullifier_hashes.len() == payloads.len() {
                            block_gas_limit -= FIXED_GAS
                        }

                        block_gas_limit -= COLD_SSTORE_GAS * payloads.len() as u64;
                    }
                    res
                }
                Err(err) => {
                    match err {
                        BlockExecutionError::Validation(BlockValidationError::InvalidTx {
                            error,
                            ..
                        }) => {
                            if error.is_nonce_too_low() {
                                // if the nonce is too low, we can skip this transaction
                                trace!(target: "payload_builder", %error, ?tx, "skipping nonce too low transaction");
                            } else {
                                // if the transaction is invalid, we can skip it and all of its
                                // descendants
                                trace!(target: "payload_builder", %error, ?tx, "skipping invalid transaction and its descendants");
                                best_txs.mark_invalid(tx.signer(), tx.nonce());
                            }

                            continue;
                        }

                        err => {
                            // this is an error that we should treat as fatal for this attempt
                            return Err(PayloadBuilderError::EvmExecutionError(Box::new(err)));
                        }
                    }
                }
            };

            self.commit_changes(info, base_fee, gas_used, tx);
        }

        if !spent_nullifier_hashes.is_empty() {
            let tx = spend_nullifiers_tx(self, builder.evm_mut(), spent_nullifier_hashes).map_err(
                |e| {
                    error!(target: "payload_builder", %e, "failed to build spend nullifiers transaction");
                    PayloadBuilderError::Other(e.into())
                },
            )?;

            // Try to execute the builder tx. In the event that execution fails due to
            // insufficient funds, continue with the built payload. This ensures that
            // PBH transactions still receive priority inclusion, even if the PBH nullifier
            // is not spent rather than sitting in the default execution client's mempool.
            match builder.execute_transaction(tx.clone()) {
                Ok(gas_used) => self.commit_changes(info, base_fee, gas_used, tx),
                Err(e) => {
                    error!(target: "payload_builder", %e, "spend nullifiers transaction failed")
                }
            }
        }

        if !invalid_txs.is_empty() {
            pool.remove_transactions(invalid_txs);
        }

        Ok(None)
    }

    /// After computing the execution result and state we can commit changes to the database
    fn commit_changes(
        &self,
        info: &mut ExecutionInfo,
        base_fee: u64,
        gas_used: u64,
        tx: Recovered<OpTransactionSigned>,
    ) {
        // add gas used by the transaction to cumulative gas used, before creating the
        // receipt
        info.cumulative_gas_used += gas_used;
        info.cumulative_da_bytes_used += tx.length() as u64;

        // update add to total fees
        let miner_fee = tx
            .effective_tip_per_gas(base_fee)
            .expect("fee is always valid; execution succeeded");
        info.total_fees += U256::from(miner_fee) * U256::from(gas_used);
    }

    /// Prepares [`BlockBuilder`] for the payload. This will configure the underlying EVM with [`PBHCallTracer`],
    /// but disable it by default. It will get enabled by [`WorldChainPayloadBuilderCtx::execute_best_transactions`].
    fn block_builder<'a, DB: Database>(
        &'a self,
        db: &'a mut State<DB>,
    ) -> Result<
        impl BlockBuilder<
            Primitives = OpPrimitives,
            Executor: BlockExecutor<Evm = OpEvm<&'a mut State<DB>, NoOpInspector, PrecompilesMap>>,
        >,
        PayloadBuilderError,
    > {
        // Prepare attributes for next block environment.
        let attributes = OpNextBlockEnvAttributes {
            timestamp: self.inner.attributes().timestamp(),
            suggested_fee_recipient: self.inner.attributes().suggested_fee_recipient(),
            prev_randao: self.inner.attributes().prev_randao(),
            gas_limit: self
                .inner
                .attributes()
                .gas_limit
                .unwrap_or(self.inner.parent().gas_limit),
            parent_beacon_block_root: self.inner.attributes().parent_beacon_block_root(),
            extra_data: self.inner.extra_data()?,
        };

        // Prepare EVM environment.
        let evm_env = self
            .inner
            .evm_config
            .next_evm_env(self.inner.parent(), &attributes)
            .map_err(PayloadBuilderError::other)?;

        // Prepare EVM.
        let evm = self.inner.evm_config.evm_with_env(db, evm_env);

        // Prepare block execution context.
        let execution_ctx = self
            .inner
            .evm_config
            .context_for_next_block(self.inner.parent(), attributes);

        // Prepare block builder.
        Ok(self
            .inner
            .evm_config
            .create_block_builder(evm, self.inner.parent(), execution_ctx))
    }
}

pub const COLD_SSTORE_GAS: u64 = 20000;
pub const FIXED_GAS: u64 = 100_000;

pub const fn dyn_gas_limit(len: u64) -> u64 {
    FIXED_GAS + len * COLD_SSTORE_GAS
}

pub fn spend_nullifiers_tx<DB, I, Client>(
    ctx: &WorldChainPayloadBuilderCtx<Client>,
    evm: &mut OpEvm<DB, I, PrecompilesMap>,
    nullifier_hashes: HashSet<Field>,
) -> eyre::Result<Recovered<OpTransactionSigned>>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + ChainSpecProvider<ChainSpec: OpHardforks>
        + Clone,
    I: Inspector<OpContext<DB>>,
    DB: revm::Database + revm::DatabaseCommit,
    <DB as revm::Database>::Error: std::fmt::Debug + Send + Sync + Error + 'static,
{
    let nonce = evm
        .db_mut()
        .basic(ctx.builder_private_key.address())?
        .unwrap_or_default()
        .nonce;

    let mut tx = OpTransactionRequest::default()
        .nonce(nonce)
        .gas_limit(dyn_gas_limit(nullifier_hashes.len() as u64))
        .max_priority_fee_per_gas(evm.ctx().block.basefee.into())
        .max_fee_per_gas(evm.ctx().block.basefee.into())
        .with_chain_id(evm.ctx().cfg.chain_id)
        .with_call(&spendNullifierHashesCall {
            _nullifierHashes: nullifier_hashes.into_iter().collect(),
        })
        .to(ctx.pbh_entry_point)
        .build_typed_tx()
        .map_err(|e| eyre!("{:?}", e))?;

    let signature = ctx.builder_private_key.sign_transaction_sync(&mut tx)?;
    let signed: OpTransactionSigned = tx.into_signed(signature).into();
    Ok(signed.try_into_recovered_unchecked()?)
}
