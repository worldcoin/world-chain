use crate::context::WorldChainPayloadBuilderCtx;
use alloy_rpc_types_debug::ExecutionWitness;
use alloy_signer_local::PrivateKeySigner;
use flashblocks_builder::traits::context::PayloadBuilderCtx;
use reth::{
    api::PayloadBuilderError,
    payload::PayloadBuilderAttributes,
    revm::{State, database::StateProviderDatabase, witness::ExecutionWitnessRecord},
    transaction_pool::{BestTransactionsAttributes, TransactionPool},
};
use reth_basic_payload_builder::{
    BuildArguments, BuildOutcome, BuildOutcomeKind, MissingPayloadBehaviour, PayloadBuilder,
    PayloadConfig,
};
use reth_chain_state::ExecutedBlock;
use reth_evm::{
    Database, Evm,
    execute::{BlockBuilder, BlockBuilderOutcome, BlockExecutor},
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{
    OpBuiltPayload, OpEvmConfig, OpPayloadBuilder, OpPayloadBuilderAttributes,
};
use reth_optimism_payload_builder::{
    OpPayloadAttributes,
    builder::{OpPayloadBuilderCtx, OpPayloadTransactions},
    config::OpBuilderConfig,
};
use reth_optimism_primitives::{OpPrimitives, OpTransactionSigned};
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_primitives::{Block, SealedHeader};
use reth_provider::{
    BlockReaderIdExt, ChainSpecProvider, ExecutionOutcome, ProviderError, StateProvider,
    StateProviderFactory,
};
use reth_transaction_pool::BlobStore;
use revm_primitives::Address;
use std::sync::Arc;
use tracing::debug;
use world_chain_pool::{WorldChainTransactionPool, tx::WorldChainPooledTransaction};

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
        builder_private_key: PrivateKeySigner,
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
        builder_private_key: PrivateKeySigner,
    ) -> Self {
        let inner = OpPayloadBuilder::with_builder_config(pool, client, evm_config, config)
            .set_compute_pending_block(compute_pending_block);

        Self {
            inner,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            builder_private_key,
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

        WorldChainPayloadBuilder {
            inner: inner.with_transactions(best_transactions),
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
                builder_config: self.inner.config.clone(),
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
        };

        let op_ctx = &ctx.inner;
        let builder = WorldChainBuilder::new(best);
        let state_provider = self
            .inner
            .client
            .state_by_block_hash(op_ctx.parent().hash())?;
        let state = StateProviderDatabase::new(&state_provider);

        if op_ctx.attributes().no_tx_pool {
            builder.build(self.inner.pool.clone(), state, &state_provider, ctx)
        } else {
            // sequencer mode we can reuse cachedreads from previous runs
            builder.build(
                self.inner.pool.clone(),
                cached_reads.as_db_mut(state),
                &state_provider,
                ctx,
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
            inner: Arc::new(OpPayloadBuilderCtx {
                evm_config: self.inner.evm_config.clone(),
                builder_config: self.inner.config.clone(),
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
        };

        let state_provider = self
            .inner
            .client
            .state_by_block_hash(ctx.inner.parent().hash())?;

        let builder: WorldChainBuilder<'_, NoopPayloadTransactions<WorldChainPooledTransaction>> =
            WorldChainBuilder::new(|_| NoopPayloadTransactions::default());

        builder.witness(self.inner.pool.clone(), state_provider, &ctx)
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
    pub fn build<Client, Pool>(
        self,
        pool: Pool,
        db: impl Database<Error = ProviderError>,
        state_provider: impl StateProvider,
        ctx: WorldChainPayloadBuilderCtx<Client>,
    ) -> Result<BuildOutcomeKind<OpBuiltPayload<OpPrimitives>>, PayloadBuilderError>
    where
        Pool: TransactionPool,
        Txs: PayloadTransactions<Transaction = WorldChainPooledTransaction>,
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

        let gas_limit = ctx.attributes().gas_limit.unwrap_or(ctx.parent().gas_limit);

        // 1. apply pre-execution changes
        builder.apply_pre_execution_changes()?;

        // 2. execute sequencer transactions
        let mut info = op_ctx.execute_sequencer_transactions(&mut builder)?;

        // 3. if mem pool transactions are requested we execute them
        if !op_ctx.attributes().no_tx_pool {
            let best_txs = best(op_ctx.best_transaction_attributes(builder.evm_mut().block()));
            // TODO: Validate gas limit
            if ctx
                .execute_best_transactions(pool, &mut info, &mut builder, best_txs, gas_limit)?
                .is_none()
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
        let executed = ExecutedBlock {
            recovered_block: Arc::new(block),
            execution_output: Arc::new(execution_outcome),
            hashed_state: Arc::new(hashed_state),
            trie_updates: Arc::new(trie_updates),
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
    pub fn witness<Client, Pool>(
        self,
        pool: Pool,
        state_provider: impl StateProvider,
        ctx: &WorldChainPayloadBuilderCtx<Client>,
    ) -> Result<ExecutionWitness, PayloadBuilderError>
    where
        Pool: TransactionPool,
        Txs: PayloadTransactions<Transaction = WorldChainPooledTransaction>,
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
            ctx.execute_best_transactions(pool, &mut info, &mut builder, best_txs, 0)?;
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
