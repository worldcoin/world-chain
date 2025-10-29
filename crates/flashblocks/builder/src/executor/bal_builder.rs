use alloy_consensus::{Block, Header, Transaction, TxReceipt};
use alloy_eips::{Decodable2718, Encodable2718};
use alloy_op_evm::block::receipt_builder::OpReceiptBuilder;
use alloy_op_evm::{OpBlockExecutionCtx, OpBlockExecutor};
use alloy_primitives::{keccak256, Address, FixedBytes, U256};
use dashmap::DashMap;
use eyre::eyre::{eyre, OptionExt as _};
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use flashblocks_primitives::access_list::FlashblockAccessList;
use flashblocks_primitives::p2p::AuthorizedPayload;
use flashblocks_primitives::primitives::FlashblocksPayloadV1;
use futures::StreamExt as _;
use op_alloy_consensus::OpTxEnvelope;
use parking_lot::RwLock;
use reth::core::primitives::Receipt;
use reth::revm::database::StateProviderDatabase;
use reth::revm::State;
use reth_chain_state::{ExecutedBlockWithTrieUpdates, ExecutedTrieUpdates};
use reth_evm::block::BlockExecutorFactory;
use reth_evm::execute::{
    BasicBlockBuilder, BlockAssembler, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome,
    ExecutorTx,
};
use reth_evm::op_revm::{OpHaltReason, OpSpecId};
use reth_evm::{
    block::{BlockExecutionError, BlockExecutor, CommitChanges, ExecutableTx},
    Database, FromRecoveredTx, FromTxWithEncoded, OnStateHook,
};
use reth_evm::{ConfigureEvm, Evm, EvmEnv};
use reth_node_api::{BuiltPayload as _, Events, FullNodeTypes, NodeTypes, PayloadBuilderError};
use reth_node_builder::BuilderContext;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpNextBlockEnvAttributes;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{
    OpBlockAssembler, OpBuiltPayload, OpEngineTypes, OpEvmConfig, OpRethReceiptBuilder,
};
use reth_optimism_primitives::{DepositReceipt, OpPrimitives, OpReceipt, OpTransactionSigned};
use reth_primitives::{transaction::SignedTransaction, SealedHeader};
use reth_primitives::{NodePrimitives, Recovered, RecoveredBlock};
use reth_provider::{
    BlockExecutionResult, ExecutionOutcome, HeaderProvider, StateProvider, StateProviderFactory,
};
use reth_trie_common::updates::TrieUpdates;
use reth_trie_common::{HashedPostState, KeccakKeyHasher};
use revm::context::result::{ExecutionResult, ResultAndState};
use revm::database::states::bundle_state::BundleRetention;
use revm::database::states::reverts::Reverts;
use revm::database::{BundleAccount, BundleState};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::time::Instant;
use tracing::{error, info, trace};

use crate::access_list::FlashblockAccessListConstruction;
use crate::executor::factory::FlashblocksBlockExecutorFactory;
use flashblocks_primitives::flashblocks::{Flashblock, Flashblocks};

/// A Block Executor for Optimism that can load pre state from previous flashblocks
///
/// A Block Access List is constucted during execution
pub struct BalBuilderBlockExecutor<Evm, R, Spec>
where
    R: OpReceiptBuilder,
{
    inner: OpBlockExecutor<Evm, R, Spec>,
    flashblock_access_list: FlashblockAccessListConstruction,
    min_tx_index: u64,
    max_tx_index: u64,
}

