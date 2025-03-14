use alloy_consensus::Transaction;
use alloy_eips::Typed2718;
use alloy_rlp::Encodable;
use alloy_rpc_types_debug::ExecutionWitness;
use reth::api::PayloadBuilderError;
use reth::payload::PayloadBuilderAttributes;
use reth::revm::database::StateProviderDatabase;
use reth::revm::witness::ExecutionWitnessRecord;
use reth::revm::{DatabaseCommit, State};
use reth::transaction_pool::{BestTransactionsAttributes, TransactionPool};
use reth_basic_payload_builder::{
    BuildArguments, BuildOutcome, BuildOutcomeKind, MissingPayloadBehaviour, PayloadBuilder,
    PayloadConfig,
};
use reth_chain_state::{ExecutedBlock, ExecutedBlockWithTrieUpdates};
use reth_evm::execute::BlockBuilderOutcome;
use reth_evm::execute::BlockExecutionError;
use reth_evm::execute::BlockValidationError;
use reth_evm::execute::InternalBlockExecutionError;
use reth_evm::execute::{BlockBuilder, BlockExecutor};
use reth_evm::Evm;
use reth_evm::{ConfigureEvm, Database};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::{
    OpBuiltPayload, OpEvm, OpEvmConfig, OpNextBlockEnvAttributes, OpPayloadBuilder,
    OpPayloadBuilderAttributes,
};
use reth_optimism_payload_builder::builder::{
    ExecutionInfo, OpPayloadBuilderCtx, OpPayloadTransactions,
};
use reth_optimism_payload_builder::config::OpBuilderConfig;
use reth_optimism_payload_builder::OpPayloadAttributes;
use reth_optimism_primitives::{OpPrimitives, OpTransactionSigned};
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_primitives::{Block, Recovered, SealedHeader};
use reth_primitives_traits::SignedTransaction;
use reth_provider::{
    BlockReaderIdExt, ChainSpecProvider, ExecutionOutcome, ProviderError, StateProvider,
    StateProviderFactory,
};
use reth_transaction_pool::{BlobStore, PoolTransaction};
use revm_primitives::{Address, U256};
use std::sync::Arc;
use tracing::{debug, error, trace};
use world_chain_builder_pool::tx::{WorldChainPoolTransaction, WorldChainPooledTransaction};
use world_chain_builder_pool::WorldChainTransactionPool;
use world_chain_builder_rpc::transactions::validate_conditional_options;

use crate::inspector::{PBHCallTracer, PBH_CALL_TRACER_ERROR};

/// World Chain payload builder
#[derive(Debug, Clone)]
pub struct WorldChainPayloadBuilder<Client, S, Txs = ()>
where
    Client: StateProviderFactory + BlockReaderIdExt<Block = Block<OpTransactionSigned>>,
{
    pub inner: OpPayloadBuilder<WorldChainTransactionPool<Client, S>, Client, OpEvmConfig, Txs>,
    pub verified_blockspace_capacity: u8,
    pub pbh_entry_point: Address,
    pub pbh_signature_aggregator: Address,
    pub builder_private_key: String,
    pub block_registry: Address,
}

impl<Client, S> WorldChainPayloadBuilder<Client, S>
where
    Client: StateProviderFactory + BlockReaderIdExt<Block = Block<OpTransactionSigned>>,
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
        block_registry: Address,
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
            block_registry,
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
        block_registry: Address,
    ) -> Self {
        let inner = OpPayloadBuilder::with_builder_config(pool, client, evm_config, config)
            .set_compute_pending_block(compute_pending_block);

        Self {
            inner,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            builder_private_key,
            block_registry,
        }
    }
}

