use crate::{
    payload::{FlashblockBuildOutcome, FlashblocksPayloadV1},
    payload_builder_ctx::{PayloadBuilderCtx, PayloadBuilderCtxBuilder},
};
use alloy_consensus::BlockHeader;
use eyre::eyre::eyre;
use futures_util::select;
use retaining_payload_txs::RetainingBestTxs;
use reth::{
    api::{payload, PayloadBuilderAttributes, PayloadBuilderError},
    chainspec::EthChainSpec,
    core::primitives::block,
    revm::{cancelled::CancelOnDrop, database::StateProviderDatabase, Database, State},
};
use reth_basic_payload_builder::{BuildArguments, BuildOutcome, BuildOutcomeKind};
use reth_basic_payload_builder::{MissingPayloadBehaviour, PayloadBuilder, PayloadConfig};
use reth_chain_state::{ExecutedBlock, ExecutedBlockWithTrieUpdates, ExecutedTrieUpdates};
use reth_evm::{
    execute::{BlockBuilder, BlockBuilderOutcome},
    ConfigureEvm, Evm,
};
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{
    txpool::{interop::MaybeInteropTransaction, OpPooledTx},
    OpNextBlockEnvAttributes,
};
use reth_optimism_payload_builder::{
    builder::{ExecutionInfo, OpPayloadTransactions},
    payload::{OpBuiltPayload, OpPayloadBuilderAttributes},
};
use reth_optimism_payload_builder::{config::OpBuilderConfig, OpPayloadPrimitives};
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_provider::{
    ChainSpecProvider, ExecutionOutcome, ProviderError, StateProvider, StateProviderFactory,
};
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use std::{
    fmt::Debug,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{runtime::Handle, sync::mpsc};
use tracing::{debug, error, info, warn, Level, Span};

mod retaining_payload_txs;

/// Flashblocks Paylod builder
///
/// A payload builder
#[derive(Debug)]
pub struct FlashblocksPayloadBuilder<Pool, Client, Evm, CtxBuilder, Txs = ()> {
    /// The type responsible for creating the evm.
    pub evm_config: Evm,
    /// Transaction pool.
    pub pool: Pool,
    /// Node client.
    pub client: Client,
    /// Settings for the builder, e.g. DA settings.
    pub config: OpBuilderConfig,
    /// The type responsible for yielding the best transactions for the payload if mempool
    /// transactions are allowed.
    pub best_transactions: Txs,
    /// Channel sender for publishing messages
    pub tx: mpsc::UnboundedSender<FlashblocksPayloadV1>,
    /// Block time in milliseconds
    pub block_time: u64,
    /// Flashblock interval in milliseconds
    pub flashblock_interval: u64,
    pub ctx_builder: CtxBuilder,
}

// TODO: This manual impl is required because we can't require PayloadBuilderCtx
//       to be Clone, because OpPayloadBuilderCtx is not Clone.
//       The workaround is to put ctx in `Arc` and not have to depend on it being Clone
impl<Pool, Client, Evm, CtxBuilder, Txs> Clone
    for FlashblocksPayloadBuilder<Pool, Client, Evm, CtxBuilder, Txs>
where
    Pool: Clone,
    Client: Clone,
    Evm: Clone,
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
            tx: self.tx.clone(),
            block_time: self.block_time,
            flashblock_interval: self.flashblock_interval,
            ctx_builder: self.ctx_builder.clone(),
        }
    }
}

impl<Pool, Client, Evm, N, CtxBuilder, Txs>
    FlashblocksPayloadBuilder<Pool, Client, Evm, CtxBuilder, Txs>