impl<'db, DB, E, R, Spec> BalBuilderBlockExecutor<E, R, Spec>
where
    DB: Database + 'db,
    E: Evm<
        DB = &'db mut State<DB>,
        Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
    >,
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt>,
    Spec: OpHardforks + Clone,
{
    /// Creates a new [`FlashblocksBlockExecutor`].
    pub fn new(
        evm: E,
        ctx: OpBlockExecutionCtx,
        spec: Spec,
        receipt_builder: R,
        min_tx_index: u64,
    ) -> Self {
        let executor = OpBlockExecutor::new(evm, ctx, spec, receipt_builder);

        Self {
            inner: executor,
            flashblock_access_list: FlashblockAccessListConstruction {
                changes: DashMap::new(),
            },
            max_tx_index: min_tx_index + 1,
            min_tx_index,
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
        self.inner.gas_used += gas_used;
        self
    }

    /// Takes ownership of the state hook and returns a [`FlashblockAccessListConstruction`].
    pub fn access_list(&self) -> &FlashblockAccessListConstruction {
        &self.flashblock_access_list
    }

    /// Records the transitions from the EVM's database into the access list construction.
    fn record_transitions(&mut self) {
        let transitions = self.evm().db().transition_state.as_ref();
        self.flashblock_access_list
            .with_transition_state(transitions, self.inner.receipts.len());
        info!(target: "test_target", "recorded transitions for tx index range: {} - {}", self.min_tx_index, self.max_tx_index);
    }

    #[expect(clippy::type_complexity)]
    pub fn finish_with_access_list(
        self,
    ) -> Result<
        (
            E,
            BlockExecutionResult<R::Receipt>,
            FlashblockAccessListConstruction,
            u64,
            u64,
        ),
        BlockExecutionError,
    > {
        let (min_tx_index, max_tx_index) = (self.min_tx_index, self.max_tx_index);
        let access_list = self.flashblock_access_list.clone();
        let (evm, result) = self.inner.finish()?;

        Ok((evm, result, access_list, min_tx_index, max_tx_index))
    }
}

impl<'db, DB, E, R, Spec> BlockExecutor for BalBuilderBlockExecutor<E, R, Spec>
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

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        f: impl FnOnce(&ExecutionResult<<Self::Evm as Evm>::HaltReason>) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        let res = self.inner.execute_transaction_with_commit_condition(tx, f);
        self.record_transitions();
        self.max_tx_index += 1;
        res
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
        let access_list = self.flashblock_access_list.clone();
        let index = self.inner.receipts.len();
        let res = self.inner.finish();
        if let Ok((evm, _)) = &res {
            access_list.with_transition_state(evm.db().transition_state.as_ref(), index);
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

/// Assembles the full block from the bundle state and Execution Result
#[derive(Clone, Debug)]
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
        self.inner.assemble_block(input)
    }
}

/// A wrapper around the [`BasicBlockBuilder`] for flashblocks.
pub struct FlashblocksBlockBuilder<'a, N: NodePrimitives, Evm> {
    pub inner: BasicBlockBuilder<
        'a,
        FlashblocksBlockExecutorFactory,
        BalBuilderBlockExecutor<Evm, OpRethReceiptBuilder, OpChainSpec>,
        OpBlockAssembler<OpChainSpec>,
        N,
    >,
}

