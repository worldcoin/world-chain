use alloy_consensus::{Block, Transaction, TxReceipt};
use alloy_eips::{eip2718::WithEncoded, eip4895::Withdrawals, Decodable2718, Encodable2718};
use alloy_op_evm::{
    block::{receipt_builder::OpReceiptBuilder, OpTxEnv},
    OpBlockExecutionCtx, OpBlockExecutor, OpBlockExecutorFactory, OpEvmFactory,
};
use alloy_rpc_types_engine::PayloadId;
use eyre::eyre::OptionExt as _;
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use flashblocks_primitives::{p2p::AuthorizedPayload, primitives::FlashblocksPayloadV1};
use futures::StreamExt as _;
use op_alloy_consensus::{encode_holocene_extra_data, OpTxEnvelope};
use parking_lot::RwLock;
use reth::{
    core::primitives::Receipt,
    payload::EthPayloadBuilderAttributes,
    revm::{cancelled::CancelOnDrop, database::StateProviderDatabase, State},
};
use reth_basic_payload_builder::{BuildOutcomeKind, PayloadConfig};
use reth_chain_state::ExecutedBlock;
use reth_evm::{
    block::{
        BlockExecutionError, BlockExecutor, BlockExecutorFactory, BlockExecutorFor, CommitChanges,
        ExecutableTx,
    },
    execute::{
        BasicBlockBuilder, BlockAssembler, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome,
        ExecutorTx,
    },
    op_revm::{OpHaltReason, OpSpecId},
    Database, Evm, EvmFactory, FromRecoveredTx, FromTxWithEncoded, OnStateHook,
};
use reth_node_api::{BuiltPayload as _, Events, FullNodeTypes, NodeTypes};
use reth_node_builder::BuilderContext;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{
    OpBlockAssembler, OpBuiltPayload, OpEngineTypes, OpEvmConfig, OpPayloadBuilderAttributes,
    OpRethReceiptBuilder,
};
use reth_optimism_payload_builder::config::OpBuilderConfig;
use reth_optimism_primitives::{DepositReceipt, OpPrimitives, OpReceipt, OpTransactionSigned};
use reth_payload_util::BestPayloadTransactions;
use reth_primitives::{
    transaction::SignedTransaction, NodePrimitives, Recovered, RecoveredBlock, SealedHeader,
};
use reth_provider::{BlockExecutionResult, HeaderProvider, StateProvider, StateProviderFactory};
use reth_transaction_pool::TransactionPool;
use revm::{
    context::{
        result::{ExecutionResult, ResultAndState},
        BlockEnv,
    },
    database::{
        states::{
            bundle_state::BundleRetention,
            reverts::{AccountInfoRevert, Reverts},
        },
        BundleState,
    },
};
use std::{
    collections::{hash_map::Entry, HashMap},
    sync::Arc,
};
use tokio::sync::broadcast;
use tracing::{error, trace};

use crate::{FlashblockBuilder, PayloadBuilderCtxBuilder};
use flashblocks_primitives::flashblocks::{Flashblock, Flashblocks};

/// A Block Executor for Optimism that can load pre state from previous flashblocks.
pub struct FlashblocksBlockExecutor<Evm, R: OpReceiptBuilder, Spec> {
    inner: OpBlockExecutor<Evm, R, Spec>,
}

impl<'db, DB, E, R, Spec> FlashblocksBlockExecutor<E, R, Spec>
where
    DB: Database + 'db,
    E: Evm<
        DB = &'db mut State<DB>,
        Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction> + OpTxEnv,
    >,
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt>,
    Spec: OpHardforks + Clone,
{
    /// Creates a new [`OpBlockExecutor`].
    pub fn new(evm: E, ctx: OpBlockExecutionCtx, spec: Spec, receipt_builder: R) -> Self {
        let inner = OpBlockExecutor::new(evm, ctx, spec, receipt_builder);
        Self { inner }
    }

    /// Extends the [`BundleState`] of the executor with a specified pre-image.
    ///
    /// This should be used _only_ when initializing the executor
    pub fn with_bundle_prestate(mut self, pre_state: BundleState) -> Self {
        self.evm_mut().db_mut().bundle_state.extend(pre_state);
        self
    }

    /// Extends the receipts to reflect the aggregated execution result
    pub fn with_receipts(mut self, receipts: Vec<R::Receipt>) -> Self {
        self.inner.receipts.extend_from_slice(&receipts);
        self
    }

    /// Extends the gas used to reflect the aggregated execution result
    pub fn with_gas_used(mut self, gas_used: u64) -> Self {
        self.inner.gas_used += gas_used;
        self
    }
}

