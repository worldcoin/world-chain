use crate::{
    BlockBuilderExt, FlashblocksPayloadBuilderConfig,
    access_list::FlashblockAccessListConstruction,
    bal_executor::CommittedState,
    executor::FlashblocksBlockBuilder,
    payload_txns::BestPayloadTxns,
    traits::{
        context::PayloadBuilderCtx, context_builder::PayloadBuilderCtxBuilder,
        payload_builder::FlashblockPayloadBuilder,
    },
};
use alloy_primitives::TxHash;
use reth_evm::{Evm, EvmFactory, execute::BlockBuilder};

use alloy_consensus::{BlockHeader, Header};

use alloy_op_evm::{
    OpBlockExecutionCtx, OpBlockExecutor, OpEvm, OpEvmFactory,
    block::receipt_builder::OpReceiptBuilder,
};
use flashblocks_primitives::access_list::FlashblockAccessList;
use op_alloy_consensus::OpTxEnvelope;
use reth::{
    api::{PayloadBuilderAttributes, PayloadBuilderError},
    chainspec::EthChainSpec,
    revm::{State, database::StateProviderDatabase},
};
use reth_basic_payload_builder::{
    BuildArguments, BuildOutcome, BuildOutcomeKind, MissingPayloadBehaviour, PayloadBuilder,
    PayloadConfig,
};
use reth_evm::{
    ConfigureEvm, Database, EvmEnv, execute::BlockBuilderOutcome, op_revm::OpSpecId,
    precompiles::PrecompilesMap,
};
use reth_node_api::BuiltPayloadExecutedBlock;
use tracing::{info, warn};

use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{
    OpEvmConfig, OpNextBlockEnvAttributes, OpRethReceiptBuilder, txpool::OpPooledTx,
};
use reth_optimism_payload_builder::{
    builder::{ExecutionInfo, OpPayloadTransactions},
    payload::{OpBuiltPayload, OpPayloadBuilderAttributes},
};
use reth_optimism_primitives::{OpPrimitives, OpReceipt, OpTransactionSigned};
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_provider::{
    ChainSpecProvider, ExecutionOutcome, ProviderError, StateProvider, StateProviderFactory,
};

use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use revm::inspector::NoOpInspector;
use std::{fmt::Debug, sync::Arc};
use tracing::{debug, span};

/// Flashblocks Paylod builder
///
/// A payload builder
#[derive(Debug, Clone)]
pub struct FlashblocksPayloadBuilder<Pool, Client, CtxBuilder, Txs = ()> {
    /// The type responsible for creating the evm.
    pub evm_config: OpEvmConfig,
    /// Transaction pool.
    pub pool: Pool,
    /// Node client.
    pub client: Client,
    /// Settings for the builder, e.g. DA settings.
    pub config: FlashblocksPayloadBuilderConfig,
    /// Iterator over best transactions from the pool.
    pub best_transactions: Txs,
    /// Context builder for the payload.
    pub ctx_builder: CtxBuilder,
}

