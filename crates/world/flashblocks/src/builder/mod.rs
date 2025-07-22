use crate::{
    builder::{
        executor::{FlashblocksBlockBuilder, FlashblocksBlockExecutor},
        payload_txns::BestPayloadTxns,
    },
    payload_builder_ctx::{PayloadBuilderCtx, PayloadBuilderCtxBuilder},
};

use alloy_consensus::BlockHeader;
use alloy_eips::Encodable2718;
use alloy_op_evm::OpEvm;
use alloy_primitives::U256;
use op_alloy_consensus::OpTxEnvelope;
use reth::api::BlockBody;
use reth::{
    api::{PayloadBuilderAttributes, PayloadBuilderError},
    chainspec::EthChainSpec,
    revm::{cancelled::CancelOnDrop, database::StateProviderDatabase, State},
};
use reth_chain_state::{ExecutedBlock, ExecutedBlockWithTrieUpdates, ExecutedTrieUpdates};
use reth_primitives::{NodePrimitives, Recovered};
use rollup_boost::FlashblocksP2PMsg;
use rollup_boost::{
    ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1, FlashblocksPayloadV1,
};

use reth_basic_payload_builder::{BuildArguments, BuildOutcome, BuildOutcomeKind};
use reth_basic_payload_builder::{MissingPayloadBehaviour, PayloadBuilder, PayloadConfig};
use reth_evm::{
    execute::{BlockBuilder, BlockBuilderOutcome},
    precompiles::PrecompilesMap,
    ConfigureEvm,
};

use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{
    txpool::OpPooledTx, OpEvmConfig, OpNextBlockEnvAttributes, OpRethReceiptBuilder,
};
use reth_optimism_payload_builder::config::OpBuilderConfig;
use reth_optimism_payload_builder::{
    builder::OpPayloadTransactions,
    payload::{OpBuiltPayload, OpPayloadBuilderAttributes},
};
use reth_optimism_primitives::{OpPrimitives, OpReceipt, OpTransactionSigned};
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_provider::{
    BlockExecutionResult, ChainSpecProvider, ExecutionOutcome, StateProvider, StateProviderFactory,
};

use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use revm::inspector::NoOpInspector;
use rollup_boost::{
    ed25519_dalek::{SigningKey, VerifyingKey},
    Authorization, Authorized,
};
use std::{fmt::Debug, sync::Arc};
use tokio::{sync::broadcast, time::Instant};
use tracing::{debug, error, span, warn};

pub mod executor;
pub mod payload_txns;

/// Flashblocks Paylod builder
///
/// A payload builder
#[derive(Debug)]
pub struct FlashblocksPayloadBuilder<Pool, Client, CtxBuilder, Txs = ()> {
    /// The type responsible for creating the evm.
    pub evm_config: OpEvmConfig,
    /// Transaction pool.
    pub pool: Pool,
    /// Node client.
    pub client: Client,
    /// Settings for the builder, e.g. DA settings.
    pub config: OpBuilderConfig,
    /// The type responsible for yielding the best transactions for the payload if mempool
    /// transactions are allowed.
    pub best_transactions: Txs,
    /// Block time in milliseconds
    pub block_time: u64,
    /// Flashblock interval in milliseconds
    pub flashblock_interval: u64,
    pub ctx_builder: CtxBuilder,
    /// Channel for publishing messages
    pub publish_tx: broadcast::Sender<FlashblocksP2PMsg>,
    pub authorizer_vk: Option<VerifyingKey>,
    pub builder_sk: SigningKey,
}

// TODO: This manual impl is required because we can't require PayloadBuilderCtx
//       to be Clone, because OpPayloadBuilderCtx is not Clone.
//       The workaround is to put ctx in `Arc` and not have to depend on it being Clone
impl<Pool, Client, CtxBuilder, Txs> Clone
    for FlashblocksPayloadBuilder<Pool, Client, CtxBuilder, Txs>
where
    Pool: Clone,
    Client: Clone,
    Txs: Clone,
    CtxBuilder: Clone,
{
    fn clone(&self) -> Self {
        Self {
            evm_config: self.evm_config.clone(),
            pool: self.pool.clone(),
            client: self.client.clone(),
            config: self.config.clone(),
            best_transactions: self.best_transactions.clone(),
            block_time: self.block_time,
            flashblock_interval: self.flashblock_interval,
            ctx_builder: self.ctx_builder.clone(),
            publish_tx: self.publish_tx.clone(),
            authorizer_vk: self.authorizer_vk,
            builder_sk: self.builder_sk.clone(),
        }
    }
}