impl<'db, DB, E, R, Spec> BlockExecutor for FlashblocksBlockExecutor<E, R, Spec>
where
    DB: Database + 'db,
    E: Evm<
        DB = &'db mut State<DB>,
        Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction> + OpTxEnv,
    >,
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt>,
    Spec: OpHardforks,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Evm = E;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.inner.apply_pre_execution_changes()
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        f: impl FnOnce(&ExecutionResult<<Self::Evm as Evm>::HaltReason>) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        self.inner.execute_transaction_with_commit_condition(tx, f)
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<ResultAndState<<Self::Evm as Evm>::HaltReason>, BlockExecutionError> {
        self.inner.execute_transaction_without_commit(tx)
    }

    fn commit_transaction(
        &mut self,
        output: ResultAndState<<Self::Evm as Evm>::HaltReason>,
        tx: impl ExecutableTx<Self>,
    ) -> Result<u64, BlockExecutionError> {
        self.inner.commit_transaction(output, tx)
    }

    fn finish(self) -> Result<(Self::Evm, BlockExecutionResult<R::Receipt>), BlockExecutionError> {
        self.inner.finish()
    }

    fn set_state_hook(&mut self, hook: Option<Box<dyn OnStateHook>>) {
        self.inner.set_state_hook(hook)
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        &mut self.inner.evm
    }

    fn evm(&self) -> &Self::Evm {
        self.inner.evm()
    }
}

/// Ethereum block executor factory.
#[derive(Debug, Clone)]
pub struct FlashblocksBlockExecutorFactory {
    inner: OpBlockExecutorFactory<OpRethReceiptBuilder, OpChainSpec>,
    pre_state: Option<BundleState>,
}

impl FlashblocksBlockExecutorFactory {
    /// Creates a new [`OpBlockExecutorFactory`] with the given spec, [`EvmFactory`], and
    /// [`OpReceiptBuilder`].
    pub const fn new(
        receipt_builder: OpRethReceiptBuilder,
        spec: OpChainSpec,
        evm_factory: OpEvmFactory,
    ) -> Self {
        Self {
            inner: OpBlockExecutorFactory::new(receipt_builder, spec, evm_factory),
            pre_state: None,
        }
    }

    /// Exposes the chain specification.
    pub const fn spec(&self) -> &OpChainSpec {
        self.inner.spec()
    }

    /// Exposes the EVM factory.
    pub const fn evm_factory(&self) -> &OpEvmFactory {
        self.inner.evm_factory()
    }

    pub const fn take_bundle(&mut self) -> Option<BundleState> {
        self.pre_state.take()
    }

    /// Sets the pre-state for the block executor factory.
    pub fn set_pre_state(&mut self, pre_state: BundleState) {
        self.pre_state = Some(pre_state);
    }
}

impl BlockExecutorFactory for FlashblocksBlockExecutorFactory {
    type EvmFactory = OpEvmFactory;
    type ExecutionCtx<'a> = OpBlockExecutionCtx;
    type Transaction = OpTransactionSigned;
    type Receipt = OpReceipt;

    fn evm_factory(&self) -> &Self::EvmFactory {
        self.inner.evm_factory()
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: <OpEvmFactory as EvmFactory>::Evm<&'a mut State<DB>, I>,
        ctx: Self::ExecutionCtx<'a>,
    ) -> impl BlockExecutorFor<'a, Self, DB, I>
    where
        DB: Database + 'a,
        I: revm::Inspector<<OpEvmFactory as EvmFactory>::Context<&'a mut State<DB>>> + 'a,
    {
        if let Some(pre_state) = &self.pre_state {
            return FlashblocksBlockExecutor::new(
                evm,
                ctx,
                self.spec().clone(),
                OpRethReceiptBuilder::default(),
            )
            .with_bundle_prestate(pre_state.clone()); // TODO: Terrible clone here
        }

        FlashblocksBlockExecutor::new(
            evm,
            ctx,
            self.spec().clone(),
            OpRethReceiptBuilder::default(),
        )
    }
}

