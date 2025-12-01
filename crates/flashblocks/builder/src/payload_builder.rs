use crate::{
    executor::bal_executor::{BalExecutionState, CommittedState},
    payload_txns::BestPayloadTxns,
    traits::{
        context::PayloadBuilderCtx, context_builder::PayloadBuilderCtxBuilder,
        payload_builder::FlashblockPayloadBuilder,
    },
};

use crate::block_builder::FlashblocksBlockBuilder;
use alloy_consensus::{BlockHeader, Header};

use alloy_op_evm::{OpEvm, block::receipt_builder::OpReceiptBuilder};
use flashblocks_primitives::{
    access_list::FlashblockAccessList, primitives::ExecutionPayloadFlashblockDeltaV1,
};
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
use reth_chain_state::ExecutedBlock;
use reth_evm::{
    ConfigureEvm, Database,
    execute::{BlockBuilder, BlockBuilderOutcome},
    precompiles::PrecompilesMap,
};
use reth_primitives::NodePrimitives;
use tracing::{info, warn};

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
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_provider::{
    ChainSpecProvider, ExecutionOutcome, ProviderError, StateProvider, StateProviderFactory,
};

use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use revm::{context::ContextTr, inspector::NoOpInspector};
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
    pub config: OpBuilderConfig,
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
    ) -> Result<(BuildOutcome<OpBuiltPayload>, FlashblockAccessList), PayloadBuilderError>
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
            self.config.clone(),
            config,
            &cancel,
            best_payload.clone(),
        );

        let state_provider = self.client.state_by_block_hash(ctx.parent().hash())?;
        let db = StateProviderDatabase::new(&state_provider);

        if ctx.attributes().no_tx_pool {
            build(
                best,
                self.pool.clone(),
                db,
                &state_provider,
                &ctx,
                committed_payload,
            )
        } else {
            // sequencer mode we can reuse cachedreads from previous runs
            build(
                best,
                self.pool.clone(),
                cached_reads.as_db_mut(db),
                &state_provider,
                &ctx,
                committed_payload,
            )
        }
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
            FlashblockAccessList,
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
fn build<'a, Txs, Ctx, Pool>(
    best: impl Fn(BestTransactionsAttributes) -> Txs + Send + Sync + 'a,
    pool: Pool,
    db: impl Database<Error = ProviderError> + Send + Sync + 'a,
    state_provider: impl StateProvider,
    ctx: &Ctx,
    committed_payload: Option<&OpBuiltPayload>,
) -> Result<(BuildOutcomeKind<OpBuiltPayload>, FlashblockAccessList), PayloadBuilderError>
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

    // 1. Prepare the db
    let committed_state = committed_payload
        .cloned()
        .map(CommittedState::<OpRethReceiptBuilder>::try_from)
        .transpose()
        .map_err(PayloadBuilderError::other)?
        .unwrap_or_default();

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

    // Prepare EVM environment.
    let evm_env = ctx
        .evm_config()
        .next_evm_env(ctx.parent(), &attributes)
        .map_err(PayloadBuilderError::other)?;

    let execution_conext = ctx
        .evm_config()
        .context_for_next_block(ctx.parent(), attributes)
        .map_err(PayloadBuilderError::other)?;

    let mut execution_state = BalExecutionState::new_bal_execution_state(
        committed_state,
        evm_env,
        ctx.evm_config().clone(),
        execution_conext,
        ExecutionPayloadFlashblockDeltaV1::default(),
    )
    .map_err(PayloadBuilderError::other)?;

    let gas_limit = ctx
        .attributes()
        .gas_limit
        .unwrap_or(ctx.parent().gas_limit)
        .saturating_sub(execution_state.committed_state.gas_used);

    let mut state = execution_state.state_for_db(db);

    // 2. Create the block builder
    let mut builder = block_builder(&mut state, &mut execution_state, ctx)?;

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
    if !ctx.attributes().no_tx_pool {
        let best_txs = best(ctx.best_transaction_attributes(builder.evm_mut().block()));
        let mut best_txns = BestPayloadTxns::new(best_txs).with_prev(
            execution_state
                .committed_transactions_iter()
                .map(|tx| tx.hash())
                .copied()
                .collect(),
        );

        if ctx
            .execute_best_transactions(pool, &mut info, &mut builder, best_txns.guard(), gas_limit)?
            .is_none()
        {
            warn!(target: "flashblocks::payload_builder", "payload build cancelled");
            if let Some(best_payload) = committed_payload {
                // we can return the previous best payload since we didn't include any new txs
                return Ok((
                    BuildOutcomeKind::Freeze(best_payload.clone()),
                    FlashblockAccessList::default(),
                ));
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
                FlashblockAccessList::default(),
            ));
        }
    }

    // 6. Build the block
    let (build_outcome, access_list) = builder.finish_with_access_list(&state_provider)?;

    info!(target: "test_target", "built new payload with {} transactions, receipts {:#?}", build_outcome.block.body().transactions().count(), build_outcome.execution_result.receipts.len());

    // 7. Seal the block
    let BlockBuilderOutcome {
        execution_result,
        block,
        hashed_state,
        trie_updates,
    } = build_outcome;

    let sealed_block = Arc::new(block.sealed_block().clone());

    let execution_outcome = ExecutionOutcome::new(
        state.take_bundle(),
        vec![execution_result.receipts.clone()],
        block.number(),
        Vec::new(),
    );

    // create the executed block data
    let executed_block = ExecutedBlock {
        recovered_block: Arc::new(block),
        execution_output: Arc::new(execution_outcome),
        hashed_state: Arc::new(hashed_state),
        trie_updates: Arc::new(trie_updates),
    };

    let payload = OpBuiltPayload::new(
        ctx.payload_id(),
        sealed_block,
        info.total_fees + execution_state.committed_state.fees,
        Some(executed_block),
    );

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

#[expect(clippy::type_complexity)]
fn block_builder<'a, Ctx, DB, R, N, Tx>(
    state: &'a mut State<DB>,
    execution_state: &mut BalExecutionState<R>,
    ctx: &'a Ctx,
) -> Result<
    FlashblocksBlockBuilder<'a, R, N, OpEvm<&'a mut State<DB>, NoOpInspector, PrecompilesMap>>,
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
    DB: reth_evm::Database + Send + Sync + 'a,
    DB::Error: Send + Sync + 'a,
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt> + Default,
    Ctx: PayloadBuilderCtx<Evm = OpEvmConfig, Transaction = Tx, ChainSpec = OpChainSpec>,
{
    let executor = execution_state.basic_executor(ctx.spec().clone().into(), R::default(), state);
    let committed_transactions = execution_state.committed_transactions_iter();

    Ok(FlashblocksBlockBuilder::new(
        execution_state.execution_context.clone(),
        ctx.parent(),
        executor,
        committed_transactions.cloned().collect(),
        Arc::new(ctx.spec().clone()),
    ))
}