impl<Pool, Client, CtxBuilder, Txs> FlashblocksPayloadBuilder<Pool, Client, CtxBuilder, Txs>
where
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec = OpChainSpec> + Clone,
    Txs: OpPayloadTransactions<Pool::Transaction>,
    Pool: TransactionPool<Transaction: OpPooledTx<Consensus = OpTxEnvelope>>,
    CtxBuilder: PayloadBuilderCtxBuilder<
            Client,
            OpEvmConfig,
            OpChainSpec,
            PayloadBuilderCtx: PayloadBuilderCtx<Transaction = Pool::Transaction>,
        >,
{
    /// Constructs an Optimism payload from the transactions sent via the
    /// Payload attributes by the sequencer. If the `no_tx_pool` argument is passed in
    /// the payload attributes, the transaction pool will be ignored and the only transactions
    /// included in the payload will be those sent through the attributes.
    ///
    /// Given build arguments including an Optimism client, transaction pool,
    /// and configuration, this function creates a transaction payload. Returns
    /// a result indicating success with the payload or an error in case of failure.    
    fn build_payload<'a, T>(
        &self,
        args: BuildArguments<OpPayloadBuilderAttributes<OpTxEnvelope>, OpBuiltPayload>,
        best: impl Fn(BestTransactionsAttributes) -> T + Send + Sync + 'a,
        committed_payload: Option<&OpBuiltPayload>,
    ) -> Result<(BuildOutcome<OpBuiltPayload>, Option<FlashblockAccessList>), PayloadBuilderError>
    where
        T: PayloadTransactions<Transaction = <Pool as TransactionPool>::Transaction>,
    {
        let BuildArguments {
            config,
            mut cached_reads,
            cancel,
            best_payload,
        } = args;

        let ctx = self.ctx_builder.build(
            self.client.clone(),
            self.evm_config.clone(),
            self.config.inner.clone(),
            config,
            &cancel,
            best_payload.clone(),
        );

        let state_provider = Arc::new(self.client.state_by_block_hash(ctx.parent().hash())?);
        let db = StateProviderDatabase::new(&state_provider);

        let database = cached_reads.as_db_mut(db);

        build(
            best,
            Some(self.pool.clone()),
            database,
            state_provider.clone(),
            &ctx,
            committed_payload,
            self.config.bal_enabled,
        )
        .map(|(out, access_list)| (out.with_cached_reads(cached_reads), access_list))
    }
}

impl<Pool, Client, CtxBuilder, Txs> PayloadBuilder
    for FlashblocksPayloadBuilder<Pool, Client, CtxBuilder, Txs>
where
    Client: Clone + StateProviderFactory + ChainSpecProvider<ChainSpec = OpChainSpec>,
    Pool: TransactionPool<Transaction: OpPooledTx<Consensus = OpTxEnvelope>>,
    Txs: OpPayloadTransactions<Pool::Transaction>,
    CtxBuilder: PayloadBuilderCtxBuilder<
            Client,
            OpEvmConfig,
            OpChainSpec,
            PayloadBuilderCtx: PayloadBuilderCtx<Transaction = Pool::Transaction>,
        >,
{
    type Attributes = OpPayloadBuilderAttributes<OpTxEnvelope>;
    type BuiltPayload = OpBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        self.build_payload(
            args,
            |attrs| {
                self.best_transactions
                    .best_transactions(self.pool.clone(), attrs)
            },
            None,
        )
        .map(|(outcome, _access_list)| outcome)
    }

    fn on_missing_payload(
        &self,
        _args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> MissingPayloadBehaviour<Self::BuiltPayload> {
        // we want to await the job that's already in progress because that should be returned as
        // is, there's no benefit in racing another job
        MissingPayloadBehaviour::AwaitInProgress
    }

    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes, Header>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        let args = BuildArguments {
            config,
            cached_reads: Default::default(),
            cancel: Default::default(),
            best_payload: None,
        };
        self.build_payload(
            args,
            |_| NoopPayloadTransactions::<Pool::Transaction>::default(),
            None,
        )?
        .0
        .into_payload()
        .ok_or_else(|| PayloadBuilderError::MissingPayload)
    }
}

impl<Pool, Client, CtxBuilder, Txs> FlashblockPayloadBuilder
    for FlashblocksPayloadBuilder<Pool, Client, CtxBuilder, Txs>
