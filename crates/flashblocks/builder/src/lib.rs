use crate::{
    executor::{FlashblocksBlockBuilder, FlashblocksBlockExecutor},
    payload_txns::BestPayloadTxns,
    traits::{
        context::PayloadBuilderCtx, context_builder::PayloadBuilderCtxBuilder,
        payload_builder::FlashblockPayloadBuilder,
    },
};

use alloy_consensus::Transaction;
use alloy_consensus::{BlockHeader, Header};
use alloy_op_evm::OpEvm;
use alloy_primitives::U256;
use alloy_rlp::Encodable;
use eyre::eyre::eyre;
use op_alloy_consensus::OpTxEnvelope;
use reth::{
    api::{BuiltPayload, PayloadBuilderAttributes, PayloadBuilderError},
    chainspec::EthChainSpec,
    revm::{database::StateProviderDatabase, State},
};
use reth_basic_payload_builder::{BuildArguments, BuildOutcome, BuildOutcomeKind};
use reth_basic_payload_builder::{MissingPayloadBehaviour, PayloadBuilder, PayloadConfig};
use reth_chain_state::ExecutedBlock;
use reth_evm::{
    execute::{BlockBuilder, BlockBuilderOutcome},
    precompiles::PrecompilesMap,
    ConfigureEvm, Database,
};
use reth_primitives::transaction::SignedTransaction;
use reth_primitives::{NodePrimitives, Recovered};
use tracing::error;

use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{
    txpool::OpPooledTx, OpEvmConfig, OpNextBlockEnvAttributes, OpRethReceiptBuilder,
};
use reth_optimism_payload_builder::{
    builder::ExecutionInfo, config::OpBuilderConfig, OpAttributes,
};
use reth_optimism_payload_builder::{
    builder::OpPayloadTransactions,
    payload::{OpBuiltPayload, OpPayloadBuilderAttributes},
};
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use reth_payload_util::{NoopPayloadTransactions, PayloadTransactions};
use reth_provider::{
    ChainSpecProvider, ExecutionOutcome, ProviderError, StateProvider, StateProviderFactory,
};

use reth::api::BlockBody;
use reth_transaction_pool::{BestTransactionsAttributes, PoolTransaction, TransactionPool};
use revm::{context::ContextTr, database::BundleState, inspector::NoOpInspector};
use std::{fmt::Debug, sync::Arc};
use tracing::{debug, span, trace, warn};

