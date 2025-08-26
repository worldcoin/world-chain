use crate::{
    builder::{
        executor::{FlashblocksBlockBuilder, FlashblocksBlockExecutor},
        payload_txns::BestPayloadTxns,
    },
    PayloadBuilderCtx, PayloadBuilderCtxBuilder,
};

use alloy_consensus::{BlockHeader, Header};
use alloy_op_evm::OpEvm;
use eyre::eyre::eyre;
use op_alloy_consensus::OpTxEnvelope;
use reth::{
    api::{BuiltPayload, PayloadBuilderAttributes, PayloadBuilderError},
    chainspec::EthChainSpec,
    revm::{database::StateProviderDatabase, State},
};
use reth_basic_payload_builder::{BuildArguments, BuildOutcome, BuildOutcomeKind};
use reth_basic_payload_builder::{MissingPayloadBehaviour, PayloadBuilder, PayloadConfig};
use reth_chain_state::{ExecutedBlock, ExecutedBlockWithTrieUpdates, ExecutedTrieUpdates};
use reth_evm::{
    execute::{BlockBuilder, BlockBuilderOutcome},
    precompiles::PrecompilesMap,
    ConfigureEvm,
};
use reth_primitives::transaction::SignedTransaction;
use reth_primitives::{NodePrimitives, Recovered};

use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{
    txpool::OpPooledTx, OpEvmConfig, OpNextBlockEnvAttributes, OpRethReceiptBuilder,
};
use reth_optimism_payload_builder::{builder::ExecutionInfo, config::OpBuilderConfig};
use reth_optimism_payload_builder::{
    builder::OpPayloadTransactions,
    payload::{OpBuiltPayload, OpPayloadBuilderAttributes},
};
use reth_optimism_primitives::{OpPrimitives, OpReceipt, OpTransactionSigned};
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_provider::{ChainSpecProvider, ExecutionOutcome, StateProvider, StateProviderFactory};

use reth::api::BlockBody;
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use revm::{context::ContextTr, database::BundleState, inspector::NoOpInspector};
use std::{fmt::Debug, sync::Arc};
use tracing::{debug, span, warn};

pub mod executor;
pub mod payload_txns;
pub mod traits;

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
    /// Iterator over best transactions from the pool.
    pub best_transactions: Txs,
    /// Context builder for the payload.
    pub ctx_builder: CtxBuilder,
}

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
            ctx_builder: self.ctx_builder.clone(),
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
            config,
            cached_reads,
            cancel,
            best_payload,
        } = args;

        let ctx = self.ctx_builder.build(
            self.evm_config.clone(),
            self.config.da_config.clone(),
            self.client.chain_spec(),
            config,
            &cancel,
            best_payload.clone(),
        );

        let builder = FlashblockBuilder::new(best);
        let state_provider = self.client.state_by_block_hash(ctx.parent().hash())?;

        if ctx.attributes().no_tx_pool {
            builder.build(&state_provider, &ctx, best_payload.clone())
        } else {
            // sequencer mode we can reuse cachedreads from previous runs
            builder.build(&state_provider, &ctx, best_payload)
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
        config: PayloadConfig<Self::Attributes, Header>,
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
}

impl<'a, Txs> FlashblockBuilder<'a, Txs>
where
    Txs: PayloadTransactions,
{
    #[allow(clippy::too_many_arguments)]
    /// Creates a new [`FlashblockBuilder`].
    pub fn new(best: impl Fn(BestTransactionsAttributes) -> Txs + Send + Sync + 'a) -> Self {
        Self {
            best: Box::new(best),
        }
    }
}