where
    Client: Clone + StateProviderFactory + ChainSpecProvider<ChainSpec = OpChainSpec>,
    Pool: TransactionPool<Transaction: OpPooledTx<Consensus = OpTxEnvelope>>,
    Txs: OpPayloadTransactions<Pool::Transaction>,
    CtxBuilder: PayloadBuilderCtxBuilder<
            Client,
            OpEvmConfig,
            OpChainSpec,
            PayloadBuilderCtx: PayloadBuilderCtx<Transaction = Pool::Transaction>,
        >,
{
    fn try_build_with_precommit(
        &self,
        args: BuildArguments<
            <Self as PayloadBuilder>::Attributes,
            <Self as PayloadBuilder>::BuiltPayload,
        >,
        committed_payload: Option<&<Self as PayloadBuilder>::BuiltPayload>,
    ) -> Result<
        (
            BuildOutcome<<Self as PayloadBuilder>::BuiltPayload>,
            Option<FlashblockAccessList>,
        ),
        PayloadBuilderError,
    > {
        self.build_payload(
            args,
            |attrs| {
                self.best_transactions
                    .best_transactions(self.pool.clone(), attrs)
            },
            committed_payload,
        )
    }
}

/// Builds the payload on top of the state.
///
/// This function uses a unified code path for both BAL-enabled and non-BAL builds.
/// When `bal_enabled` is true, the State is configured with a bal_builder and the
/// FlashblocksBlockBuilder is configured with BAL index tracking.
pub fn build<'a, Txs, Ctx, Pool>(
    best: impl Fn(BestTransactionsAttributes) -> Txs + Send + Sync + 'a,
    pool: Option<Pool>,
    db: impl Database<Error = ProviderError> + Send + Sync,
    state_provider: impl StateProvider + Clone + 'static,
    ctx: &Ctx,
    committed_payload: Option<&OpBuiltPayload>,
    bal_enabled: bool,
) -> Result<
    (
        BuildOutcomeKind<OpBuiltPayload>,
        Option<FlashblockAccessList>,
    ),
    PayloadBuilderError,
>
where
    Pool: TransactionPool,
    Txs: PayloadTransactions,
    Txs::Transaction: OpPooledTx,
    Ctx: PayloadBuilderCtx<
            Evm = OpEvmConfig,
            Transaction = Txs::Transaction,
            ChainSpec = OpChainSpec,
        >,
{
    let span = span!(
        tracing::Level::INFO,
        "flashblock_builder",
        id = %ctx.payload_id(),
    );

    let _enter = span.enter();

    debug!(target: "flashblocks::payload_builder", "building new payload");

    let attributes = OpNextBlockEnvAttributes {
        timestamp: ctx.attributes().timestamp(),
        suggested_fee_recipient: ctx.attributes().suggested_fee_recipient(),
        prev_randao: ctx.attributes().prev_randao(),
        gas_limit: ctx.attributes().gas_limit.unwrap_or(ctx.parent().gas_limit),
        parent_beacon_block_root: ctx.attributes().parent_beacon_block_root(),
        extra_data: if ctx
            .spec()
            .is_holocene_active_at_timestamp(ctx.attributes().timestamp())
        {
            ctx.attributes()
                .get_holocene_extra_data(
                    ctx.spec()
                        .base_fee_params_at_timestamp(ctx.attributes().timestamp()),
                )
                .map_err(PayloadBuilderError::other)?
        } else {
            Default::default()
        },
    };

    info!(target: "flashblocks::payload_builder", ?attributes, "prepared next block attributes");

    // Prepare EVM environment.
    let evm_env = ctx
        .evm_config()
        .next_evm_env(ctx.parent(), &attributes)
        .map_err(PayloadBuilderError::other)?;

    let execution_context = ctx
        .evm_config()
        .context_for_next_block(ctx.parent(), attributes)
        .map_err(PayloadBuilderError::other)?;

    let committed_state = CommittedState::<OpRethReceiptBuilder>::try_from(committed_payload)
        .map_err(PayloadBuilderError::other)?;

    let gas_limit = ctx
        .attributes()
        .gas_limit
        .unwrap_or(ctx.parent().gas_limit)
        .saturating_sub(committed_state.gas_used);

    let bundle_state = committed_state.bundle.clone();

    let visited_transactions = committed_state
        .transaction_hashes_iter()
        .collect::<Vec<_>>();

    // Calculate the start index for BAL tracking
    // When building on top of committed transactions, start after them
    let start_index = if committed_state.transactions.is_empty() {
        0u64
    } else {
        committed_state.transactions.len() as u64 + 1
    };

    // Build State with optional bal_builder
    let mut state_builder = State::builder()
        .with_database(db)
        .with_bundle_prestate(bundle_state)
        .with_bundle_update();

    if bal_enabled {
        state_builder = state_builder.with_bal_builder();
    }

    let mut state = state_builder.build();

    // Set initial BAL index if enabled
    if bal_enabled {
        state.set_bal_index(start_index);
    }

    // Create the unified block builder
    let builder = flashblocks_block_builder(
        &mut state,
        execution_context,
        evm_env,
        &committed_state,
        ctx,
        bal_enabled,
    )?;

    build_inner(
        committed_payload,
        visited_transactions,
        gas_limit,
        best,
        pool,
        state_provider,
        ctx,
        builder,
        &committed_state,
        bal_enabled,
    )
}