pub mod executor;
pub mod payload_txns;
pub mod traits;

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
        committed_payload: Option<OpBuiltPayload>,
    ) -> Result<BuildOutcome<OpBuiltPayload>, PayloadBuilderError>
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
            self.config.da_config.clone(),
            config,
            &cancel,
            best_payload.clone(),
        );

        let builder = FlashblockBuilder::new(best);
        let state_provider = self.client.state_by_block_hash(ctx.parent().hash())?;
        let db = StateProviderDatabase::new(&state_provider);

        if ctx.attributes().no_tx_pool {
            builder.build(
                self.pool.clone(),
                db,
                &state_provider,
                &ctx,
                committed_payload,
            )
        } else {
            // sequencer mode we can reuse cachedreads from previous runs
            builder.build(
                self.pool.clone(),
                cached_reads.as_db_mut(db),
                &state_provider,
                &ctx,
                committed_payload,
            )
        }
        .map(|out| out.with_cached_reads(cached_reads))
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
        committed_payload: Option<<Self as PayloadBuilder>::BuiltPayload>,
    ) -> Result<BuildOutcome<<Self as PayloadBuilder>::BuiltPayload>, PayloadBuilderError> {
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
    pub fn build<Ctx, Pool>(
        self,
        pool: Pool,
        db: impl Database<Error = ProviderError>,
        state_provider: impl StateProvider,
        ctx: &Ctx,
        committed_payload: Option<OpBuiltPayload>,
    ) -> Result<BuildOutcomeKind<OpBuiltPayload>, PayloadBuilderError>
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
        let Self { best } = self;
        let span = span!(
            tracing::Level::INFO,
            "flashblock_builder",
            id = %ctx.payload_id(),
        );

        let _enter = span.enter();

        debug!(target: "flashblocks::payload_builder", "building new payload");

        // 1. Prepare the db
        let (bundle, receipts, transactions, gas_used, fees) = if let Some(payload) =
            &committed_payload
        {
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

            trace!(target: "flashblocks::payload_builder", "using best payload");

            (
                execution_result.bundle.clone(),
                receipts,
                transactions,
                Some(payload.block().gas_used()),
                payload.fees(),
            )
        } else {
            (BundleState::default(), vec![], vec![], None, U256::ZERO)
        };

        let gas_limit = ctx
            .attributes()
            .gas_limit
            .unwrap_or(ctx.parent().gas_limit)
            .saturating_sub(gas_used.unwrap_or(0));

        let mut state = State::builder()
            .with_database(db)
            .with_bundle_prestate(bundle)
            .with_bundle_update()
            .build();

        // 2. Create the block builder
        let mut builder =
            Self::block_builder(&mut state, transactions.clone(), receipts, gas_used, ctx)?;

        // Only execute the sequencer transactions on the first payload. The sequencer transactions
        // will already be in the [`BundleState`] at this point if the `best_payload` is set.
        let mut info = if committed_payload.is_none() {
            // 3. apply pre-execution changes
            builder.apply_pre_execution_changes()?;

            // 4. Execute Deposit transactions
            ctx.execute_sequencer_transactions(&mut builder)
                .map_err(PayloadBuilderError::other)?
        } else {
            // bundle is non-empty - execute any transactions from the attributes that do not exist on the `best_payload`
            let unexecuted_txs: Vec<Recovered<OpTxEnvelope>> = ctx
                .attributes()
                .sequencer_transactions()
                .iter()
                .filter_map(|attr_tx| {
                    let tx_hash = attr_tx.1.hash();
                    if !transactions.iter().any(|tx| *tx.hash() == *tx_hash) {
                        Some(attr_tx.1.clone().try_into_recovered().map_err(|_| {
                            PayloadBuilderError::Other(eyre!("tx recovery failed").into())
                        }))
                    } else {
                        None
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;

            let mut execution_info = ExecutionInfo::default();
            let base_fee = builder.evm_mut().block().basefee;

            for tx in unexecuted_txs {
                match builder.execute_transaction(tx.clone()) {
                    Ok(gas_used) => {
                        execution_info.cumulative_gas_used += gas_used;
                        execution_info.cumulative_da_bytes_used += tx.length() as u64;

                        if !tx.is_deposit() {
                            let miner_fee = tx
                                .effective_tip_per_gas(base_fee)
                                .expect("fee is always valid; execution succeeded");
                            execution_info.total_fees +=
                                U256::from(miner_fee) * U256::from(gas_used);
                        }
                    }

                    Err(e) => {
                        error!(target: "flashblocks::payload_builder", %e, "spend nullifiers transaction failed")
                    }
                }
            }

            execution_info
        };

        // 5. Execute transactions from the tx-pool, draining any transactions seen in previous
        // flashblocks
        if !ctx.attributes().no_tx_pool {
            let best_txs = best(ctx.best_transaction_attributes(builder.evm_mut().block()));
            let mut best_txns = BestPayloadTxns::new(best_txs)
                .with_prev(transactions.iter().map(|tx| *tx.hash()).collect::<Vec<_>>());

            if ctx
                .execute_best_transactions(
                    pool,
                    &mut info,
                    &mut builder,
                    best_txns.guard(),
                    gas_limit,
                )?
                .is_none()
            {
                warn!(target: "flashblocks::payload_builder", "payload build cancelled");
                if let Some(best_payload) = committed_payload {
                    // we can return the previous best payload since we didn't include any new txs
                    return Ok(BuildOutcomeKind::Freeze(best_payload));
                } else {
                    return Err(PayloadBuilderError::MissingPayload);
                }
            }

            // check if the new payload is even more valuable
            if !ctx.is_better_payload(info.total_fees) {
                // can skip building the block
                return Ok(BuildOutcomeKind::Aborted {
                    fees: info.total_fees,
                });
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
        let executed = ExecutedBlock {
            recovered_block: Arc::new(block),
            execution_output: Arc::new(execution_outcome),
            hashed_state: Arc::new(hashed_state),
            trie_updates: Arc::new(trie_updates),
        };

        let payload = OpBuiltPayload::new(
            ctx.payload_id(),
            sealed_block,
            info.total_fees + fees,
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

    #[expect(clippy::type_complexity)]
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
            .context_for_next_block(ctx.parent(), attributes)
            .map_err(PayloadBuilderError::other)?;

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
