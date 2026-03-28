use crate::{
    BlockBuilderExt,
    bal_executor::{BalBlockBuilder, CommittedState},
    database::bal_builder_db::BalBuilderDb,
    executor::FlashblocksBlockBuilder,
    metrics::{
        PayloadBuildAttemptMetrics, PayloadBuildMetrics, PayloadBuildOutcome, PayloadBuildStage,
    },
    payload_txns::BestPayloadTxns,
    traits::{
        context::PayloadBuilderCtx, context_builder::PayloadBuilderCtxBuilder,
        payload_builder::FlashblockPayloadBuilder,
    },
};
use alloy_eips::Encodable2718;
use alloy_primitives::TxHash;
use reth_evm::{
    Evm, EvmFactory,
    block::{BlockExecutor, BlockExecutorFactory, StateDB},
};

use alloy_consensus::{BlockHeader, Header};

use alloy_op_evm::{
    OpBlockExecutionCtx, OpBlockExecutor, OpBlockExecutorFactory, OpEvm, OpEvmFactory,
    block::receipt_builder::OpReceiptBuilder,
};
use op_alloy_consensus::OpTxEnvelope;

use reth_basic_payload_builder::{
    BuildArguments, BuildOutcome, BuildOutcomeKind, MissingPayloadBehaviour, PayloadBuilder,
    PayloadConfig,
};
use reth_evm::{
    ConfigureEvm, Database, EvmEnv, execute::BlockBuilderOutcome, op_revm::OpSpecId,
    precompiles::PrecompilesMap,
};
use reth_node_api::{BuiltPayloadExecutedBlock, PayloadBuilderAttributes, PayloadBuilderError};
use reth_primitives::NodePrimitives;
use reth_revm::database::StateProviderDatabase;
use revm_database::State;
use tracing::trace;
use world_chain_primitives::access_list::FlashblockAccessList;

use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{
    OpEvmConfig, OpNextBlockEnvAttributes, OpRethReceiptBuilder, txpool::OpPooledTx,
};
use reth_optimism_payload_builder::{
    builder::{ExecutionInfo, OpPayloadTransactions},
    config::OpBuilderConfig,
    payload::{OpBuiltPayload, OpPayloadBuilderAttributes},
};
use reth_optimism_primitives::{OpPrimitives, OpReceipt, OpTransactionSigned};
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_provider::{BlockExecutionOutput, ChainSpecProvider, ProviderError, StateProviderFactory};

use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use revm::{DatabaseCommit, context::BlockEnv, inspector::NoOpInspector};
use std::{fmt::Debug, sync::Arc, time::Instant};
use tracing::span;