fn build_inner<'a, Txs, Ctx, Pool, R, DB>(
    committed_payload: Option<&OpBuiltPayload>,
    visited_transactions: Vec<TxHash>,
    gas_limit: u64,
    best: impl Fn(BestTransactionsAttributes) -> Txs + Send + Sync + 'a,
    pool: Option<Pool>,
    state_provider: impl StateProvider + Clone + 'static,
    ctx: &Ctx,
    mut builder: FlashblocksBlockBuilder<
        'a,
        OpPrimitives,
        OpEvm<&'a mut State<DB>, NoOpInspector, PrecompilesMap>,
        R,
    >,
    committed_state: &CommittedState<R>,
    bal_enabled: bool,
) -> Result<
    (
        BuildOutcomeKind<OpBuiltPayload>,
        Option<FlashblockAccessList>,
    ),
    PayloadBuilderError,
>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt> + Default + 'static,
    Pool: TransactionPool,
    Txs: PayloadTransactions,
    Txs::Transaction: OpPooledTx,
    DB: Database + 'a,
    Ctx: PayloadBuilderCtx<
            Evm = OpEvmConfig,
            Transaction = Txs::Transaction,
            ChainSpec = OpChainSpec,
        >,
{
    // Only execute the sequencer transactions on the first payload. The sequencer transactions
    // will already be in the [`BundleState`] at this point if the `best_payload` is set.
    let mut info = if committed_payload.is_none() {
        // 3. apply pre-execution changes
        builder.apply_pre_execution_changes()?;

        // 4. Execute Deposit transactions
        ctx.execute_sequencer_transactions(&mut builder)
            .map_err(PayloadBuilderError::other)?
    } else {
        committed_payload.map_or(ExecutionInfo::default(), |p| ExecutionInfo {
            total_fees: p.fees(),
            cumulative_gas_used: p.block().gas_used(),
            ..Default::default()
        })
    };

    // 5. Execute transactions from the tx-pool, draining any transactions seen in previous
    // flashblocks
    if let Some(pool) = pool
        && !ctx.attributes().no_tx_pool
    {
        let best_txs = best(ctx.best_transaction_attributes(builder.evm_mut().block()));
        let mut best_txns = BestPayloadTxns::new(best_txs).with_prev(visited_transactions);

        if ctx
            .execute_best_transactions(pool, &mut info, &mut builder, best_txns.guard(), gas_limit)?
            .is_some()
        {
            warn!(target: "flashblocks::payload_builder", "payload build cancelled");
            if let Some(best_payload) = committed_payload {
                // we can return the previous best payload since we didn't include any new txs
                return Ok((BuildOutcomeKind::Freeze(best_payload.clone()), None));
            } else {
                return Err(PayloadBuilderError::MissingPayload);
            }
        }

        // check if the new payload is even more valuable
        if !ctx.is_better_payload(info.total_fees) {
            // can skip building the block
            return Ok((
                BuildOutcomeKind::Aborted {
                    fees: info.total_fees,
                },
                None,
            ));
        }
    }

    // Get the index range from the counter before consuming the builder
    let index_range = builder
        .counter
        .as_ref()
        .map(|c| (c.start_index, c.current_index));

    // 6. Build the block and get Bal if enabled
    let (build_outcome, bundle, bal) = builder.finish_with_bundle_and_bal(&state_provider)?;

    // 7. Seal the block
    let BlockBuilderOutcome {
        execution_result,
        block,
        hashed_state,
        trie_updates,
    } = build_outcome;

    let sealed_block = Arc::new(block.sealed_block().clone());

    let execution_outcome = ExecutionOutcome::new(
        bundle,
        vec![execution_result.receipts.clone()],
        block.number(),
        Vec::new(),
    );

    // create the executed block data
    let executed_block: BuiltPayloadExecutedBlock<OpPrimitives> = BuiltPayloadExecutedBlock {
        recovered_block: Arc::new(block),
        execution_output: Arc::new(execution_outcome.clone()),
        hashed_state: either::Left(Arc::new(hashed_state)),
        trie_updates: either::Left(Arc::new(trie_updates)),
    };

    let payload = OpBuiltPayload::new(
        ctx.payload_id(),
        sealed_block,
        info.total_fees + committed_state.fees,
        Some(executed_block),
    );

    // Convert revm's Bal to FlashblockAccessList if BAL was enabled
    let access_list = if bal_enabled {
        let (min_tx_index, max_tx_index) = index_range.unwrap_or((0, 0));
        bal.map(FlashblockAccessListConstruction::from)
            .map(|bal| bal.build((min_tx_index, max_tx_index)))
    } else {
        None
    };

    if ctx.attributes().no_tx_pool {
        // if `no_tx_pool` is set only transactions from the payload attributes will be included
        // in the payload. In other words, the payload is deterministic and we can
        // freeze it once we've successfully built it.
        Ok((BuildOutcomeKind::Freeze(payload), access_list))
    } else {
        // always better since we are re-using built payloads
        Ok((BuildOutcomeKind::Better { payload }, access_list))
    }
}