where
    N: OpPayloadPrimitives,
    Txs: OpPayloadTransactions<Pool::Transaction>,
    Pool: TransactionPool<Transaction: OpPooledTx<Consensus = N::SignedTx>>,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec: EthChainSpec + OpHardforks>,
    Evm: ConfigureEvm<Primitives = N, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
    CtxBuilder: PayloadBuilderCtxBuilder<Evm, Client::ChainSpec, Pool::Transaction>,
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
        args: BuildArguments<OpPayloadBuilderAttributes<N::SignedTx>, OpBuiltPayload<N>>,
        best: impl Fn(BestTransactionsAttributes) -> T + Send + Sync + 'a,
    ) -> Result<BuildOutcome<OpBuiltPayload<N>>, PayloadBuilderError>
    where
        T: PayloadTransactions<Transaction = <Pool as TransactionPool>::Transaction>,
    {
        let BuildArguments {
            mut cached_reads,
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
            self.tx.clone(),
            self.block_time,
            self.flashblock_interval,
        );
        let state_provider = self.client.state_by_block_hash(ctx.parent().hash())?;
        let state = StateProviderDatabase::new(&state_provider);

        if ctx.attributes().no_tx_pool {
            builder.build(state, &state_provider, &ctx, cancel)
        } else {
            // sequencer mode we can reuse cachedreads from previous runs
            builder.build(cached_reads.as_db_mut(state), &state_provider, &ctx, cancel)
        }
        .map(|out| out.with_cached_reads(cached_reads))
    }
}

impl<Pool, Client, Evm, N, CtxBuilder, Txs> PayloadBuilder
    for FlashblocksPayloadBuilder<Pool, Client, Evm, CtxBuilder, Txs>
where
    N: OpPayloadPrimitives,
    Client: Clone + StateProviderFactory + ChainSpecProvider<ChainSpec: OpHardforks>,
    Pool: TransactionPool<Transaction: OpPooledTx<Consensus = N::SignedTx>>,
    Evm: ConfigureEvm<Primitives = N, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
    CtxBuilder: PayloadBuilderCtxBuilder<Evm, Client::ChainSpec, Pool::Transaction>,
    Txs: OpPayloadTransactions<Pool::Transaction>,
{
    type Attributes = OpPayloadBuilderAttributes<N::SignedTx>;
    type BuiltPayload = OpBuiltPayload<N>;

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
        config: PayloadConfig<Self::Attributes, N::BlockHeader>,
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
    pub tx: mpsc::UnboundedSender<FlashblocksPayloadV1>,
    pub block_time: u64,
    pub flashblock_interval: u64,
    executed_txs: Vec<Txs::Transaction>,
}

impl<'a, Txs> FlashblockBuilder<'a, Txs>
where
    Txs: PayloadTransactions,
{
    /// Creates a new [`OpBuilder`].
    pub fn new(
        best: impl Fn(BestTransactionsAttributes) -> Txs + Send + Sync + 'a,
        tx: mpsc::UnboundedSender<FlashblocksPayloadV1>,
        block_time: u64,
        flashblock_interval: u64,
    ) -> Self {
        Self {
            best: Box::new(best),
            tx,
            block_time,
            flashblock_interval,
            executed_txs: Vec::new(),
        }
    }
}

