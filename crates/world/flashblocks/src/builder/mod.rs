use crate::{
    payload::{ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1, FlashblocksPayloadV1},
    payload_builder_ctx::{PayloadBuilderCtx, PayloadBuilderCtxBuilder},
};
use alloy_eips::Encodable2718;
use alloy_primitives::U256;
use eyre::eyre::ContextCompat;
use retaining_payload_txs::RetainingBestTxs;
use reth::{
    api::{PayloadBuilderAttributes, PayloadBuilderError},
    chainspec::EthChainSpec,
    revm::{database::StateProviderDatabase, Database, State},
};
use reth_basic_payload_builder::{BuildArguments, BuildOutcome, BuildOutcomeKind};
use reth_basic_payload_builder::{MissingPayloadBehaviour, PayloadBuilder, PayloadConfig};
use reth_chain_state::{ExecutedBlock, ExecutedBlockWithTrieUpdates};
use reth_evm::{
    execute::{BlockBuilder, BlockBuilderOutcome},
    ConfigureEvm, Evm,
};
use reth_optimism_consensus::calculate_receipt_root_no_memo_optimism;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{txpool::interop::MaybeInteropTransaction, OpNextBlockEnvAttributes};
use reth_optimism_payload_builder::{
    builder::ExecutionInfo, config::OpBuilderConfig, OpPayloadPrimitives,
};
use reth_optimism_payload_builder::{
    builder::OpPayloadTransactions,
    payload::{OpBuiltPayload, OpPayloadBuilderAttributes},
};
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_primitives::NodePrimitives;
use reth_provider::{
    ChainSpecProvider, ExecutionOutcome, HashedPostStateProvider, ProviderError, StateProvider,
    StateProviderFactory, StateRootProvider, StorageRootProvider,
};
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tracing::{debug, warn};

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

impl<Pool, Client, Evm, CtxBuilder, Txs>
    FlashblocksPayloadBuilder<Pool, Client, Evm, CtxBuilder, Txs>
where
    Pool: TransactionPool<
        Transaction: MaybeInteropTransaction
                         + PoolTransaction<Consensus = <Evm::Primitives as NodePrimitives>::SignedTx>,
    >,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec: EthChainSpec + OpHardforks>,
    Evm: ConfigureEvm<Primitives: OpPayloadPrimitives, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
    Txs: OpPayloadTransactions<Pool::Transaction>,
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
    fn build_payload<F, T>(
        &self,
        args: BuildArguments<
            OpPayloadBuilderAttributes<<Evm::Primitives as NodePrimitives>::SignedTx>,
            OpBuiltPayload<Evm::Primitives>,
        >,
        best: F,
    ) -> Result<BuildOutcome<OpBuiltPayload<Evm::Primitives>>, PayloadBuilderError>
    where
        F: Send + Sync + Fn(BestTransactionsAttributes) -> T,
        T: PayloadTransactions<Transaction = Pool::Transaction>,
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
            cancel,
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
            builder.build(state, &state_provider, &ctx)
        } else {
            // sequencer mode we can reuse cachedreads from previous runs
            builder.build(cached_reads.as_db_mut(state), &state_provider, &ctx)
        }
        .map(|out| out.with_cached_reads(cached_reads))
    }
}

impl<Pool, Client, Evm, N, CtxBuilder, Txs> PayloadBuilder
    for FlashblocksPayloadBuilder<Pool, Client, Evm, CtxBuilder, Txs>
where
    Client: Clone + StateProviderFactory + ChainSpecProvider<ChainSpec: EthChainSpec + OpHardforks>,
    N: OpPayloadPrimitives,
    Pool: TransactionPool<
        Transaction: MaybeInteropTransaction + PoolTransaction<Consensus = N::SignedTx>,
    >,
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
        config: PayloadConfig<Self::Attributes>,
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

    total_gas_limit: u64,
    num_flashblocks: u64,
    flashblock_gas_limit: u64,

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

            total_gas_limit: 0,
            num_flashblocks: 0,
            flashblock_gas_limit: 0,

            executed_txs: Vec::new(),
        }
    }
}