/// Flashblocks Payload builder
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
    /// Inner Optimism builder configuration (DA settings, etc.).
    pub builder_config: OpBuilderConfig,
    /// Whether to enable Block Access List (BAL) generation.
    pub bal_enabled: bool,
    /// Iterator over best transactions from the pool.
    pub best_transactions: Txs,
    /// Context builder for the payload.
    pub ctx_builder: CtxBuilder,
    /// Metrics recorder for the payload build pipeline.
    pub metrics: Arc<PayloadBuildMetrics>,
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
        self.metrics.increment_attempts();
        let build_started = Instant::now();
        let mut attempt_metrics = PayloadBuildAttemptMetrics::default();

        let ctx = self.ctx_builder.build(
            self.client.clone(),
            self.evm_config.clone(),
            self.builder_config.clone(),
            config,
            &cancel,
            best_payload.clone(),
        );

        let state_provider = self.client.state_by_block_hash(ctx.parent().hash())?;
        let db = StateProviderDatabase::new(state_provider);
        let db = cached_reads.as_db_mut(db);

        let result = build(
            self.client.clone(),
            best,
            Some(self.pool.clone()),
            db,
            &ctx,
            committed_payload,
            &mut attempt_metrics,
            self.bal_enabled,
        );

        attempt_metrics.record_stage_duration(PayloadBuildStage::Total, build_started.elapsed());
        match &result {
            Ok((outcome, _)) => {
                self.metrics.record_outcome(payload_build_outcome(outcome));
                if matches!(
                    outcome,
                    BuildOutcomeKind::Better { .. } | BuildOutcomeKind::Freeze(_)
                ) {
                    attempt_metrics.publish(self.metrics.as_ref());
                }
            }
            Err(_) => self.metrics.record_outcome(PayloadBuildOutcome::Error),
        }

        result.map(|(out, access_list)| (out.with_cached_reads(cached_reads), access_list))
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
    fn payload_build_metrics(&self) -> Arc<PayloadBuildMetrics> {
        self.metrics.clone()
    }

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
pub fn build<'a, Txs, Ctx, Pool>(
    client: impl StateProviderFactory + Clone,
    best: impl Fn(BestTransactionsAttributes) -> Txs + Send + Sync + 'a,
    pool: Option<Pool>,
    db: impl Database<Error = ProviderError>,
    ctx: &Ctx,
    committed_payload: Option<&OpBuiltPayload>,
    attempt_metrics: &mut PayloadBuildAttemptMetrics,
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
    let gas_limit = ctx.attributes().gas_limit.unwrap_or(ctx.parent().gas_limit);

    let attributes = OpNextBlockEnvAttributes {
        timestamp: ctx.attributes().timestamp(),
        suggested_fee_recipient: ctx.attributes().suggested_fee_recipient(),
        prev_randao: ctx.attributes().prev_randao(),
        gas_limit,
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

    // Prepare EVM environment.
    let evm_env = ctx
        .evm_config()
        .next_evm_env(ctx.parent(), &attributes)
        .map_err(PayloadBuilderError::other)?;

    let execution_conext = ctx
        .evm_config()
        .context_for_next_block(ctx.parent(), attributes)
        .map_err(PayloadBuilderError::other)?;

    let committed_state = CommittedState::<OpRethReceiptBuilder>::try_from(committed_payload)
        .map_err(PayloadBuilderError::other)?;

    let effective_gas_limit = ctx
        .effective_gas_limit()
        .saturating_sub(committed_state.gas_used);

    trace!(
        target: "flashblocks::payload_builder",
        gas_limit,
        effective_gas_limit,
        committed_gas_used = committed_state.gas_used,
        timestamp = ctx.attributes().timestamp(),
        "building new payload"
    );

    let bundle_state = committed_state.bundle.clone();

    let visited_transactions = committed_state
        .transaction_hashes_iter()
        .collect::<Vec<_>>();

    let cumulative_uncompressed_bytes: u64 = committed_state
        .transactions
        .iter()
        .map(|(_, tx)| tx.encode_2718_len() as u64)
        .sum();

    if bal_enabled {
        let mut state = State::builder()
            .with_database(db)
            .with_bundle_prestate(bundle_state)
            .with_bundle_update()
            .build();

        let bal_builder_db = BalBuilderDb::new(&mut state);

        // 2. Create the block builder
        let (tx, access_list_rx) = crossbeam_channel::bounded(1);

        let builder = bal_block_builder(
            bal_builder_db,
            execution_conext,
            evm_env,
            &committed_state,
            ctx,
            tx.clone(),
        )?;

        build_inner(
            client,
            committed_payload,
            visited_transactions,
            effective_gas_limit,
            best,
            pool,
            ctx,
            builder,
            attempt_metrics,
            &committed_state,
            Some(access_list_rx),
            cumulative_uncompressed_bytes,
        )
    } else {
        let mut state = State::builder()
            .with_database(db)
            .with_bundle_prestate(bundle_state)
            .with_bundle_update()
            .build();

        let builder = flashblocks_block_builder(
            &mut state,
            execution_conext,
            evm_env,
            &committed_state,
            ctx,
        )?;

        build_inner(
            client,
            committed_payload,
            visited_transactions,
            effective_gas_limit,
            best,
            pool,
            ctx,
            builder,
            attempt_metrics,
            &committed_state,
            None,
            cumulative_uncompressed_bytes,
        )
    }
}

