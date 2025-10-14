use alloy_consensus::{Block, Transaction, TxReceipt};
use alloy_eips::eip2718::WithEncoded;
use alloy_eips::eip4895::Withdrawals;
use alloy_eips::{Decodable2718, Encodable2718};
use alloy_op_evm::block::receipt_builder::OpReceiptBuilder;
use alloy_op_evm::{OpBlockExecutionCtx, OpBlockExecutor, OpBlockExecutorFactory, OpEvmFactory};
use alloy_primitives::{keccak256, FixedBytes};
use alloy_rpc_types_engine::PayloadId;
use eyre::eyre::{eyre, OptionExt as _};
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use flashblocks_primitives::p2p::AuthorizedPayload;
use flashblocks_primitives::primitives::FlashblocksPayloadV1;
use futures::StreamExt as _;
use op_alloy_consensus::{encode_holocene_extra_data, OpTxEnvelope};
use parking_lot::RwLock;
use reth::core::primitives::Receipt;
use reth::payload::EthPayloadBuilderAttributes;
use reth::revm::cancelled::CancelOnDrop;
use reth::revm::database::StateProviderDatabase;
use reth::revm::State;
use reth_basic_payload_builder::{BuildOutcomeKind, PayloadConfig};
use reth_chain_state::ExecutedBlockWithTrieUpdates;
use reth_evm::block::{BlockExecutorFactory, BlockExecutorFor};
use reth_evm::execute::{
    BasicBlockBuilder, BlockAssembler, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome,
    ExecutorTx,
};
use reth_evm::op_revm::{OpHaltReason, OpSpecId};
use reth_evm::{
    block::{BlockExecutionError, BlockExecutor, CommitChanges, ExecutableTx},
    Database, FromRecoveredTx, FromTxWithEncoded, OnStateHook,
};
use reth_evm::{ConfigureEvm, Evm, EvmFactory};
use reth_node_api::{BuiltPayload as _, FullNodeTypes, NodeTypes, PayloadBuilderError};
use reth_node_builder::BuilderContext;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpNextBlockEnvAttributes;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{
    OpBlockAssembler, OpBuiltPayload, OpDAConfig, OpEvmConfig, OpPayloadBuilderAttributes,
    OpRethReceiptBuilder,
};
use reth_optimism_primitives::{DepositReceipt, OpPrimitives, OpReceipt, OpTransactionSigned};
use reth_payload_util::BestPayloadTransactions;
use reth_primitives::{transaction::SignedTransaction, SealedHeader};
use reth_primitives::{NodePrimitives, Recovered, RecoveredBlock};
use reth_provider::{BlockExecutionResult, HeaderProvider, StateProvider, StateProviderFactory};
use reth_transaction_pool::TransactionPool;
use revm::context::result::{ExecutionResult, ResultAndState};
use revm::database::states::bundle_state::BundleRetention;
use revm::database::states::reverts::Reverts;
use revm::database::BundleState;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{error, trace};

use crate::access_list::FlashblockAccessListConstruction;
use crate::{FlashblockBuilder, PayloadBuilderCtxBuilder};
use flashblocks_primitives::flashblocks::{Flashblock, Flashblocks};

/// A Block Executor for Optimism that can load pre state from previous flashblocks.
pub struct FlashblocksBlockExecutor<Evm, R, Spec>
where
    R: OpReceiptBuilder,
{
    inner: OpBlockExecutor<Evm, R, Spec>,
    flashblock_access_list: FlashblockAccessListConstruction,
}