impl<'a, Txs> FlashblockBuilder<'_, Txs>
where
    Txs: PayloadTransactions,
{
    /// Builds the payload on top of the state.
    pub fn build<Ctx>(
        self,
        state_provider: impl StateProvider,
        ctx: &Ctx,
        best_payload: Option<OpBuiltPayload>,
    ) -> Result<BuildOutcomeKind<OpBuiltPayload>, PayloadBuilderError>
    where
        Txs: PayloadTransactions,
        Txs::Transaction: OpPooledTx,
        Ctx: PayloadBuilderCtx<
            Evm = OpEvmConfig,
            Transaction = Txs::Transaction,
            ChainSpec = OpChainSpec,
        >,
    {
        let Self { best } = self;
        let span = span!(
            tracing::Level::INFO,
            "flashblock_builder",
            id = %ctx.payload_id(),
        );

        let _enter = span.enter();

        debug!(target: "payload_builder", "building new payload");

        let state = StateProviderDatabase::new(&state_provider);

        // 1. Prepare the db
        let (bundle, receipts, transactions, gas_used) = if let Some(payload) = &best_payload {
            // if we have a best payload we will always have a bundle
            let execution_result = &payload
                .executed_block()
                .ok_or(PayloadBuilderError::MissingPayload)?
                .execution_output;

            let receipts = execution_result
                .receipts
                .iter()
                .flatten()
                .cloned()
                .collect();

            let transactions = payload
                .block()
                .body()
                .transactions_iter()
                .cloned()
                .map(|tx| {
                    tx.try_into_recovered()
                        .map_err(|_| PayloadBuilderError::Other(eyre!("tx recovery failed").into()))
                })
                .collect::<Result<Vec<_>, _>>()?;

            (
                execution_result.bundle.clone(),
                receipts,
                transactions,
                Some(payload.block().gas_used()),
            )
        } else {
            (BundleState::default(), Vec::new(), Vec::new(), None)
        };

        let gas_limit = ctx
            .attributes()
            .gas_limit
            .unwrap_or(ctx.parent().gas_limit)
            .saturating_sub(gas_used.unwrap_or(0));

        let bundle_is_empty = bundle.is_empty();

        let mut state = State::builder()
            .with_database(state)
            .with_bundle_prestate(bundle)
            .with_bundle_update()
            .build();

        // 2. Create the block builder
        let mut builder =
            Self::block_builder(&mut state, transactions.clone(), receipts, gas_used, ctx)?;

        // Only execute the sequencer transactions on the first payload. The sequencer transactions
        // will already be in the [`BundleState`] at this point if the `bundle` non-empty.
        let mut info = if bundle_is_empty {
            // 3. apply pre-execution changes
            builder.apply_pre_execution_changes()?;

            // 4. Execute Deposit transactions
            ctx.execute_sequencer_transactions(&mut builder)
                .map_err(PayloadBuilderError::other)?
        } else {
            ExecutionInfo::default()
        };

        // 5. Execute transactions from the tx-pool, draining any transactions seen in previous
        // flashblocks
        if !ctx.attributes().no_tx_pool {
            let best_txs = best(ctx.best_transaction_attributes(builder.evm_mut().block()));
            let mut best_txns = BestPayloadTxns::new(best_txs)
                .with_prev(transactions.iter().map(|tx| *tx.hash()).collect::<Vec<_>>());

            if ctx
                .execute_best_transactions(&mut info, &mut builder, best_txns.guard(), gas_limit)?
                .is_none()
            {
                warn!(target: "payload_builder", "payload build cancelled");
                if let Some(best_payload) = best_payload {
                    // we can return the previous best payload since we didn't include any new txs
                    return Ok(BuildOutcomeKind::Freeze(best_payload));
                } else {
                    return Err(PayloadBuilderError::MissingPayload);
                }
            }
        }

        // 6. Build the block
        let build_outcome = builder.finish(&state_provider)?;

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

        if ctx.attributes().no_tx_pool {
            // if `no_tx_pool` is set only transactions from the payload attributes will be included
            // in the payload. In other words, the payload is deterministic and we can
            // freeze it once we've successfully built it.
            Ok(BuildOutcomeKind::Freeze(payload))
        } else {
            // always better since we are re-using built payloads
            Ok(BuildOutcomeKind::Better { payload })
        }
    }

    pub fn block_builder<Ctx, DB, N, Tx>(
        db: &'a mut State<DB>,
        transactions: Vec<Recovered<N::SignedTx>>,
        receipts: Vec<N::Receipt>,
        cumulative_gas_used: Option<u64>,
        ctx: &'a Ctx,
    ) -> Result<
        FlashblocksBlockBuilder<'a, N, OpEvm<&'a mut State<DB>, NoOpInspector, PrecompilesMap>>,
        PayloadBuilderError,
    >
    where
        // Tx: PoolTransaction<Consensus = OpTransactionSigned> + OpPooledTx,
        Tx: PoolTransaction + OpPooledTx,
        N: NodePrimitives<
            Block = alloy_consensus::Block<OpTransactionSigned>,
            BlockHeader = alloy_consensus::Header,
            Receipt = OpReceipt,
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
            execution_ctx.clone(),
            ctx.spec().clone(),
            OpRethReceiptBuilder::default(),
        )
        .with_receipts(receipts);

        if let Some(cumulative_gas_used) = cumulative_gas_used {
            executor = executor.with_gas_used(cumulative_gas_used)
        }

        Ok(FlashblocksBlockBuilder::new(
            execution_ctx,
            ctx.parent(),
            executor,
            transactions,
            Arc::new(ctx.spec().clone()),
        ))
    }
}