/// Block builder for Optimism.
#[derive(Debug)]
pub struct FlashblocksBlockAssembler {
    inner: OpBlockAssembler<OpChainSpec>,
}

impl FlashblocksBlockAssembler {
    /// Creates a new [`OpBlockAssembler`].
    pub const fn new(chain_spec: Arc<OpChainSpec>) -> Self {
        Self {
            inner: OpBlockAssembler::new(chain_spec),
        }
    }
}

impl FlashblocksBlockAssembler {
    /// Builds a block for `input` without any bounds on header `H`.
    pub fn assemble_block<
        F: for<'a> BlockExecutorFactory<
            ExecutionCtx<'a> = OpBlockExecutionCtx,
            Transaction: SignedTransaction,
            Receipt: Receipt + DepositReceipt,
        >,
        H,
    >(
        &self,
        input: BlockAssemblerInput<'_, '_, F, H>,
    ) -> Result<Block<F::Transaction>, BlockExecutionError> {
        self.inner.assemble_block(input)
    }
}

impl Clone for FlashblocksBlockAssembler {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<F> BlockAssembler<F> for FlashblocksBlockAssembler
where
    F: for<'a> BlockExecutorFactory<
        ExecutionCtx<'a> = OpBlockExecutionCtx,
        Transaction: SignedTransaction,
        Receipt: Receipt + DepositReceipt,
    >,
{
    type Block = Block<F::Transaction>;

    fn assemble_block(
        &self,
        input: BlockAssemblerInput<'_, '_, F>,
    ) -> Result<Self::Block, BlockExecutionError> {
        self.assemble_block(input)
    }
}

/// A wrapper around the [`BasicBlockBuilder`] for flashblocks.
pub struct FlashblocksBlockBuilder<'a, N: NodePrimitives, Evm> {
    pub inner: BasicBlockBuilder<
        'a,
        FlashblocksBlockExecutorFactory,
        FlashblocksBlockExecutor<Evm, OpRethReceiptBuilder, OpChainSpec>,
        OpBlockAssembler<OpChainSpec>,
        N,
    >,
}

impl<'a, N: NodePrimitives, Evm> FlashblocksBlockBuilder<'a, N, Evm> {
    /// Creates a new [`FlashblocksBlockBuilder`] with the given executor factory and assembler.
    pub fn new(
        ctx: OpBlockExecutionCtx,
        parent: &'a SealedHeader<N::BlockHeader>,
        executor: FlashblocksBlockExecutor<Evm, OpRethReceiptBuilder, OpChainSpec>,
        transactions: Vec<Recovered<N::SignedTx>>,
        chain_spec: Arc<OpChainSpec>,
    ) -> Self {
        Self {
            inner: BasicBlockBuilder {
                executor,
                assembler: OpBlockAssembler::new(chain_spec),
                ctx,
                parent,
                transactions,
            },
        }
    }
}