impl<'db, DB, E, R, Spec> FlashblocksBlockExecutor<E, R, Spec>
where
    DB: Database + 'db,
    E: Evm<
        DB = &'db mut State<DB>,
        Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
    >,
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt>,
    Spec: OpHardforks + Clone,
{
    /// Creates a new [`OpBlockExecutor`].
    pub fn new(evm: E, ctx: OpBlockExecutionCtx, spec: Spec, receipt_builder: R) -> Self {
        let executor = OpBlockExecutor::new(evm, ctx, spec, receipt_builder);

        Self {
            inner: executor,
            flashblock_access_list: FlashblockAccessListConstruction {
                changes: HashMap::default(),
            },
        }
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

    /// Takes ownership of the state hook and returns a [`FlashblockAccessListConstruction`].
    pub fn access_list(&self) -> &FlashblockAccessListConstruction {
        &self.flashblock_access_list
    }

    /// Records the transitions from the EVM's database into the access list construction.
    fn record_transitions(&mut self) {
        let transitions = self.evm().db().transition_state.as_ref().cloned();
        let index = self.inner.receipts.len();

        self.flashblock_access_list
            .on_state_transition(transitions, index);
    }

    pub fn finish_with_access_list(
        self,
    ) -> Result<
        (
            E,
            BlockExecutionResult<R::Receipt>,
            FlashblockAccessListConstruction,
        ),
        BlockExecutionError,
    > {
        let (evm, result) = self.inner.finish()?;
        let access_list = self.flashblock_access_list.clone();
        Ok((evm, result, access_list))
    }
}

impl<'db, DB, E, R, Spec> BlockExecutor for FlashblocksBlockExecutor<E, R, Spec>
where
    DB: Database + 'db,
    E: Evm<
        DB = &'db mut State<DB>,
        Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
    >,
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt>,
    Spec: OpHardforks + Clone,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Evm = E;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        let res = self.inner.apply_pre_execution_changes();
        self.record_transitions();
        res
    }

    fn execute_transaction(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<u64, BlockExecutionError> {
        self.execute_transaction_without_commit(&tx)
            .and_then(|output| {
                println!("Output: {:#?}", output.result);
                self.commit_transaction(output, &tx)
            })
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
        let res = self.inner.commit_transaction(output, tx);
        self.record_transitions();
        res
    }

    fn finish(self) -> Result<(Self::Evm, BlockExecutionResult<R::Receipt>), BlockExecutionError> {
        let mut access_list = self.flashblock_access_list.clone();
        let index = self.inner.receipts.len();
        let res = self.inner.finish();
        if let Ok((evm, _)) = &res {
            access_list.on_state_transition(evm.db().transition_state.as_ref().cloned(), index);
        }

        res
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        self.inner.evm_mut()
    }

    fn evm(&self) -> &Self::Evm {
        self.inner.evm()
    }

    fn set_state_hook(&mut self, hook: Option<Box<dyn OnStateHook>>) {
        self.inner.set_state_hook(hook)
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
        Tx: FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
        Spec = OpSpecId,
        HaltReason = OpHaltReason,
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

        // flatten reverts into a single reverts as the bundle is re-used across multiple payloads
        // which represent a single atomic state transition. therefore reverts should have length 1
        // we only retain the first occurance of a revert for any given account.
        let flattened = db
            .bundle_state
            .reverts
            .iter()
            .flatten()
            .scan(HashSet::new(), |visited, (acc, revert)| {
                if visited.insert(acc) {
                    Some((*acc, revert.clone()))
                } else {
                    None
                }
            })
            .collect();

        db.bundle_state.reverts = Reverts::new(vec![flattened]);

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
    da_config: OpDAConfig,
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlockWithTrieUpdates<OpPrimitives>>>,
}

impl Default for FlashblocksStateExecutor {
    fn default() -> Self {
        unimplemented!("FlashblocksStateExecutor::new must be used instead")
    }
}

#[derive(Debug, Clone)]
pub struct FlashblocksStateExecutorInner {
    flashblocks: Option<Flashblocks>,
    latest_payload: Option<(OpBuiltPayload, u64)>,
}

impl FlashblocksStateExecutor {
    /// Creates a new instance of [`FlashblocksStateExecutor`].
    ///
    /// This function spawn a task that handles updates. It should only be called once.
    pub fn new(
        p2p_handle: FlashblocksHandle,
        da_config: OpDAConfig,
        pending_block: tokio::sync::watch::Sender<
            Option<ExecutedBlockWithTrieUpdates<OpPrimitives>>,
        >,
    ) -> Self {
        let inner = Arc::new(RwLock::new(FlashblocksStateExecutorInner {
            flashblocks: None,
            latest_payload: None,
        }));

        Self {
            inner,
            p2p_handle,
            da_config,
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
        let (_flashblocks, _new_payload) = match flashblocks {
            Some(ref mut f) => {
                let new_payload = f.push(flashblock.clone())?;
                (f, new_payload)
            }
            None => {
                *flashblocks = Some(Flashblocks::new(vec![flashblock.clone()])?);
                (flashblocks.as_mut().unwrap(), true)
            }
        };

        *latest_payload = Some((built_payload, index));

        self.p2p_handle.publish_new(authorized_payload.clone())?;

        Ok(())
    }

    /// Returns a reference to the latest flashblock.
    pub fn last(&self) -> Option<Flashblock> {
        self.inner.read().flashblocks.as_ref().map(|f| Flashblock {
            flashblock: f.last().clone(),
        })
    }

    /// Returns a reference to the latest flashblock.
    pub fn flashblocks(&self) -> Option<Flashblocks> {
        self.inner.read().flashblocks.clone()
    }

    /// Returns a receiver for the pending block.
    pub fn pending_block(
        &self,
    ) -> tokio::sync::watch::Receiver<Option<ExecutedBlockWithTrieUpdates<OpPrimitives>>> {
        self.pending_block.subscribe()
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
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlockWithTrieUpdates<OpPrimitives>>>,
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
        ..
    } = *state_executor.inner.write();

    let flashblock = Flashblock { flashblock };
    let (flashblocks, _new_payload) = match flashblocks {
        Some(ref mut f) => {
            if let Some(latest_payload) = latest_payload {
                if latest_payload.0.id() == flashblock.flashblock.payload_id
                    && latest_payload.1 >= flashblock.flashblock.index
                {
                    // Already processed this flashblock
                    pending_block.send_replace(latest_payload.0.executed_block());
                    return Ok(());
                }
            }

            let new_payload = f.push(flashblock.clone())?;
            (f, new_payload)
        }
        None => {
            *flashblocks = Some(Flashblocks::new(vec![flashblock.clone()]).unwrap());
            (flashblocks.as_mut().unwrap(), true)
        }
    };

    let flashblock = flashblocks.last();
    let base = flashblocks.base();

    let transactions = flashblock
        .diff
        .transactions
        .iter()
        .map(|b| {
            let tx: OpTxEnvelope = Decodable2718::decode_2718_exact(b)?;
            Ok(tx.try_into_recovered().unwrap())
        })
        .collect::<eyre::Result<Vec<_>>>()?;

    let sealed_header = provider
        .sealed_header_by_hash(base.parent_hash)?
        .ok_or_eyre(format!("missing sealed header: {}", base.parent_hash))?;

    let attributes = OpNextBlockEnvAttributes {
        timestamp: base.timestamp,
        suggested_fee_recipient: base.fee_recipient,
        prev_randao: base.prev_randao,
        gas_limit: base.gas_limit,
        parent_beacon_block_root: Some(base.parent_beacon_block_root),
        extra_data: base.extra_data.clone(),
    };

    // Prepare EVM environment.
    let evm_env = evm_config
        .next_evm_env(sealed_header.header(), &attributes)
        .map_err(PayloadBuilderError::other)?;

    let state_provider = provider.state_by_block_hash(sealed_header.hash())?;
    let mut state = StateProviderDatabase::new(&state_provider);
    let mut db = if let Some((payload, _)) = latest_payload {
        State::builder()
            .with_database(&mut state)
            .with_bundle_prestate(
                payload
                    .executed_block()
                    .as_ref()
                    .unwrap()
                    .block
                    .execution_output
                    .bundle
                    .clone(),
            )
            .with_bundle_update()
            .build()
    } else {
        State::builder()
            .with_database(&mut state)
            .with_bundle_update()
            .build()
    };

    // Prepare EVM.
    let evm = evm_config.evm_with_env(&mut db, evm_env);
    let execution_context = OpBlockExecutionCtx {
        parent_hash: base.parent_hash,
        parent_beacon_block_root: Some(base.parent_beacon_block_root),
        extra_data: base.extra_data.clone(),
    };

    let mut executor = FlashblocksBlockExecutor::new(
        evm,
        execution_context,
        chain_spec,
        OpRethReceiptBuilder::default(),
    );

    // If we don't have a payload yet we need to apply the pre-execution changes
    if latest_payload.is_none() {
        executor.apply_pre_execution_changes()?;
    }

    // Execute each transaction individually
    for tx in transactions {
        executor.execute_transaction(tx)?;
    }

    // TODO: When to apply post execution changes

    // Validate the BAL matches the provided Flashblock BAL
    let (_, _, access_list) = executor.finish_with_access_list()?;

    // Validation
    // 1.) BAL Hash Matches
    let expected_bal_hash = keccak256(alloy_rlp::encode(access_list.build()));

    if expected_bal_hash != flashblock.diff.access_list_hash {
        return Err(eyre!(format!(
            "Access List Hash does not match computed hash - expected {:#?} got {:#?}",
            expected_bal_hash, flashblock.diff.access_list_hash
        )));
    }

    // trace!(target: "flashblocks::state_executor", hash = %payload.block().hash(), "setting latest payload");

    // *latest_payload = Some((payload.clone(), flashblock.index));

    // pending_block.send_replace(payload.executed_block());

    Ok(())
}