impl<Txs> FlashblockBuilder<'_, Txs>
where
    Txs: PayloadTransactions,
{
    /// Builds the payload on top of the state.
    pub fn build<EvmConfig, ChainSpec, N, Ctx, Tx>(
        mut self,
        db: impl Database<Error = ProviderError>,
        state_provider: impl StateProvider,
        ctx: &Ctx,
    ) -> Result<BuildOutcomeKind<OpBuiltPayload<N>>, PayloadBuilderError>
    where
        EvmConfig: ConfigureEvm<Primitives = N, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
        ChainSpec: EthChainSpec + OpHardforks,
        N: OpPayloadPrimitives,
        Tx: MaybeInteropTransaction + PoolTransaction<Consensus = N::SignedTx>,
        Txs: PayloadTransactions<Transaction = Tx>,
        Ctx: PayloadBuilderCtx<Evm = EvmConfig, ChainSpec = ChainSpec, Transaction = Tx>,
    {
        debug!(target: "payload_builder", id=%ctx.payload_id(), parent_header = ?ctx.parent().hash(), parent_number = ctx.parent().number, "building new payload");

        // NOTE: Do we need to init this every time?
        let mut db = State::builder()
            .with_database(db)
            .with_bundle_update()
            .build();

        // 1. Setup relevant variables
        let total_gas_limit = ctx.attributes().gas_limit.unwrap_or(ctx.parent().gas_limit);
        let num_flashblocks = self.block_time / self.flashblock_interval;
        let flashblock_gas_limit = total_gas_limit / num_flashblocks;

        self.total_gas_limit = total_gas_limit;
        self.num_flashblocks = num_flashblocks;
        self.flashblock_gas_limit = flashblock_gas_limit;

        // 1. Build initial flashblock
        let now = Instant::now();

        let outcome = self
            .build_flashblock(&mut db, &state_provider, ctx, 0)?
            .expect("Missing outcome");

        let payload = Self::create_payload(&outcome, ctx, 0).unwrap();
        let _ = self.tx.send(payload);

        let elapsed = now.elapsed().as_millis() as u64;
        let sleep_time = self.flashblock_interval.saturating_sub(elapsed);
        std::thread::sleep(Duration::from_millis(sleep_time));

        if ctx.attributes().no_tx_pool {
            // TODO: What do do here? Clearly no reason to produce flashblocks
            // should we just return early?
        }

        // Produce intermediate flashblocks
        for flashblock_idx in 1..num_flashblocks - 1 {
            let now = Instant::now();

            self.build_flashblock(&mut db, &state_provider, ctx, flashblock_idx)?;

            let mut payload = Self::create_payload(&outcome, ctx, 0).unwrap();
            payload.base = None;
            let _ = self.tx.send(payload);

            let elapsed = now.elapsed().as_millis() as u64;
            let sleep_time = self.flashblock_interval.saturating_sub(elapsed);
            std::thread::sleep(Duration::from_millis(sleep_time));
        }

        // Produce the final block
        let Some(outcome) =
            self.build_flashblock(&mut db, &state_provider, ctx, num_flashblocks - 1)?
        else {
            return Ok(BuildOutcomeKind::Cancelled);
        };
        let mut payload = Self::create_payload(&outcome, ctx, 0).unwrap();
        payload.base = None;
        let _ = self.tx.send(payload);

        let (info, outcome, _diff_txs) = outcome;

        // check if the new payload is even more valuable
        if !ctx.is_better_payload(info.total_fees) {
            // can skip building the block
            return Ok(BuildOutcomeKind::Aborted {
                fees: info.total_fees,
            });
        }

        let BlockBuilderOutcome {
            execution_result,
            hashed_state,
            trie_updates,
            block,
        } = outcome;

        let sealed_block = Arc::new(block.sealed_block().clone());
        debug!(target: "payload_builder", id=%ctx.attributes().payload_id(), sealed_block_header = ?sealed_block.header(), "sealed built block");

        let execution_outcome = ExecutionOutcome::new(
            db.take_bundle(),
            vec![execution_result.receipts],
            block.number,
            Vec::new(),
        );

        // create the executed block data
        let executed: ExecutedBlockWithTrieUpdates<N> = ExecutedBlockWithTrieUpdates {
            block: ExecutedBlock {
                recovered_block: Arc::new(block),
                execution_output: Arc::new(execution_outcome),
                hashed_state: Arc::new(hashed_state),
            },
            trie: Arc::new(trie_updates),
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

    fn build_flashblock<EvmConfig, ChainSpec, N, Ctx, DB, Tx>(
        &mut self,
        db: &mut State<DB>,
        state_provider: impl StateProvider,
        ctx: &Ctx,
        idx: u64,
    ) -> Result<Option<(ExecutionInfo, BlockBuilderOutcome<N>, Vec<Tx>)>, PayloadBuilderError>
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

        builder.apply_pre_execution_changes().map_err(|err| {
            warn!(target: "payload_builder", %err, "failed to apply pre-execution changes");
            PayloadBuilderError::Internal(err.into())
        })?;

        let mut info = ctx.execute_sequencer_transactions(&mut builder)?;

        let gas_limit = (idx + 1) * self.flashblock_gas_limit;

        let mut diff_txs = vec![];

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

            diff_txs.extend_from_slice(&observed);

            self.executed_txs.extend_from_slice(&prev);
            self.executed_txs.extend_from_slice(&observed);
        }

        // builder.executor_mut().
        let outcome = builder.finish(&state_provider)?;

        Ok(Some((info, outcome, diff_txs)))
    }

    pub fn create_payload<EvmConfig, ChainSpec, N, Ctx, Tx>(
        (_exec_info, outcome, diff_txs): &(ExecutionInfo, BlockBuilderOutcome<N>, Vec<Tx>),
        ctx: &Ctx,
        idx: u64,
    ) -> eyre::Result<FlashblocksPayloadV1>
    where
        EvmConfig: ConfigureEvm<Primitives = N, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
        ChainSpec: EthChainSpec + OpHardforks,
        N: OpPayloadPrimitives,
        Tx: MaybeInteropTransaction + PoolTransaction<Consensus = N::SignedTx>,
        Txs: PayloadTransactions<Transaction = Tx>,
        Ctx: PayloadBuilderCtx<Evm = EvmConfig, ChainSpec = ChainSpec, Transaction = Tx>,
    {
        let block_number = ctx.parent().number + 1;

        let block_header = outcome.block.sealed_block().header();

        let base = ExecutionPayloadBaseV1 {
            parent_beacon_block_root: ctx
                .parent()
                .parent_beacon_block_root
                .context("Missing parent beacon block root")?,
            parent_hash: ctx.parent().hash(),
            fee_recipient: ctx.attributes().suggested_fee_recipient(),
            prev_randao: ctx.attributes().prev_randao(),
            block_number,
            gas_limit: ctx.attributes().gas_limit.context("Missing gas limit")?,
            timestamp: ctx.attributes().timestamp(),
            extra_data: block_header.extra_data.clone(),
            base_fee_per_gas: U256::from(
                block_header
                    .base_fee_per_gas
                    .context("Missing base fee per gas")?,
            ),
        };

        let transactions = diff_txs
            .iter()
            .cloned()
            .map(|tx| tx.into_consensus().encoded_2718())
            .map(|tx_bytes| tx_bytes.into())
            .collect();

        let withdrawals = ctx.attributes().payload_attributes.withdrawals.clone().0;

        let diff = ExecutionPayloadFlashblockDeltaV1 {
            state_root: block_header.state_root,
            receipts_root: block_header.receipts_root,
            logs_bloom: block_header.logs_bloom,
            gas_used: block_header.gas_used,

            block_hash: outcome.block.sealed_block().hash(),

            transactions,
            withdrawals,
        };

        let payload = FlashblocksPayloadV1 {
            payload_id: ctx.attributes().payload_id(),
            index: idx,
            base: Some(base),
            diff,
            // TODO: Fill out metadata
            metadata: serde_json::Value::Null,
        };

        Ok(payload)
    }
}