impl<Client, S> WorldChainPayloadBuilder<Client, S>
where
    Client: StateProviderFactory + BlockReaderIdExt<Block = Block<OpTransactionSigned>>,
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
            block_registry,
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
            block_registry,
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
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + Clone
        + 'static,

    S: BlobStore + Clone,
{
    /// Constructs an Optimism payload from the transactions sent via the
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
        Txs: PayloadTransactions<
            Transaction: WorldChainPoolTransaction<Consensus = OpTransactionSigned>,
        >,
    {
        let BuildArguments {
            mut cached_reads,
            config,
            cancel,
            best_payload,
        } = args;

        let ctx = WorldChainPayloadBuilderCtx {
            inner: OpPayloadBuilderCtx {
                evm_config: self.inner.evm_config.clone(),
                da_config: self.inner.config.da_config.clone(),
                chain_spec: self.inner.client.chain_spec(),
                config,
                cancel,
                best_payload,
            },
            client: self.inner.client.clone(),
            verified_blockspace_capacity: self.verified_blockspace_capacity,
            pbh_entry_point: self.pbh_entry_point,
            pbh_signature_aggregator: self.pbh_signature_aggregator,
            builder_private_key: self.builder_private_key.clone(),
            block_registry: self.block_registry,
        };

        let op_ctx = &ctx.inner;
        let builder = WorldChainBuilder::new(best);
        let state_provider = self
            .inner
            .client
            .state_by_block_hash(op_ctx.parent().hash())?;
        let state = StateProviderDatabase::new(&state_provider);

        if op_ctx.attributes().no_tx_pool {
            builder.build(state, &state_provider, ctx, &self.inner.pool)
        } else {
            // sequencer mode we can reuse cachedreads from previous runs
            builder.build(
                cached_reads.as_db_mut(state),
                &state_provider,
                ctx,
                &self.inner.pool,
            )
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
            inner: OpPayloadBuilderCtx {
                evm_config: self.inner.evm_config.clone(),
                da_config: self.inner.config.da_config.clone(),
                chain_spec: self.inner.client.chain_spec(),
                config,
                cancel: Default::default(),
                best_payload: Default::default(),
            },
            client,
            verified_blockspace_capacity: self.verified_blockspace_capacity,
            pbh_entry_point: self.pbh_entry_point,
            pbh_signature_aggregator: self.pbh_signature_aggregator,
            builder_private_key: self.builder_private_key.clone(),
            block_registry: self.block_registry,
        };

        let state_provider = self
            .inner
            .client
            .state_by_block_hash(ctx.inner.parent().hash())?;

        let builder: WorldChainBuilder<'_, NoopPayloadTransactions<WorldChainPooledTransaction>> =
            WorldChainBuilder::new(|_| NoopPayloadTransactions::default());

        builder.witness(state_provider, &ctx, &self.inner.pool)
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
        ctx: WorldChainPayloadBuilderCtx<Client>,
        pool: &Pool,
    ) -> Result<BuildOutcomeKind<OpBuiltPayload<OpPrimitives>>, PayloadBuilderError>
    where
        Txs: PayloadTransactions<
            Transaction: WorldChainPoolTransaction<Consensus = OpTransactionSigned>,
        >,
        Pool: TransactionPool<
            Transaction: WorldChainPoolTransaction<Consensus = OpTransactionSigned>,
        >,
        Client: StateProviderFactory
            + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
            + ChainSpecProvider<ChainSpec = OpChainSpec>
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
        let mut builder = ctx.block_builder(&mut state)?;

        // 1. apply pre-execution changes
        builder.apply_pre_execution_changes()?;

        // 2. execute sequencer transactions
        let mut info = op_ctx.execute_sequencer_transactions(&mut builder)?;

        // 3. if mem pool transactions are requested we execute them
        if !op_ctx.attributes().no_tx_pool {
            let best_txs = best(op_ctx.best_transaction_attributes(builder.evm_mut().block()));
            if ctx
                .execute_best_transactions(&mut info, &mut builder, best_txs, pool)?
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
            trie: Arc::new(trie_updates),
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
        ctx: &WorldChainPayloadBuilderCtx<Client>,
        pool: &Pool,
    ) -> Result<ExecutionWitness, PayloadBuilderError>
    where
        Txs: PayloadTransactions<
            Transaction: WorldChainPoolTransaction<Consensus = OpTransactionSigned>,
        >,
        Pool: TransactionPool<
            Transaction: WorldChainPoolTransaction<Consensus = OpTransactionSigned>,
        >,
        Client: StateProviderFactory
            + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
            + ChainSpecProvider<ChainSpec = OpChainSpec>
            + Clone,
    {
        let Self { best } = self;

        let mut db = State::builder()
            .with_database(StateProviderDatabase::new(&state_provider))
            .with_bundle_update()
            .build();
        let mut builder = ctx.block_builder(&mut db)?;

        builder.apply_pre_execution_changes()?;
        let mut info = ctx.inner.execute_sequencer_transactions(&mut builder)?;
        if !ctx.inner.attributes().no_tx_pool {
            let best_txs = best(
                ctx.inner
                    .best_transaction_attributes(builder.evm_mut().block()),
            );
            ctx.execute_best_transactions(&mut info, &mut builder, best_txs, pool)?;
        }
        builder.into_executor().apply_post_execution_changes()?;

        let ExecutionWitnessRecord {
            hashed_state,
            codes,
            keys,
        } = ExecutionWitnessRecord::from_executed_state(&db);
        let state = state_provider.witness(Default::default(), hashed_state)?;
        Ok(ExecutionWitness {
            state: state.into_iter().collect(),
            codes,
            keys,
        })
    }
}