impl<'a, N: NodePrimitives, Evm> FlashblocksBlockBuilder<'a, N, Evm> {
    /// Creates a new [`FlashblocksBlockBuilder`] with the given executor factory and assembler.
    pub fn new(
        ctx: OpBlockExecutionCtx,
        parent: &'a SealedHeader<N::BlockHeader>,
        executor: BalBuilderBlockExecutor<Evm, OpRethReceiptBuilder, OpChainSpec>,
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
    type Executor = BalBuilderBlockExecutor<E, OpRethReceiptBuilder, OpChainSpec>;

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

impl<'a, DB, N, E> FlashblocksBlockBuilder<'a, N, E>
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
    // TODO: unify duplicate code
    pub fn finish_with_access_list(
        self,
        state: impl StateProvider,
    ) -> Result<(BlockBuilderOutcome<N>, FlashblockAccessList), BlockExecutionError> {
        let (evm, result, access_list, min_tx_index, max_tx_index) =
            self.inner.executor.finish_with_access_list()?;

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

        // Write the expected bundle state to a JSON
        // let json = serde_json::to_string_pretty(&db.bundle_state).map_err(BlockExecutionError::other)?;
        // std::fs::write("expected_bundle_state.json", json).map_err(BlockExecutionError::other)?;

        // calculate the state root``
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

        let access_list_before = access_list.clone();
        // TODO: Remove debug traces
        trace!(target: "test_target", "recorded transitions for tx index range: {} - {}, transactions length {:#?}", min_tx_index, max_tx_index, block.body().transactions().count());
        trace!(target: "test_target", "finished execution with access list length {:#?}", access_list_before.changes.len());

        let access_list_after = access_list.build(min_tx_index, max_tx_index);
        // let access_list_bundle: HashMap<Address, BundleAccount> =
        //     access_list_after.clone().into();

        // // Write the access list to a JSON
        // let json = serde_json::to_string_pretty(&access_list_bundle)
        //     .map_err(BlockExecutionError::other)?;
        // std::fs::write("flashblock_access_list_bundle.json", json).map_err(BlockExecutionError::other)?;

        trace!(target: "test_target", "built final access list length {:#?}", access_list_after.changes.len());
        Ok((
            BlockBuilderOutcome {
                execution_result: result,
                hashed_state,
                trie_updates,
                block,
            },
            access_list_after,
        ))
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
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlockWithTrieUpdates<OpPrimitives>>>,
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
        pending_block: tokio::sync::watch::Sender<
            Option<ExecutedBlockWithTrieUpdates<OpPrimitives>>,
        >,
    ) -> Self {
        let inner = Arc::new(RwLock::new(FlashblocksStateExecutorInner {
            flashblocks: Default::default(),
            latest_payload: None,
            payload_events: None,
        }));

        Self {
            inner,
            p2p_handle,
            pending_block,
        }
    }

    /// Launches the executor to listen for new flashblocks and build payloads.
    pub fn launch<Node>(&self, ctx: &BuilderContext<Node>, evm_config: OpEvmConfig)
    where
        Node: FullNodeTypes,
        Node::Provider: StateProviderFactory + HeaderProvider<Header = alloy_consensus::Header>,
        Node::Types: NodeTypes<ChainSpec = OpChainSpec>,
    {
        let mut stream = self.p2p_handle.flashblock_stream();
        let this = self.clone();
        let provider = ctx.provider().clone();
        let chain_spec = ctx.chain_spec().clone();

        let pending_block = self.pending_block.clone();

        ctx.task_executor()
            .spawn_critical("flashblocks executor", async move {
                while let Some(flashblock) = stream.next().await {
                    let provider = provider.clone();
                    if let Err(e) = process_flashblock(
                        provider,
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
    ) -> tokio::sync::watch::Receiver<Option<ExecutedBlockWithTrieUpdates<OpPrimitives>>> {
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

fn process_flashblock<Provider>(
    provider: Provider,
    evm_config: &OpEvmConfig,
    state_executor: &FlashblocksStateExecutor,
    chain_spec: &OpChainSpec,
    flashblock: FlashblocksPayloadV1,
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlockWithTrieUpdates<OpPrimitives>>>,
) -> eyre::Result<()>
where
    Provider:
        StateProviderFactory + HeaderProvider<Header = alloy_consensus::Header> + Clone + 'static,
{
    trace!(
        target: "flashblocks::state_executor",
        id = %flashblock.payload_id,
        diff = %flashblock.diff,
        index = %flashblock.index,
        "processing flashblock"
    );

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

    let index = flashblock.flashblock().index;

    let transactions = flashblock
        .diff()
        .transactions
        .iter()
        .map(|b| {
            let tx: OpTxEnvelope = Decodable2718::decode_2718_exact(b)?;
            Ok(tx.try_into_recovered().unwrap())
        })
        .collect::<eyre::Result<Vec<_>>>()?;

    let senders = transactions
        .clone()
        .into_iter()
        .map(|tx| tx.into_parts().1)
        .collect::<Vec<_>>();

    let sealed_header = provider
        .sealed_header_by_hash(base.parent_hash)?
        .ok_or_eyre(format!("missing sealed header: {}", base.parent_hash))?;

    // We can now create the block executor, and transform the BAL into the expected hashed post state.
    //
    // Transaction Execution, and State Transition can be performed in parellel.
    // We need to ensure that transaction execution produces the same BAL as provided in the flashblock.

    let provider_clone = provider.clone();
    let evm_config = evm_config.clone();
    let base = base.clone();

    let latest_bundle = latest_payload.as_ref().map(|(p, _)| {
        p.executed_block()
            .unwrap() // Safe unwrap
            .block
            .execution_output
            .bundle
            .clone()
    });

    let state_provider = Arc::new(provider_clone.state_by_block_hash(sealed_header.hash())?);

    let sealed_header = Arc::new(
        provider_clone
            .sealed_header_by_hash(base.clone().parent_hash)?
            .ok_or_eyre(format!(
                "missing sealed header: {}",
                base.clone().parent_hash
            ))?,
    );

    let attributes = OpNextBlockEnvAttributes {
        timestamp: base.timestamp,
        suggested_fee_recipient: base.fee_recipient,
        prev_randao: base.prev_randao,
        gas_limit: base.gas_limit,
        parent_beacon_block_root: Some(base.parent_beacon_block_root),
        extra_data: base.extra_data.clone(),
    };

    // Prepare EVM.
    let execution_context = OpBlockExecutionCtx {
        parent_hash: base.parent_hash,
        parent_beacon_block_root: Some(base.parent_beacon_block_root),
        extra_data: base.extra_data.clone(),
    };

    let execution_context_clone = execution_context.clone();

    let state_provider_clone = state_provider.clone();
    let state_provider_clone2 = state_provider.clone();

    let sealed_header_clone = sealed_header.clone();
    let transactions_clone = transactions.clone();

    let before_execution = Instant::now();

    let (execution_result, state_root_result) = if let Some(access_list_data) =
        flashblock.diff().access_list_data.as_ref()
    {
        info!(target: "flashblocks::state_executor", "executing flashblock with access list");
        let (execution_result, state_root_result) = rayon::join(
            move || {
                execute_transactions(
                    transactions_clone.clone(),
                    Some(access_list_data.access_list_hash),
                    &evm_config,
                    sealed_header.clone(),
                    state_provider_clone.clone(),
                    &attributes,
                    latest_bundle,
                    execution_context_clone.clone(),
                    chain_spec,
                )
            },
            move || {
                let optimistic_bundle: HashMap<Address, BundleAccount> =
                    access_list_data.access_list.clone().into();

                compute_state_root(state_provider_clone2.clone(), &optimistic_bundle)
            },
        );

        (execution_result?, state_root_result?)
    } else {
        info!(target: "flashblocks::state_executor", "executing flashblock without access list");
        let execution_result = execute_transactions(
            transactions_clone.clone(),
            None,
            &evm_config,
            sealed_header.clone(),
            state_provider_clone.clone(),
            &attributes,
            latest_bundle,
            execution_context_clone.clone(),
            chain_spec,
        )?;

        let converted: HashMap<Address, BundleAccount> =
            execution_result.clone().0.state.into_iter().collect();
        let state_root_result = compute_state_root(state_provider_clone2.clone(), &converted)?;
        (execution_result, state_root_result)
    };

    let elapsed = before_execution.elapsed().as_millis();
    tracing::info!(target: "flashblocks::state_executor", "time taken to get here: {:?}, bal enabled: {}", elapsed, flashblock.diff().access_list_data.is_some());

    let (bundle_state, block_execution_result, _access_list, evm_env, total_fees) =
        execution_result;

    let (state_root, trie_updates, hashed_state) = state_root_result;

    assert_eq!(
        state_root,
        flashblock.diff().state_root,
        "Computed state root does not match state root provided in flashblock"
    );

    let block_assembler = FlashblocksBlockAssembler::new(Arc::new(chain_spec.clone()));

    let block = block_assembler.assemble_block(BlockAssemblerInput::<
        '_,
        '_,
        FlashblocksBlockExecutorFactory,
    >::new(
        evm_env,
        execution_context.clone(),
        &sealed_header_clone.clone(),
        transactions
            .into_iter()
            .map(|tx| tx.into_parts().0)
            .collect(),
        &block_execution_result,
        &bundle_state,
        &state_provider,
        state_root,
    ))?;

    let block = RecoveredBlock::new_unhashed(block, senders);
    let sealed_block = Arc::new(block.sealed_block().clone());

    let recovered_block = Arc::new(block);

    trace!(target: "flashblocks::state_executor", hash = %recovered_block.hash(), "setting latest payload");

    let execution_outcome = ExecutionOutcome::new(
        bundle_state.clone(),
        vec![block_execution_result.receipts],
        0,
        vec![block_execution_result.requests],
    );

    let executed_block = ExecutedBlockWithTrieUpdates::new(
        recovered_block,
        Arc::new(execution_outcome),
        Arc::new(hashed_state),
        ExecutedTrieUpdates::Present(Arc::new(trie_updates)),
    );

    let payload = OpBuiltPayload::new(
        *flashblock.payload_id(),
        sealed_block,
        latest_payload
            .as_ref()
            .map(|p| p.0.fees() + total_fees)
            .unwrap_or(total_fees),
        Some(executed_block),
    );

    flashblocks.push(flashblock)?;
    // construct the full payload
    *latest_payload = Some((payload.clone(), index));

    pending_block.send_replace(payload.executed_block());

    state_executor.broadcast_payload(
        Events::BuiltPayload(payload.clone()),
        payload_events.clone(),
    )?;

    Ok(())
}

#[expect(clippy::too_many_arguments, clippy::type_complexity)]

fn execute_transactions(
    transactions: Vec<Recovered<OpTransactionSigned>>,
    provided_bal_hash: Option<FixedBytes<32>>,
    evm_config: &OpEvmConfig,
    sealed_header: Arc<SealedHeader<Header>>,
    state_provider: Arc<dyn StateProvider>,
    attributes: &OpNextBlockEnvAttributes,
    latest_bundle: Option<BundleState>,
    execution_context: OpBlockExecutionCtx,
    chain_spec: &OpChainSpec,
) -> Result<
    (
        BundleState,
        BlockExecutionResult<OpReceipt>,
        FlashblockAccessList,
        EvmEnv<OpSpecId>,
        U256,
    ),
    eyre::Report,
> {
    // Prepare EVM environment.
    let evm_env = evm_config
        .next_evm_env(sealed_header.clone().header(), attributes)
        .map_err(PayloadBuilderError::other)?;

    let state = StateProviderDatabase::new(&state_provider);

    let mut state = if let Some(ref bundle) = latest_bundle {
        State::builder()
            .with_database(state)
            .with_bundle_prestate(bundle.clone())
            .with_bundle_update()
            .build()
    } else {
        State::builder()
            .with_database(state)
            .with_bundle_update()
            .build()
    };

    let evm = evm_config.evm_with_env(&mut state, evm_env);
    let base_fee = evm.block().basefee;
    let mut executor = BalBuilderBlockExecutor::new(
        evm,
        execution_context.clone(),
        chain_spec,
        OpRethReceiptBuilder::default(),
        0, // TODO: Need to pre-load receipts from the latest payload if available min_tx_index = receipts.len() as u64
    );

    let mut total_fees = U256::ZERO;

    if latest_bundle.is_none() {
        executor
            .apply_pre_execution_changes()
            .map_err(|e| eyre!(format!("failed to apply pre-execution changes: {e}")))?;
    }

    for transaction in transactions.iter() {
        let gas_used = executor
            .execute_transaction(transaction)
            .map_err(|e| eyre!(format!("failed to execute transaction: {e}")))?;

        if !transaction.is_deposit() {
            let miner_fee = transaction
                .effective_tip_per_gas(base_fee)
                .expect("fee is always valid; execution succeeded");
            total_fees += U256::from(miner_fee) * U256::from(gas_used);
        }
    }

    // Apply post execution changes
    let (evm, result, access_list, min_tx_index, max_tx_index) = executor
        .finish_with_access_list()
        .map_err(|e| eyre!(format!("failed to finish execution: {e}")))?;

    let access_list = access_list.build(min_tx_index, max_tx_index);

    // Validate the BAL matches the provided Flashblock BAL
    let expected_bal_hash = keccak256(alloy_rlp::encode(&access_list));

    if provided_bal_hash.is_some() && expected_bal_hash != provided_bal_hash.unwrap() {
        return Err(eyre!(format!(
            "Access List Hash does not match computed hash - expected {:#?} got {:#?}",
            expected_bal_hash,
            provided_bal_hash.unwrap()
        )));
    }

    let (db, env) = evm.finish();

    // merge changes into the db
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

    Ok((
        db.bundle_state.clone(),
        result,
        access_list,
        env,
        total_fees,
    ))
}

pub fn compute_state_root(
    state_provider: Arc<Box<dyn StateProvider>>,
    bundle: &HashMap<Address, BundleAccount>,
) -> Result<(FixedBytes<32>, TrieUpdates, HashedPostState), eyre::Report> {
    let bundle_state: HashMap<&Address, &BundleAccount> = bundle.iter().collect();

    // compute hashed post state
    let hashed_state = HashedPostState::from_bundle_state::<KeccakKeyHasher>(bundle_state);

    // compute state root & trie updates
    let (state_root, trie_updates) = state_provider
        .state_root_with_updates(hashed_state.clone())
        .map_err(BlockExecutionError::other)?;

    Ok((state_root, trie_updates, hashed_state))
}