fn payload_build_outcome<P>(outcome: &BuildOutcomeKind<P>) -> PayloadBuildOutcome {
    match outcome {
        BuildOutcomeKind::Better { .. } => PayloadBuildOutcome::Better,
        BuildOutcomeKind::Freeze(_) => PayloadBuildOutcome::Freeze,
        BuildOutcomeKind::Aborted { .. } => PayloadBuildOutcome::Aborted,
        BuildOutcomeKind::Cancelled => PayloadBuildOutcome::Cancelled,
    }
}

fn build_inner<'a, Txs, Ctx, Pool, R>(
    client: impl StateProviderFactory + Clone,
    committed_payload: Option<&OpBuiltPayload>,
    visited_transactions: Vec<TxHash>,
    effective_gas_limit: u64,
    best: impl Fn(BestTransactionsAttributes) -> Txs + Send + Sync + 'a,
    pool: Option<Pool>,
    ctx: &Ctx,
    mut builder: impl BlockBuilderExt<
        Primitives = OpPrimitives,
        Executor: BlockExecutor<
            Evm: Evm<DB: StateDB + DatabaseCommit + Database + 'a, BlockEnv = BlockEnv>,
            Receipt = R::Receipt,
            Transaction = R::Transaction,
        >,
    >,
    attempt_metrics: &mut PayloadBuildAttemptMetrics,
    committed_state: &CommittedState<R>,
    access_list_rx: Option<crossbeam_channel::Receiver<FlashblockAccessList>>,
    mut cumulative_uncompressed_bytes: u64,
) -> Result<
    (
        BuildOutcomeKind<OpBuiltPayload>,
        Option<FlashblockAccessList>,
    ),
    PayloadBuilderError,