impl<'a, DB, N, E> BlockBuilder for FlashblocksBlockBuilder<'a, N, E>
where
    DB: Database + 'a,
    N: NodePrimitives<
        Receipt = OpReceipt,
        SignedTx = OpTransactionSigned,
        Block = alloy_consensus::Block<OpTransactionSigned>,
        BlockHeader = alloy_consensus::Header,
    >,
    E: Evm<
        DB = &'a mut State<DB>,
        Tx: FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned> + OpTxEnv,
        Spec = OpSpecId,
        HaltReason = OpHaltReason,
        BlockEnv = BlockEnv,
    >,
{
    type Primitives = N;
    type Executor = FlashblocksBlockExecutor<E, OpRethReceiptBuilder, OpChainSpec>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.inner.apply_pre_execution_changes()
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutorTx<Self::Executor>,
        f: impl FnOnce(
            &ExecutionResult<<<Self::Executor as BlockExecutor>::Evm as Evm>::HaltReason>,
        ) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        self.inner.execute_transaction_with_commit_condition(tx, f)
    }

    fn finish(
        self,
        state: impl StateProvider,
    ) -> Result<BlockBuilderOutcome<N>, BlockExecutionError> {
        let (evm, result) = self.inner.executor.finish()?;
        let (db, evm_env) = evm.finish();

        // merge all transitions into bundle state
        db.merge_transitions(BundleRetention::Reverts);

        // Flatten reverts into a single transition:
        // - per account: keep earliest `previous_status`
        // - per account: keep earliest non-`DoNothing` account-info revert
        // - per account+slot: keep earliest revert-to value
        // - per account: OR `wipe_storage`
        //
        // This keeps `bundle_state.reverts.len() == 1`, which matches the expectation that this
        // bundle represents a single block worth of changes even if we built multiple payloads.
        let flattened = flatten_reverts(&db.bundle_state.reverts);
        db.bundle_state.reverts = flattened;

        // calculate the state root
        let hashed_state = state.hashed_post_state(&db.bundle_state);
        let (state_root, trie_updates) = state
            .state_root_with_updates(hashed_state.clone())
            .map_err(BlockExecutionError::other)?;

        let (transactions, senders) = self
            .inner
            .transactions
            .into_iter()
            .map(|tx| tx.into_parts())
            .unzip();

        let block = self.inner.assembler.assemble_block(BlockAssemblerInput::<
            '_,
            '_,
            FlashblocksBlockExecutorFactory,
        >::new(
            evm_env,
            self.inner.ctx,
            self.inner.parent,
            transactions,
            &result,
            &db.bundle_state,
            &state,
            state_root,
        ))?;

        let block = RecoveredBlock::new_unhashed(block, senders);

        Ok(BlockBuilderOutcome {
            execution_result: result,
            hashed_state,
            trie_updates,
            block,
        })
    }

    fn executor_mut(&mut self) -> &mut Self::Executor {
        self.inner.executor_mut()
    }

    fn executor(&self) -> &Self::Executor {
        self.inner.executor()
    }

    fn into_executor(self) -> Self::Executor {
        self.inner.into_executor()
    }
}

/// Flattens a multi-transition [`Reverts`] into a single transition, merging per-account data.
///
/// Merge rules (iterate earliest -> latest):
/// - For each account, keep the **earliest** `previous_status`.
/// - For each account, keep the **earliest non-`DoNothing`** account-info revert.
/// - For each account+slot, keep the **earliest** `RevertToSlot`.
/// - For each account, OR `wipe_storage`.
fn flatten_reverts(reverts: &Reverts) -> Reverts {
    let mut per_account = HashMap::new();

    for (addr, acc_revert) in reverts.iter().flatten() {
        match per_account.entry(*addr) {
            Entry::Vacant(v) => {
                v.insert(acc_revert.clone());
            }
            Entry::Occupied(mut o) => {
                let entry = o.get_mut();

                // Always OR wipe_storage (if any transition wiped storage, the block-level revert
                // must reflect it).
                entry.wipe_storage |= acc_revert.wipe_storage;

                // Merge storage: keep earliest revert-to value per slot.
                for (slot, revert_to) in &acc_revert.storage {
                    entry.storage.entry(*slot).or_insert(*revert_to);
                }

                // Merge account-info revert: keep earliest non-DoNothing.
                if matches!(entry.account, AccountInfoRevert::DoNothing)
                    && !matches!(acc_revert.account, AccountInfoRevert::DoNothing)
                {
                    entry.account = acc_revert.account.clone();
                }

                // Keep earliest previous_status: do not overwrite.
            }
        }
    }

    // Transform the map into a vec
    let flattened = per_account.into_iter().collect();
    Reverts::new(vec![flattened])
}

/// The current state of all known pre confirmations received over the P2P layer
/// or generated from the payload building job of this node.
///
/// The state is flushed when FCU is received with a parent hash that matches the block hash
/// of the latest pre confirmation _or_ when an FCU is received that does not match the latest pre confirmation,
/// in which case the pre confirmations were not included as part of the canonical chain.
#[derive(Debug, Clone)]
pub struct FlashblocksStateExecutor {
    inner: Arc<RwLock<FlashblocksStateExecutorInner>>,
    p2p_handle: FlashblocksHandle,
    builder_config: OpBuilderConfig,
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
}