impl<Txs> FlashblockBuilder<'_, Txs>
where
    Txs: PayloadTransactions,
{
    /// Builds the payload on top of the state.
    pub fn build<Evm, ChainSpec, N, Ctx, Tx>(
        mut self,
        db: impl Database<Error = ProviderError>,
        state_provider: impl StateProvider + Clone,
        ctx: &Ctx,
        cancel: CancelOnDrop,
    ) -> Result<BuildOutcomeKind<OpBuiltPayload<N>>, PayloadBuilderError>
    where
        N: OpPayloadPrimitives,
        Tx: PoolTransaction<Consensus = N::SignedTx> + OpPooledTx,
        Txs: PayloadTransactions<Transaction = Tx>,
        Evm: ConfigureEvm<Primitives = N, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
        ChainSpec: EthChainSpec + OpHardforks,
        Ctx: PayloadBuilderCtx<Evm = Evm, ChainSpec = ChainSpec, Transaction = Tx>,
    {
        debug!(target: "payload_builder", id=%ctx.payload_id(), parent_header = ?ctx.parent().hash(), parent_number = ctx.parent().number, "building new payload");
        let mut state = State::builder()
            .with_database(db)
            .with_bundle_update()
            .build();

        // 1. Setup relevant variables
        let mut flashblock_idx = 0;
        let gas_limit = ctx.attributes().gas_limit.unwrap_or(ctx.parent().gas_limit);

        // 2. Build the block
        let Some(mut latest_outcome) =
            self.build_flashblock(&mut state, &state_provider, ctx, gas_limit, flashblock_idx)?
        else {
            return Ok(BuildOutcomeKind::Cancelled);
        };

        // 3.) Constuct the payload
        let payload = FlashblocksPayloadV1::try_from(&latest_outcome.1)?;

        if let Err(err) = self.tx.send(payload) {
            error!(target: "payload_builder", %err, "failed to send flashblock payload");
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(
            self.block_time as usize / self.flashblock_interval as usize,
        );

        // spawn a task to schedule when the next flashblock job should be started/cancelled
        self.spawn_flashblock_job_manager(tx, cancel.clone());
        let notify = tokio::task::block_in_place(|| rx.blocking_recv());

        loop {
            match notify {
                Some(()) => {
                    debug!(target: "payload_builder", id=%ctx.attributes().payload_id(),  "building flashblock {flashblock_idx}");

                    let Some((info, outcome)) = self.build_flashblock(
                        &mut state,
                        &state_provider,
                        ctx,
                        gas_limit,
                        flashblock_idx,
                    )?
                    else {
                        break;
                    };

                    let payload = FlashblocksPayloadV1::try_from(&outcome)?;

                    if let Err(err) = self.tx.send(payload) {
                        error!(target: "payload_builder", %err, "failed to send flashblock payload");
                    }
                    latest_outcome = (info, outcome);
                }
                // tx was dropped, resolve the most recent payload
                None => {
                    break;
                }
            }
            flashblock_idx += 1;
        }

        let (
            info,
            FlashblockBuildOutcome::<N> {
                outcome:
                    BlockBuilderOutcome {
                        hashed_state,
                        execution_result,
                        trie_updates,
                        block,
                    },
                ..
            },
        ) = latest_outcome;

        let sealed_block = Arc::new(block.sealed_block().clone());
        debug!(target: "payload_builder", id=%ctx.attributes().payload_id(), sealed_block_header = ?sealed_block.header(), "sealed built block");

        let execution_outcome = ExecutionOutcome::new(
            state.take_bundle(),
            vec![execution_result.receipts],
            block.header().number(),
            Vec::new(),
        );

        // create the executed block data
        let executed: ExecutedBlockWithTrieUpdates<N> = ExecutedBlockWithTrieUpdates {
            block: ExecutedBlock {
                recovered_block: Arc::new(block),
                execution_output: Arc::new(execution_outcome),
                hashed_state: Arc::new(hashed_state),
            },
            trie: ExecutedTrieUpdates::Present(Arc::new(trie_updates)),
        };

        let no_tx_pool = ctx.attributes().no_tx_pool;

        let payload = OpBuiltPayload::new(
            ctx.payload_id(),
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

    /// Spawns a task responsible for cancelling, and initiating building of new flashblock payloads.
    ///
    /// A Flashblock job will be cancelled under the following conditions:
    /// - If the parent `cancel` is dropped.
    /// - If the `flashblock_interval` has been exceeded.
    /// - If the `max_flashblocks` has been exceeded.
    fn spawn_flashblock_job_manager(
        &self,
        tx: tokio::sync::mpsc::Sender<()>,
        cancel: CancelOnDrop,
    ) {
        let block_time = self.block_time;
        let flashblock_interval = self.flashblock_interval;
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let cancelled = async || loop {
                if cancel.is_cancelled() {
                    break;
                }
            };
            let mut flashblock_interval =
                tokio::time::interval(Duration::from_millis(flashblock_interval));
            let mut block_interval = tokio::time::interval(Duration::from_millis(block_time));

            let _ = tx.send(());

            tokio::select! {
                _ = block_interval.tick() => {
                    // block interval exceeded, cancel the current job
                    // and drop the sender to resolve the most recent payload.
                    drop(tx);
                },
                _ = async {
                    loop {
                        let tx = tx.clone();
                        tokio::select! {
                            _ = flashblock_interval.tick() => {
                                // queue the next flashblock job, but there's no benefit in cancelling the current job
                                // here we are exceeding `flashblock_interval` on the current job, but we want to give it `block_time` to finish.
                                // because the next job will also exceed `flashblock_interval`.
                                let _ = tx.send(());
                            },
                            _ = cancelled() => {
                                // parent cancel was dropped by the payload jobs generator
                                // cancel the child job, and drop the sender
                                drop(tx);
                                break;
                            },
                        }
                    }
                } => {
                    // either the parent payload job was cancelled, or all flashblocks were built.
                    // in either case, drop the sender to resolve the most recent payload.
                    drop(tx);
                }
            }
        });
    }

    fn build_flashblock<EvmConfig, ChainSpec, N, Ctx, DB, Tx>(
        &mut self,
        db: &mut State<DB>,
        state_provider: impl StateProvider,
        ctx: &Ctx,
        gas_limit: u64,
        flashblock_idx: u64,
    ) -> Result<Option<(ExecutionInfo, FlashblockBuildOutcome<N>)>, PayloadBuilderError>
    where
        EvmConfig: ConfigureEvm<Primitives = N, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
        ChainSpec: EthChainSpec,
        DB: Database<Error = ProviderError>,
        N: OpPayloadPrimitives,
        Tx: MaybeInteropTransaction + PoolTransaction<Consensus = N::SignedTx>,
        Txs: PayloadTransactions<Transaction = Tx>,
        Ctx: PayloadBuilderCtx<Evm = EvmConfig, ChainSpec = ChainSpec, Transaction = Tx>,
    {
        let mut builder = ctx.block_builder(db)?;

        // Execute the sequencer transactions, and pre-execution changes only once on the first block.
        // `db` will retain the bundle state as a pre-state for all subsequent flashblocks, so we only need to apply the pre-execution changes once.
        builder.apply_pre_execution_changes().map_err(|err| {
            warn!(target: "payload_builder", %err, "failed to apply pre-execution changes");
            PayloadBuilderError::Internal(err.into())
        })?;

        let mut info = ctx.execute_sequencer_transactions(&mut builder)?;

        if !ctx.attributes().no_tx_pool {
            // TODO: builder doesn't have to be &mut here, we could use `.env()` if such method existed
            let best_txs = (self.best)(ctx.best_transaction_attributes(builder.evm_mut().block()));
            let mut best_txs =
                RetainingBestTxs::new(best_txs).with_prev(self.executed_txs.drain(..).collect());

            if ctx
                .execute_best_transactions(&mut info, &mut builder, best_txs.guard(), gas_limit)?
                .is_some()
            {
                return Ok(None);
            }

            let (prev, observed) = best_txs.take_observed();

            self.executed_txs.extend_from_slice(&prev);
            self.executed_txs.extend_from_slice(&observed);
        }

        let outcome = builder.finish(&state_provider)?;

        let bundle_state = db.take_bundle();
        let outcome = FlashblockBuildOutcome::<N> {
            bundle_state,
            current_flashblock_offset: self.executed_txs.len() as u64,
            outcome: outcome,
            total_flashblocks: flashblock_idx,
            attributes: ctx.attributes().clone(),
        };

        Ok(Some((info, outcome)))
    }
}