>
where
    R: OpReceiptBuilder + Default,
    Pool: TransactionPool,
    Txs: PayloadTransactions,
    Txs::Transaction: OpPooledTx,
    Ctx: PayloadBuilderCtx<
            Evm = OpEvmConfig,
            Transaction = Txs::Transaction,
            ChainSpec = OpChainSpec,
        >,
{
    // Only execute the sequencer transactions on the first payload. The sequencer transactions
    // will already be in the [`BundleState`] at this point if the `best_payload` is set.
    let mut info = if committed_payload.is_none() {
        let pre_execution_changes_started = Instant::now();
        let pre_execution_changes_result = builder.apply_pre_execution_changes();
        attempt_metrics.record_stage_duration(
            PayloadBuildStage::PreExecutionChanges,
            pre_execution_changes_started.elapsed(),
        );
        pre_execution_changes_result?;

        // 4. Execute the sequencer transactions after the block-level pre-execution changes.
        let sequencer_tx_execution_started = Instant::now();
        let sequencer_transactions_uncompressed_size: u64 = ctx
            .attributes()
            .transactions
            .iter()
            .map(|tx| tx.1.encode_2718_len() as u64)
            .sum();
        cumulative_uncompressed_bytes = sequencer_transactions_uncompressed_size;
        let sequencer_tx_execution_result = ctx
            .execute_sequencer_transactions(&mut builder)
            .map_err(PayloadBuilderError::other);
        attempt_metrics.record_stage_duration(
            PayloadBuildStage::SequencerTxExecution,
            sequencer_tx_execution_started.elapsed(),
        );
        sequencer_tx_execution_result?
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
        let txpool_fetch_started = Instant::now();
        let best_txs = best(ctx.best_transaction_attributes(builder.evm_mut().block()));
        let mut best_txns = BestPayloadTxns::new(best_txs).with_prev(visited_transactions);
        attempt_metrics.record_stage_duration(
            PayloadBuildStage::TxPoolFetch,
            txpool_fetch_started.elapsed(),
        );

        let tx_execution_started = Instant::now();
        let tx_execution_result = ctx.execute_best_transactions(
            pool,
            &mut info,
            &mut builder,
            best_txns.guard(),
            attempt_metrics,
            effective_gas_limit,
            cumulative_uncompressed_bytes,
        );
        attempt_metrics.record_stage_duration(
            PayloadBuildStage::BestTxExecution,
            tx_execution_started.elapsed(),
        );

        if tx_execution_result?.is_some() {
            trace!(target: "flashblocks::payload_builder", "payload build cancelled");
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

    // 6. Build the block
    let state_provider = client.state_by_block_hash(ctx.parent().hash())?;
    let finalize_started = Instant::now();
    let finalize_result =
        builder.finish_with_bundle(state_provider.as_ref(), Some(attempt_metrics));
    attempt_metrics.record_stage_duration(PayloadBuildStage::Finalize, finalize_started.elapsed());
    let (build_outcome, bundle) = finalize_result?;

    // 7. Seal the block
    let BlockBuilderOutcome {
        execution_result,
        block,
        hashed_state,
        trie_updates,
    } = build_outcome;

    let sealed_block = Arc::new(block.sealed_block().clone());

    let execution_outcome = BlockExecutionOutput {
        state: bundle,
        result: execution_result,
    };

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

    let access_list = if let Some(access_list_rx) = access_list_rx {
        Some(access_list_rx.recv().map_err(PayloadBuilderError::other)?)
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

pub fn bal_block_builder<'a, Ctx, DB, R, N, Tx>(
    state: BalBuilderDb<&'a mut DB>,
    execution_context: OpBlockExecutionCtx,
    evm_env: EvmEnv<OpSpecId>,
    committed_state: &CommittedState<R>,
    ctx: &'a Ctx,
    tx: crossbeam_channel::Sender<FlashblockAccessList>,
) -> Result<
    BalBlockBuilder<'a, R, N, OpEvm<BalBuilderDb<&'a mut DB>, NoOpInspector, PrecompilesMap>>,
    PayloadBuilderError,
>
where
    Tx: PoolTransaction + OpPooledTx,
    N: NodePrimitives<
            Block = alloy_consensus::Block<OpTransactionSigned>,
            BlockHeader = alloy_consensus::Header,
            Receipt = OpReceipt,
            SignedTx = OpTransactionSigned,
        >,
    DB: StateDB + DatabaseCommit + Database<Error: Send + Sync + 'a> + 'a,
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt> + Default,
    Ctx: PayloadBuilderCtx<Evm = OpEvmConfig, Transaction = Tx, ChainSpec = OpChainSpec>,
{
    let evm = OpEvmFactory::default().create_evm(state, evm_env);

    let mut executor = OpBlockExecutor::<
        OpEvm<BalBuilderDb<&'a mut DB>, NoOpInspector, PrecompilesMap>,
        R,
        Arc<OpChainSpec>,
    >::new(
        evm,
        execution_context.clone(),
        ctx.spec().clone().into(),
        R::default(),
    );
    executor.gas_used = committed_state.gas_used;
    executor.receipts = committed_state.receipts_iter().cloned().collect();

    let builder = BalBlockBuilder::new(
        execution_context,
        ctx.parent(),
        executor,
        committed_state.transactions_iter().cloned().collect(),
        ctx.spec().clone().into(),
        tx,
    );

    Ok(builder)
}

pub fn flashblocks_block_builder<'a, Ctx, DB, Tx>(
    state: &'a mut DB,
    execution_context: OpBlockExecutionCtx,
    evm_env: EvmEnv<OpSpecId>,
    committed_state: &CommittedState<OpRethReceiptBuilder>,
    ctx: &'a Ctx,
) -> Result<
    impl BlockBuilderExt<
        Primitives = OpPrimitives,
        Executor = OpBlockExecutor<
            OpEvm<&'a mut DB, NoOpInspector, PrecompilesMap>,
            OpRethReceiptBuilder,
            OpChainSpec,
        >,
    > + 'a,
    PayloadBuilderError,
>
where
    OpBlockExecutorFactory<OpRethReceiptBuilder>:
        BlockExecutorFactory<Receipt = OpReceipt, Transaction = OpTransactionSigned>,
    Tx: PoolTransaction + OpPooledTx,
    DB: StateDB + DatabaseCommit + reth_evm::Database<Error: Send + Sync + 'a> + 'a,
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

    let builder = FlashblocksBlockBuilder::new(
        execution_context,
        ctx.parent(),
        executor,
        committed_state.transactions_iter().cloned().collect(),
        ctx.spec().clone().into(),
    );

    Ok(builder)
}