#[derive(Debug, Clone)]
pub struct FlashblocksStateExecutorInner {
    /// List of flashblocks for the current payload
    flashblocks: Flashblocks,
    /// The latest built payload with its associated flashblock index
    latest_payload: Option<(OpBuiltPayload, u64)>,
    payload_events: Option<broadcast::Sender<Events<OpEngineTypes>>>,
}

impl FlashblocksStateExecutor {
    /// Creates a new instance of [`FlashblocksStateExecutor`].
    ///
    /// This function spawn a task that handles updates. It should only be called once.
    pub fn new(
        p2p_handle: FlashblocksHandle,
        builder_config: OpBuilderConfig,
        pending_block: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
    ) -> Self {
        let inner = Arc::new(RwLock::new(FlashblocksStateExecutorInner {
            flashblocks: Default::default(),
            latest_payload: None,
            payload_events: None,
        }));

        Self {
            inner,
            p2p_handle,
            builder_config,
            pending_block,
        }
    }

    /// Launches the executor to listen for new flashblocks and build payloads.
    pub fn launch<Node, Pool, P>(
        &self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        payload_builder_ctx_builder: P,
        evm_config: OpEvmConfig,
    ) where
        Pool: TransactionPool + 'static,
        Node: FullNodeTypes,
        Node::Provider: StateProviderFactory + HeaderProvider<Header = alloy_consensus::Header>,
        Node::Types: NodeTypes<ChainSpec = OpChainSpec>,
        P: PayloadBuilderCtxBuilder<Node::Provider, OpEvmConfig, OpChainSpec> + 'static,
    {
        let mut stream = self.p2p_handle.flashblock_stream();
        let this = self.clone();
        let provider = ctx.provider().clone();
        let chain_spec = ctx.chain_spec().clone();

        let pending_block = self.pending_block.clone();

        ctx.task_executor()
            .spawn_critical("flashblocks executor", async move {
                while let Some(flashblock) = stream.next().await {
                    if let Err(e) = process_flashblock(
                        &provider,
                        &pool,
                        &payload_builder_ctx_builder,
                        &evm_config,
                        &this,
                        &chain_spec,
                        flashblock,
                        pending_block.clone(),
                    ) {
                        error!("error processing flashblock: {e:?}")
                    }
                }
            });
    }

    pub fn publish_built_payload(
        &self,
        authorized_payload: AuthorizedPayload<FlashblocksPayloadV1>,
        built_payload: OpBuiltPayload,
    ) -> eyre::Result<()> {
        let flashblock = authorized_payload.msg().clone();

        let FlashblocksStateExecutorInner {
            ref mut flashblocks,
            ref mut latest_payload,
            ..
        } = *self.inner.write();

        let index = flashblock.index;
        let flashblock = Flashblock { flashblock };
        flashblocks.push(flashblock.clone())?;

        *latest_payload = Some((built_payload, index));

        self.p2p_handle.publish_new(authorized_payload.clone())?;

        Ok(())
    }

    /// Returns a reference to the latest flashblock.
    pub fn last(&self) -> Flashblock {
        self.inner.read().flashblocks.last().clone()
    }

    /// Returns a reference to the latest flashblock.
    pub fn flashblocks(&self) -> Flashblocks {
        self.inner.read().flashblocks.clone()
    }

    /// Returns a receiver for the pending block.
    pub fn pending_block(
        &self,
    ) -> tokio::sync::watch::Receiver<Option<ExecutedBlock<OpPrimitives>>> {
        self.pending_block.subscribe()
    }

    /// Registers a new broadcast channel for built payloads.
    pub fn register_payload_events(&self, tx: broadcast::Sender<Events<OpEngineTypes>>) {
        self.inner.write().payload_events = Some(tx);
    }

    /// Broadcasts a new payload to cache in the in memory tree.
    pub fn broadcast_payload(
        &self,
        event: Events<OpEngineTypes>,
        payload_events: Option<broadcast::Sender<Events<OpEngineTypes>>>,
    ) -> eyre::Result<()> {
        if let Some(payload_events) = payload_events {
            if let Err(e) = payload_events.send(event) {
                error!("error broadcasting payload: {e:?}");
            }
        }
        Ok(())
    }
}