impl<Pool, Client, CtxBuilder, Txs> FlashblocksPayloadBuilder<Pool, Client, CtxBuilder, Txs>
where
    Txs: OpPayloadTransactions<Pool::Transaction>,
    Pool: TransactionPool<Transaction: OpPooledTx<Consensus = OpTxEnvelope>>,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec = OpChainSpec>,
    CtxBuilder: PayloadBuilderCtxBuilder<OpEvmConfig, OpChainSpec, Pool::Transaction>,
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
    ) -> Result<BuildOutcome<OpBuiltPayload>, PayloadBuilderError>
    where
        T: PayloadTransactions<Transaction = <Pool as TransactionPool>::Transaction>,
    {
        let BuildArguments {
            cached_reads,
            config,
            cancel,
            best_payload,
        } = args;

        let ctx = self.ctx_builder.build::<Txs>(
            self.evm_config.clone(),
            self.config.da_config.clone(),
            self.client.chain_spec(),
            config,
            &cancel,
            best_payload,
        );

        let builder = FlashblockBuilder::new(
            best,
            self.publish_tx.clone(),
            // TODO: figure out how to get the authorization from the FCU
            None,
            self.builder_sk.clone(),
            self.block_time,
            self.flashblock_interval,
            cancel.clone(),
        );

        let state_provider = self.client.state_by_block_hash(ctx.parent().hash())?;

        if ctx.attributes().no_tx_pool {
            builder.build(&state_provider, &ctx)
        } else {
            // sequencer mode we can reuse cachedreads from previous runs
            builder.build(&state_provider, &ctx)
        }
        .map(|out| out.with_cached_reads(cached_reads))
    }
}

impl<Pool, Client, CtxBuilder, Txs> PayloadBuilder
    for FlashblocksPayloadBuilder<Pool, Client, CtxBuilder, Txs>