/// Container type that holds all necessities to build a new payload.
#[derive(Debug)]
pub struct WorldChainPayloadBuilderCtx<Client> {
    pub inner: OpPayloadBuilderCtx<OpEvmConfig, OpChainSpec>,
    pub verified_blockspace_capacity: u8,
    pub pbh_entry_point: Address,
    pub pbh_signature_aggregator: Address,
    pub client: Client,
    pub builder_private_key: String,
    pub block_registry: Address,
}

impl<Client> WorldChainPayloadBuilderCtx<Client>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Block = Block<OpTransactionSigned>>
        + ChainSpecProvider<ChainSpec = OpChainSpec>
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
            Executor: BlockExecutor<Evm = OpEvm<DB, PBHCallTracer>>,
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

        // Enable inspector for execution of the block transactions
        builder.evm_mut().inspector_mut().active = true;

        // TODO: perhaps we don't want to error out here.
        let (builder_addr, stamp_block_tx) = crate::stamp::stamp_block_tx(self, builder.evm_mut())
            .map_err(|e| PayloadBuilderError::Other(e.into()))?;

        let mut invalid_txs = vec![];
        let verified_gas_limit = (self.verified_blockspace_capacity as u64 * block_gas_limit) / 100;

        // Subtract stamp_block_tx gas limit from the overall block gas limit.
        // This gas will be saved to execute stampBlock at the end of the block.
        block_gas_limit -= stamp_block_tx.gas_limit();

        while let Some(pooled_tx) = best_txs.next(()) {
            let tx = pooled_tx.clone().into_consensus();
            if info.is_tx_over_limits(tx.inner(), block_gas_limit, tx_da_limit, block_da_limit) {
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

            // If the transaction is verified, check if it can be added within the verified gas limit
            if pooled_tx.valid_pbh()
                && info.cumulative_gas_used + tx.gas_limit() > verified_gas_limit
            {
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
            }

            // ensure we still have capacity for this transaction
            if info.cumulative_gas_used + tx.gas_limit() > block_gas_limit {
                // we can't fit this transaction into the block, so we need to mark it as
                // invalid which also removes all dependent transaction from
                // the iterator before we can continue
                best_txs.mark_invalid(tx.signer(), tx.nonce());
                continue;
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

            let gas_used = match builder.execute_transaction(tx.clone()) {
                Ok(res) => res,
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
                        BlockExecutionError::Internal(InternalBlockExecutionError::EVM {
                            error,
                            ..
                        }) if error.to_string() == PBH_CALL_TRACER_ERROR => {
                            trace!(target: "payload_builder", %error, ?tx, "skipping invalid transaction and its descendants");
                            best_txs.mark_invalid(tx.signer(), tx.nonce());
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

        // execute the stamp block transaction
        let op_tx_signed: OpTransactionSigned = stamp_block_tx.into();
        let op_tx_signed = op_tx_signed.with_signer(builder_addr);

        let gas_used = builder
            .execute_transaction(op_tx_signed.clone())
            .map_err(|e| {
                error!(target: "payload_builder", %e, "failed to stamp block transaction");
                PayloadBuilderError::evm(e)
            })?;

        self.commit_changes(info, base_fee, gas_used, op_tx_signed);

        if !invalid_txs.is_empty() {
            pool.remove_transactions(invalid_txs);
        }

        // Disable inspector
        builder.evm_mut().inspector_mut().active = false;

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
            Executor: BlockExecutor<Evm = OpEvm<&'a mut State<DB>, PBHCallTracer>>,
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

        let mut pbh_call_tracer =
            PBHCallTracer::new(self.pbh_entry_point, self.pbh_signature_aggregator);
        // disable the tracer by default
        pbh_call_tracer.active = false;

        // Prepare EVM.
        let evm = self
            .inner
            .evm_config
            .evm_with_env_and_inspector(db, evm_env, pbh_call_tracer);

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