#[expect(clippy::too_many_arguments)]
fn process_flashblock<Provider, Pool, P>(
    provider: &Provider,
    pool: &Pool,
    payload_builder_ctx_builder: &P,
    evm_config: &OpEvmConfig,
    state_executor: &FlashblocksStateExecutor,
    chain_spec: &OpChainSpec,
    flashblock: FlashblocksPayloadV1,
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
) -> eyre::Result<()>
where
    Provider: StateProviderFactory + HeaderProvider<Header = alloy_consensus::Header> + Clone,

    Pool: TransactionPool + 'static,
    P: PayloadBuilderCtxBuilder<Provider, OpEvmConfig, OpChainSpec> + 'static,
{
    trace!(target: "flashblocks::state_executor",id = %flashblock.payload_id, index = %flashblock.index, "processing flashblock");

    let FlashblocksStateExecutorInner {
        ref mut flashblocks,
        ref mut latest_payload,
        ref mut payload_events,
    } = *state_executor.inner.write();

    let flashblock = Flashblock { flashblock };

    if let Some(latest_payload) = latest_payload {
        if latest_payload.0.id() == flashblock.flashblock.payload_id
            && latest_payload.1 >= flashblock.flashblock.index
        {
            // Already processed this flashblock. This happens when set directly
            // from publish_build_payload. Since we already built the payload, no need
            // to do it again.
            pending_block.send_replace(latest_payload.0.executed_block());
            return Ok(());
        }
    }

    // If for whatever reason we are not processing flashblocks in order
    // we will error and return here.
    let base = if flashblocks.is_new_payload(&flashblock)? {
        *latest_payload = None;
        // safe unwrap from check in is_new_payload
        flashblock.base().unwrap()
    } else {
        flashblocks.base()
    };

    let diff = &flashblock.flashblock.diff;
    let index = flashblock.flashblock.index;
    let cancel = CancelOnDrop::default();

    let transactions = diff
        .transactions
        .iter()
        .map(|b| {
            let tx: OpTxEnvelope = Decodable2718::decode_2718_exact(b)?;
            eyre::Result::Ok(WithEncoded::new(b.clone(), tx))
        })
        .collect::<eyre::Result<Vec<_>>>()?;

    let eth_attrs = EthPayloadBuilderAttributes {
        id: PayloadId(flashblock.payload_id().to_owned()),
        parent: base.parent_hash,
        timestamp: base.timestamp,
        suggested_fee_recipient: base.fee_recipient,
        prev_randao: base.prev_randao,
        withdrawals: Withdrawals(diff.withdrawals.clone()),
        parent_beacon_block_root: Some(base.parent_beacon_block_root),
    };

    let eip1559 = encode_holocene_extra_data(
        Default::default(),
        chain_spec.base_fee_params_at_timestamp(base.timestamp),
    )?;

    let attributes = OpPayloadBuilderAttributes {
        payload_attributes: eth_attrs,
        no_tx_pool: true,
        transactions: transactions.clone(),
        gas_limit: None,
        eip_1559_params: Some(eip1559[1..=8].try_into()?),
        min_base_fee: None,
    };

    let sealed_header = provider
        .sealed_header_by_hash(base.parent_hash)?
        .ok_or_eyre(format!("missing sealed header: {}", base.parent_hash))?;

    let state_provider = provider.state_by_block_hash(base.parent_hash)?;

    let config = PayloadConfig::new(Arc::new(sealed_header), attributes);
    let builder_ctx = payload_builder_ctx_builder.build(
        provider.clone(),
        evm_config.clone(),
        state_executor.builder_config.clone(),
        config,
        &cancel,
        latest_payload.as_ref().map(|p| p.0.clone()),
    );

    let best = |_| BestPayloadTransactions::new(vec![].into_iter());
    let db = StateProviderDatabase::new(&state_provider);

    let outcome = FlashblockBuilder::new(best).build(
        pool.clone(),
        db,
        &state_provider,
        &builder_ctx,
        latest_payload.as_ref().map(|p| p.0.clone()),
    )?;

    let payload = match outcome {
        BuildOutcomeKind::Better { payload } => payload,
        BuildOutcomeKind::Freeze(payload) => payload,
        _ => return Ok(()),
    };

    debug_assert_eq!(
        payload.block().hash(),
        flashblock.diff().block_hash,
        "executed block hash does not match flashblock diff block hash"
    );

    trace!(target: "flashblocks::state_executor", hash = %payload.block().hash(), "setting latest payload");
    flashblocks.push(flashblock)?;
    *latest_payload = Some((payload.clone(), index));
    pending_block.send_replace(payload.executed_block());

    state_executor.broadcast_payload(
        Events::BuiltPayload(payload.clone()),
        payload_events.clone(),
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::executor::flatten_reverts;
    use alloy_primitives::{Address, U256};
    use revm::{
        database::{
            states::reverts::{AccountInfoRevert, Reverts},
            AccountRevert, AccountStatus, RevertToSlot,
        },
        primitives::HashMap,
        state::AccountInfo,
    };

    #[test]
    fn test_flatten_reverts_different_storage_slots() {
        let mut first_reverts = vec![];
        let addr = Address::with_last_byte(1);
        // create first revert
        let account = AccountInfoRevert::DoNothing;
        let mut storage = HashMap::default();
        let revert_to_slot = RevertToSlot::Some(U256::from(1));
        let storage_slot = U256::ZERO;
        storage.insert(storage_slot, revert_to_slot);
        let previous_status = AccountStatus::Loaded;
        let wipe_storage = false;
        let acc_revert = AccountRevert {
            account,
            storage,
            previous_status,
            wipe_storage,
        };
        first_reverts.push((addr, acc_revert));
        // create second revert
        let mut second_reverts = vec![];
        // same account info revert
        let account = AccountInfoRevert::DoNothing;
        let mut storage = HashMap::default();
        // change the slot value: this should be ignored in final revert because it
        // works on a storage slot that is already present in reverts
        let revert_to_slot = RevertToSlot::Some(U256::from(2));
        let storage_slot = U256::ZERO;
        storage.insert(storage_slot, revert_to_slot);
        // now add a new storage slot: this should be considered and included in final
        // reverts because it's a completely new storage slot
        let revert_to_slot = RevertToSlot::Some(U256::from(1));
        let storage_slot = U256::from(1);
        storage.insert(storage_slot, revert_to_slot);
        // new previous status: this should be ignored
        let previous_status = AccountStatus::InMemoryChange;
        // keep wipe storage = false
        let wipe_storage = false;
        let acc_revert = AccountRevert {
            account,
            storage,
            previous_status,
            wipe_storage,
        };
        second_reverts.push((addr, acc_revert));
        // create final Reverts
        let reverts = Reverts::new(vec![first_reverts, second_reverts]);
        assert_eq!(reverts.len(), 2);
        // flatten reverts using the helper fn
        let flatten_reverts = flatten_reverts(&reverts);
        assert_eq!(flatten_reverts.len(), 1);
        let actual_reverts = flatten_reverts.first().unwrap();
        assert_eq!(actual_reverts.len(), 1);
        let (actual_addr, actual_account_revert) = actual_reverts.first().unwrap().clone();
        assert_eq!(actual_addr, addr);
        let account = AccountInfoRevert::DoNothing;
        let mut storage = HashMap::default();
        let revert_to_slot = RevertToSlot::Some(U256::from(1));
        let storage_slot = U256::ZERO;
        storage.insert(storage_slot, revert_to_slot);
        let revert_to_slot = RevertToSlot::Some(U256::from(1));
        let storage_slot = U256::from(1);
        storage.insert(storage_slot, revert_to_slot);
        let previous_status = AccountStatus::Loaded;
        let wipe_storage = false;
        let expected_acc_revert = AccountRevert {
            account,
            storage,
            previous_status,
            wipe_storage,
        };
        assert_eq!(expected_acc_revert, actual_account_revert);
    }

    #[test]
    fn test_flatten_reverts_different_account_info() {
        let mut first_reverts = vec![];
        let addr = Address::with_last_byte(1);
        // create first revert
        let account = AccountInfoRevert::DoNothing;
        let storage = HashMap::default();
        let previous_status = AccountStatus::Loaded;
        let wipe_storage = false;
        let acc_revert = AccountRevert {
            account,
            storage,
            previous_status,
            wipe_storage,
        };
        first_reverts.push((addr, acc_revert));
        // create second revert
        let mut second_reverts = vec![];
        // change account info
        let prev_acc_info = AccountInfo::default();
        let account = AccountInfoRevert::RevertTo(prev_acc_info.clone());
        let storage = HashMap::default();
        let previous_status = AccountStatus::Loaded;
        // keep wipe storage = false
        let wipe_storage = false;
        let acc_revert = AccountRevert {
            account,
            storage,
            previous_status,
            wipe_storage,
        };
        second_reverts.push((addr, acc_revert));
        // create third revert
        let mut third_reverts = vec![];
        // change account info: this should be ignored because it's already in
        // a non-DoNothing state before this revert
        let account = AccountInfoRevert::DeleteIt;
        let storage = HashMap::default();
        let previous_status = AccountStatus::Loaded;
        // keep wipe storage = false
        let wipe_storage = false;
        let acc_revert = AccountRevert {
            account,
            storage,
            previous_status,
            wipe_storage,
        };
        third_reverts.push((addr, acc_revert));
        // create final Reverts
        let reverts = Reverts::new(vec![first_reverts, second_reverts, third_reverts]);
        assert_eq!(reverts.len(), 3);
        // flatten reverts using the helper fn
        let flatten_reverts = flatten_reverts(&reverts);
        assert_eq!(flatten_reverts.len(), 1);
        let actual_reverts = flatten_reverts.first().unwrap();
        assert_eq!(actual_reverts.len(), 1);
        let (actual_addr, actual_account_revert) = actual_reverts.first().unwrap().clone();
        assert_eq!(actual_addr, addr);
        let account = AccountInfoRevert::RevertTo(prev_acc_info);
        let storage = HashMap::default();
        let previous_status = AccountStatus::Loaded;
        let wipe_storage = false;
        let expected_acc_revert = AccountRevert {
            account,
            storage,
            previous_status,
            wipe_storage,
        };
        assert_eq!(expected_acc_revert, actual_account_revert);
    }

    #[test]
    fn test_flatten_reverts_wipe_storage() {
        let mut first_reverts = vec![];
        let addr = Address::with_last_byte(1);
        // create first revert
        let account = AccountInfoRevert::DoNothing;
        let storage = HashMap::default();
        let previous_status = AccountStatus::Loaded;
        let wipe_storage = false;
        let acc_revert = AccountRevert {
            account,
            storage,
            previous_status,
            wipe_storage,
        };
        first_reverts.push((addr, acc_revert));
        // create second revert
        let mut second_reverts = vec![];
        let prev_acc_info = AccountInfo::default();
        let account = AccountInfoRevert::RevertTo(prev_acc_info.clone());
        let storage = HashMap::default();
        let previous_status = AccountStatus::Loaded;
        // change wipe storage equal to true
        let wipe_storage = true;
        let acc_revert = AccountRevert {
            account,
            storage,
            previous_status,
            wipe_storage,
        };
        second_reverts.push((addr, acc_revert));
        // create third revert
        let mut third_reverts = vec![];
        let account = AccountInfoRevert::DeleteIt;
        let storage = HashMap::default();
        let previous_status = AccountStatus::Loaded;
        // change again wipe storage = false: this should be ignored because it's
        // already true before this revert
        let wipe_storage = false;
        let acc_revert = AccountRevert {
            account,
            storage,
            previous_status,
            wipe_storage,
        };
        third_reverts.push((addr, acc_revert));
        // create final Reverts
        let reverts = Reverts::new(vec![first_reverts, second_reverts, third_reverts]);
        assert_eq!(reverts.len(), 3);
        // flatten reverts using the helper fn
        let flatten_reverts = flatten_reverts(&reverts);
        assert_eq!(flatten_reverts.len(), 1);
        let actual_reverts = flatten_reverts.first().unwrap();
        assert_eq!(actual_reverts.len(), 1);
        let (actual_addr, actual_account_revert) = actual_reverts.first().unwrap().clone();
        assert_eq!(actual_addr, addr);
        let account = AccountInfoRevert::RevertTo(prev_acc_info);
        let storage = HashMap::default();
        let previous_status = AccountStatus::Loaded;
        let wipe_storage = true;
        let expected_acc_revert = AccountRevert {
            account,
            storage,
            previous_status,
            wipe_storage,
        };
        assert_eq!(expected_acc_revert, actual_account_revert);
    }
}