where
    Client: Clone + StateProviderFactory + ChainSpecProvider<ChainSpec = OpChainSpec>,
    Pool: TransactionPool<Transaction: OpPooledTx<Consensus = OpTxEnvelope>>,
    CtxBuilder: PayloadBuilderCtxBuilder<OpEvmConfig, Client::ChainSpec, Pool::Transaction>,
    Txs: OpPayloadTransactions<Pool::Transaction>,
{
    type Attributes = OpPayloadBuilderAttributes<OpTxEnvelope>;
    type BuiltPayload = OpBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        self.build_payload(args, |attrs| {
            self.best_transactions
                .best_transactions(self.pool.clone(), attrs)
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

    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes, alloy_consensus::Header>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        let args = BuildArguments {
            config,
            cached_reads: Default::default(),
            cancel: Default::default(),
            best_payload: None,
        };
        self.build_payload(args, |_| {
            NoopPayloadTransactions::<Pool::Transaction>::default()
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
pub struct FlashblockBuilder<'a, Txs>
where
    Txs: PayloadTransactions,
{
    /// Yields the best transaction to include if transactions from the mempool are allowed.
    best: Box<dyn Fn(BestTransactionsAttributes) -> Txs + 'a>,

    /// Channel sender for publishing messages
    pub publish_tx: broadcast::Sender<FlashblocksP2PMsg>,
    pub authorization: Option<Authorization>,
    pub builder_sk: SigningKey,
    pub block_time: u64,
    pub flashblock_interval: u64,
    pub cancel: CancelOnDrop,
}

impl<'a, Txs> FlashblockBuilder<'a, Txs>
where
    Txs: PayloadTransactions,
{
    /// Creates a new [`OpBuilder`].
    pub fn new(
        best: impl Fn(BestTransactionsAttributes) -> Txs + Send + Sync + 'a,
        publish_tx: broadcast::Sender<FlashblocksP2PMsg>,
        authorization: Option<Authorization>,
        builder_sk: SigningKey,
        block_time: u64,
        flashblock_interval: u64,
        cancel: CancelOnDrop,
    ) -> Self {
        Self {
            best: Box::new(best),
            authorization,
            builder_sk,
            publish_tx,
            block_time,
            flashblock_interval,
            cancel,
        }
    }
}

impl<'a, Txs> FlashblockBuilder<'_, Txs>
where
    Txs: PayloadTransactions,
{
    /// Builds the payload on top of the state.
    pub fn build<Ctx, Tx>(
        self,
        state_provider: impl StateProvider + Clone,
        ctx: &Ctx,
    ) -> Result<BuildOutcomeKind<OpBuiltPayload>, PayloadBuilderError>
    where
        Tx: PoolTransaction<Consensus = OpTransactionSigned> + OpPooledTx,
        Txs: PayloadTransactions<Transaction = Tx>,
        Ctx: PayloadBuilderCtx<Evm = OpEvmConfig, Transaction = Tx, ChainSpec = OpChainSpec>,
    {
        let span = span!(
            tracing::Level::INFO,
            "flashblock_builder",
            id = %ctx.payload_id(),
        );

        let _enter = span.enter();

        debug!(target: "payload_builder", "building new payload");

        // 1. Setup relevant variables
        let mut flashblock_idx = 0;
        let mut transactions_offset = 0;

        let gas_limit = ctx.attributes().gas_limit.unwrap_or(ctx.parent().gas_limit);

        let state = StateProviderDatabase::new(&state_provider);

        let mut db = State::builder()
            .with_database(state)
            .with_bundle_update()
            .build();

        let mut builder = self.block_builder(&mut db, vec![], None, ctx)?;

        let (mut info, mut bundle_state) = ctx
            .execute_sequencer_transactions(&mut builder)
            .map_err(PayloadBuilderError::other)?;

        let mut build_outcome = builder.finish(&state_provider)?;

        let flashblock_payload = flashblock_payload_from_outcome(
            &build_outcome,
            ctx,
            flashblock_idx,
            transactions_offset,
        );

        transactions_offset += build_outcome.block.body().transactions_iter().count();

        let authorization = self.authorization.clone().unwrap_or_else(|| {
            // if no authorization is provided, we create one using the builder's signing key
            Authorization::new(
                flashblock_payload.payload_id,
                self.block_time,
                &self.builder_sk,
                self.builder_sk.verifying_key(),
            )
        });
        let authorized =
            Authorized::new(&self.builder_sk, authorization.clone(), flashblock_payload);
        let p2p_msg = FlashblocksP2PMsg::FlashblocksPayloadV1(authorized);

        if let Err(err) = self.publish_tx.send(p2p_msg) {
            error!(target: "payload_builder", %err, "failed to send flashblock payload");
        };

        let total_flashbblocks = self.block_time as usize / self.flashblock_interval as usize;
        let (tx, mut rx) = tokio::sync::mpsc::channel(total_flashbblocks);

        // Tracks all executed transactions across all flashblocks.
        let mut executed_txns = vec![];

        // spawn a task to schedule when the next flashblock job should be started/cancelled
        self.spawn_flashblock_job_manager(tx);

        let bundle_state = loop {
            debug!(target: "payload_builder", "building flashblock {flashblock_idx}");
            let state = StateProviderDatabase::new(&state_provider);

            let mut db = State::builder()
                .with_database(state)
                .with_bundle_update()
                .with_bundle_prestate(bundle_state)
                .build();

            let notify = tokio::task::block_in_place(|| rx.blocking_recv());

            let span = span!(
                tracing::Level::DEBUG,
                "flashblock_builder",
                id = %ctx.attributes().payload_id(),
                flashblock_idx,
            );

            match notify {
                Some(()) => {
                    let _enter = span.enter();

                    let best_txns =
                        (*self.best)(ctx.best_transaction_attributes(ctx.evm_env().block_env()));

                    let mut best_txns = BestPayloadTxns::new(best_txns)
                        .with_prev(std::mem::take(&mut executed_txns));

                    let transactions = build_outcome
                        .block
                        .clone_transactions_recovered()
                        .collect::<Vec<_>>();

                    let mut builder = self.block_builder(
                        &mut db,
                        transactions,
                        Some(build_outcome.execution_result.clone()),
                        ctx,
                    )?;

                    let inner_gas_limit = gas_limit.saturating_sub(build_outcome.block.gas_used());
                    if inner_gas_limit == 0 {
                        debug!(target: "payload_builder",  "no gas left for flashblock - stopping");
                        break db.take_bundle();
                    };

                    let Some(new_bundle_state) = ctx.execute_best_transactions(
                        &mut info,
                        &mut builder,
                        best_txns.guard(),
                        inner_gas_limit,
                    )?
                    else {
                        break db.take_bundle();
                    };

                    build_outcome = builder.finish(&state_provider)?;

                    let flashblock_payload = flashblock_payload_from_outcome(
                        &build_outcome,
                        ctx,
                        flashblock_idx,
                        transactions_offset,
                    );

                    let authorized = Authorized::new(
                        &self.builder_sk,
                        authorization.clone(),
                        flashblock_payload,
                    );
                    let p2p_msg = FlashblocksP2PMsg::FlashblocksPayloadV1(authorized);

                    if let Err(err) = self.publish_tx.send(p2p_msg) {
                        error!(target: "payload_builder", %err, "failed to send flashblock payload");
                    }

                    // update executed transactions
                    let (prev, observed) = best_txns.take_observed();

                    executed_txns.extend_from_slice(&prev.collect::<Vec<_>>());
                    executed_txns.extend_from_slice(&observed.collect::<Vec<_>>());

                    transactions_offset += build_outcome.block.body().transactions_iter().count();
                    bundle_state = new_bundle_state;
                }

                // tx was dropped, resolve the most recent payload
                None => {
                    debug!(target: "payload_builder", "no more flashblocks to build, resolving payload");
                    break db.take_bundle();
                }
            }

            flashblock_idx += 1;
        };

        let BlockBuilderOutcome {
            execution_result,
            block,
            hashed_state,
            trie_updates,
        } = build_outcome;

        let sealed_block = Arc::new(block.sealed_block().clone());

        let execution_outcome = ExecutionOutcome::new(
            bundle_state,
            vec![execution_result.receipts],
            block.number(),
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

        let payload = OpBuiltPayload::new(
            ctx.payload_id(),
            sealed_block,
            info.total_fees,
            Some(executed),
        );

        debug!(target: "payload_builder", id=%ctx.attributes().payload_id(), "built payload");

        if ctx.attributes().no_tx_pool {
            // if `no_tx_pool` is set only transactions from the payload attributes will be included
            // in the payload. In other words, the payload is deterministic and we can
            // freeze it once we've successfully built it.
            Ok(BuildOutcomeKind::Freeze(payload))
        } else {
            Ok(BuildOutcomeKind::Better { payload })
        }
    }

    pub fn block_builder<Ctx, DB, N, Tx>(
        &self,
        db: &'a mut State<DB>,
        transactions: Vec<Recovered<N::SignedTx>>,
        execution_result: Option<BlockExecutionResult<OpReceipt>>,
        ctx: &'a Ctx,
    ) -> Result<
        FlashblocksBlockBuilder<'a, N, OpEvm<&'a mut State<DB>, NoOpInspector, PrecompilesMap>>,
        PayloadBuilderError,
    >
    where
        Tx: PoolTransaction<Consensus = OpTransactionSigned> + OpPooledTx,
        N: NodePrimitives<
            Block = alloy_consensus::Block<OpTransactionSigned>,
            BlockHeader = alloy_consensus::Header,
        >,
        DB: reth_evm::Database + 'a,
        DB::Error: Send + Sync + 'static,
        Ctx: PayloadBuilderCtx<Evm = OpEvmConfig, Transaction = Tx, ChainSpec = OpChainSpec>,
    {
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

        // Prepare EVM.
        let evm = ctx.evm_config().evm_with_env(db, evm_env);

        // Prepare block execution context.
        let execution_ctx = ctx
            .evm_config()
            .context_for_next_block(ctx.parent(), attributes);

        let mut executor = FlashblocksBlockExecutor::new(
            evm,
            ctx.spec().clone(),
            OpRethReceiptBuilder::default(),
            execution_ctx.clone(),
        );

        if let Some(execution_result) = execution_result {
            executor = executor.with_execution_result(execution_result)
        }

        Ok(FlashblocksBlockBuilder::new(
            execution_ctx,
            ctx.parent(),
            executor,
            transactions,
            Arc::new(ctx.spec().clone()),
        ))
    }

    /// Spawns a task responsible for cancelling, and initiating building of new flashblock payloads.
    ///
    /// A Flashblock job will be cancelled under the following conditions:
    /// - If the parent `cancel` is dropped.
    /// - If the `flashblock_interval` has been exceeded.
    /// - If the `max_flashblocks` has been exceeded.
    fn spawn_flashblock_job_manager(&self, tx: tokio::sync::mpsc::Sender<()>) {
        let block_time = self.block_time;
        let flashblock_interval = self.flashblock_interval;
        let cancel = self.cancel.clone();
        let span = span!(tracing::Level::DEBUG, "flashblock_job_manager");
        tokio::spawn(async move {
            let _enter = span.enter();
            debug!(target: "payload_builder", "flashblock job manager started");

            let mut flashblock_interval =
                tokio::time::interval(tokio::time::Duration::from_millis(flashblock_interval));
            let mut block_interval =
                tokio::time::interval(tokio::time::Duration::from_millis(block_time));

            block_interval.tick().await;
            flashblock_interval.tick().await;

            tokio::select! {
                _ = block_interval.tick() => {
                    debug!(target: "payload_builder", "block interval exceeded, cancelling current job");
                    // block interval exceeded, cancel the current job
                    // and drop the sender to resolve the most recent payload.
                    drop(tx);
                },

                _ = async {
                    loop {
                        let instant = Instant::now();
                        let _tx = tx.clone();
                        if cancel.is_cancelled() {
                            debug!(target: "payload_builder", "parent cancel was dropped, cancelling flashblock job");
                            // parent cancel was dropped by the payload jobs generator
                            // cancel the child job, and drop the sender
                            break;
                        }

                        tokio::select! {
                            _ = flashblock_interval.tick() => {
                                let elapsed = instant.elapsed();
                                warn!(target: "payload_builder", ?elapsed, "flashblock interval exceeded, queuing next flashblock job");
                                // queue the next flashblock job, but there's no benefit in cancelling the current job
                                // here we are exceeding `flashblock_interval` on the current job, but we want to give it `block_time` to finish.
                                // because the next job will also exceed `flashblock_interval`.
                                if let Err(err) = tx.send(()).await {
                                    warn!(target: "payload_builder", %err, "failed to send flashblock job notification");
                                }
                            },
                       }
                    }
                } => {
                    // either the parent payload job was cancelled, or all flashblocks were built.
                    // in either case, drop the sender to resolve the most recent payload.
                }
            }
        });
    }
}

fn flashblock_payload_from_outcome<N: NodePrimitives, Ctx>(
    outcome: &BlockBuilderOutcome<N>,
    ctx: &Ctx,
    flashblock_idx: u64,
    transactions_offset: usize,
) -> FlashblocksPayloadV1
where
    Ctx:
        PayloadBuilderCtx<Evm = OpEvmConfig, ChainSpec = OpChainSpec, Transaction: PoolTransaction>,
{
    let payload_base =
        if flashblock_idx == 0 {
            Some(ExecutionPayloadBaseV1 {
                parent_beacon_block_root: ctx
                    .attributes()
                    .payload_attributes
                    .parent_beacon_block_root
                    .unwrap(),
                parent_hash: ctx.parent().hash(),
                fee_recipient: ctx.attributes().suggested_fee_recipient(),
                prev_randao: ctx.attributes().payload_attributes.prev_randao,
                block_number: ctx.parent().number + 1,
                gas_limit: ctx.attributes().gas_limit.unwrap_or(ctx.parent().gas_limit),
                timestamp: ctx.attributes().payload_attributes.timestamp,
                extra_data: ctx
                    .attributes()
                    .get_holocene_extra_data(ctx.spec().base_fee_params_at_timestamp(
                        ctx.attributes().payload_attributes.timestamp,
                    ))
                    .unwrap_or_default(),
                base_fee_per_gas: U256::from(ctx.evm_env().block_env().basefee),
            })
        } else {
            None
        };

    let transactions = outcome
        .block
        .body()
        .transactions_iter()
        .skip(transactions_offset)
        .map(|tx| tx.encoded_2718().into())
        .collect::<Vec<_>>();

    FlashblocksPayloadV1 {
        payload_id: ctx.payload_id(),
        index: flashblock_idx,
        base: payload_base,
        diff: ExecutionPayloadFlashblockDeltaV1 {
            state_root: outcome.block.state_root(),
            receipts_root: outcome.block.receipts_root(),
            logs_bloom: outcome.block.logs_bloom(),
            gas_used: outcome.block.gas_used(),
            block_hash: outcome.block.hash(),
            transactions,
            withdrawals: outcome
                .block
                .body()
                .withdrawals()
                .cloned()
                .unwrap_or_default()
                .to_vec(),
            withdrawals_root: outcome.block.withdrawals_root().unwrap_or_default(),
        },
        metadata: Default::default(),
    }
}