/// Creates a unified block builder for flashblocks.
///
/// When `bal_enabled` is true, the builder is configured with BAL index tracking
/// starting from `start_index`.
pub fn flashblocks_block_builder<'a, Ctx, DB, Tx>(
    state: &'a mut State<DB>,
    execution_context: OpBlockExecutionCtx,
    evm_env: EvmEnv<OpSpecId>,
    committed_state: &CommittedState<OpRethReceiptBuilder>,
    ctx: &'a Ctx,
    bal_enabled: bool,
) -> Result<
    FlashblocksBlockBuilder<
        'a,
        OpPrimitives,
        OpEvm<&'a mut State<DB>, NoOpInspector, PrecompilesMap>,
        OpRethReceiptBuilder,
    >,
    PayloadBuilderError,
>
where
    Tx: PoolTransaction + OpPooledTx,
    DB: Database + 'a,
    Ctx: PayloadBuilderCtx<Evm = OpEvmConfig, Transaction = Tx, ChainSpec = OpChainSpec>,
{
    let evm = OpEvmFactory::default().create_evm(state, evm_env);

    let mut executor = OpBlockExecutor::new(
        evm,
        execution_context.clone(),
        ctx.spec().clone(),
        OpRethReceiptBuilder::default(),
    );

    executor.gas_used = committed_state.gas_used;
    executor.receipts = committed_state.receipts_iter().cloned().collect();

    let mut builder = FlashblocksBlockBuilder::new(
        execution_context,
        ctx.parent(),
        executor,
        committed_state.transactions_iter().cloned().collect(),
        ctx.spec().clone().into(),
    );

    let min_bal_index = if committed_state.transactions.is_empty() {
        0u64
    } else {
        committed_state.transactions.len() as u64 + 1
    };

    // Configure BAL index tracking if enabled
    if bal_enabled {
        builder = builder.with_bal_index(min_bal_index);
    }

    Ok(builder)
}
