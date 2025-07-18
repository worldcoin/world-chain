use std::sync::Arc;

use alloy_consensus::{Block, Transaction, TxReceipt};
use alloy_eips::eip7685::Requests;
use alloy_eips::Encodable2718;
use alloy_op_evm::{block::receipt_builder::OpReceiptBuilder, OpBlockExecutor};
use alloy_op_evm::{OpBlockExecutionCtx, OpBlockExecutorFactory, OpEvmFactory};
use alloy_primitives::{Address, U256};
use reth::core::primitives::Receipt;
use reth::revm::State;
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
use reth_evm::{Evm, EvmFactory};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::{OpBlockAssembler, OpRethReceiptBuilder};
use reth_optimism_primitives::{DepositReceipt, OpReceipt, OpTransactionSigned};
use reth_primitives::{transaction::SignedTransaction, SealedHeader};
use reth_primitives::{NodePrimitives, Recovered};
use reth_provider::{BlockExecutionResult, StateProvider};
use revm::context::result::ExecutionResult;
use revm::database::BundleState;

/// This type wraps the [`OpBlockExecutor`] and provides a way to execute flashblocks
/// with the correct context and state management from prior flashblocks.
pub struct FlashblocksBlockExecutor<Evm, R>
where
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt>,
{
    /// The total flashblocks that have been executed.
    pub total_flashblocks: u64,
    /// The index of the current flashblock in the transactions, and receipts.
    pub current_flashblock_offset: u64,
    /// Aggregated receipts.
    pub receipts: Vec<R::Receipt>,
    /// Latest flashblocks bundle state.
    pub bundle_prestate: BundleState,
    /// All executed transactions (unrecovered).
    pub executed_transactions: Vec<R::Transaction>,
    /// The recovered senders for the executed transactions.
    pub executed_senders: Vec<Address>,
    /// All gas used so far
    pub cumulative_gas_used: u64,
    /// Estimated DA size
    pub cumulative_da_bytes_used: u64,
    /// Tracks fees from executed mempool transactions
    pub total_fees: U256,
    /// The inner block executor.
    /// This is used to execute the block and commit changes.
    inner: OpBlockExecutor<Evm, R, OpChainSpec>,
}

impl<'a, E, DB, R> BlockExecutor for FlashblocksBlockExecutor<E, R>
where
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt>,
    DB: Database + 'a,
    E: Evm<
        DB = &'a mut State<DB>,
        Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
    >,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Evm = E;

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        f: impl FnOnce(&ExecutionResult<<Self::Evm as Evm>::HaltReason>) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        let result = self
            .inner
            .execute_transaction_with_commit_condition(tx, f)?;
        Ok(result)
    }

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.inner.apply_pre_execution_changes()
    }

    fn finish(
        mut self,
    ) -> Result<(Self::Evm, BlockExecutionResult<R::Receipt>), BlockExecutionError> {
        let (
            mut evm,
            BlockExecutionResult {
                receipts,
                requests: _,
                gas_used,
            },
        ) = self.inner.finish()?;

        self.receipts.extend_from_slice(&receipts);
        self.cumulative_gas_used = gas_used + self.cumulative_gas_used;
        self.bundle_prestate = evm.db_mut().take_bundle();
        self.total_flashblocks += 1;

        Ok((
            evm,
            BlockExecutionResult {
                receipts: self.receipts,
                gas_used: self.cumulative_gas_used,
                requests: Requests::default(),
            },
        ))
    }

    fn set_state_hook(&mut self, _hook: Option<Box<dyn OnStateHook>>) {
        self.inner.set_state_hook(_hook)
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        self.inner.evm_mut()
    }

    fn evm(&self) -> &Self::Evm {
        self.inner.evm()
    }

    fn execute_transaction(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<u64, BlockExecutionError> {
        self.inner.execute_transaction(tx)
    }

    fn apply_post_execution_changes(
        self,
    ) -> Result<BlockExecutionResult<Self::Receipt>, BlockExecutionError>
    where
        Self: Sized,
    {
        self.finish().map(|(_, result)| result)
    }

    fn with_state_hook(mut self, hook: Option<Box<dyn OnStateHook>>) -> Self
    where
        Self: Sized,
    {
        self.set_state_hook(hook);
        self
    }

    fn execute_block(
        mut self,
        transactions: impl IntoIterator<Item = impl ExecutableTx<Self>>,
    ) -> Result<BlockExecutionResult<Self::Receipt>, BlockExecutionError>
    where
        Self: Sized,
    {
        self.apply_pre_execution_changes()?;

        for tx in transactions {
            self.execute_transaction(tx)?;
        }

        self.apply_post_execution_changes()
    }
}

impl<'a, E, DB, R> FlashblocksBlockExecutor<E, R>
where
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt>,
    E: Evm<
        DB = &'a mut State<DB>,
        Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
    >,
    DB: Database + 'a,
{
    pub fn new(evm: E, spec: OpChainSpec, receipt_builder: R, ctx: OpBlockExecutionCtx) -> Self {
        let inner = OpBlockExecutor::new(evm, ctx, spec, receipt_builder);

        Self {
            total_flashblocks: 0,
            executed_transactions: Vec::new(),
            executed_senders: Vec::new(),
            current_flashblock_offset: 0,
            bundle_prestate: BundleState::default(),
            cumulative_gas_used: 0,
            cumulative_da_bytes_used: 0,
            total_fees: U256::ZERO,
            receipts: Vec::new(),
            inner,
        }
    }

    /// Extends the [`BundleState`] of the inner executor with a specified pre-image.
    ///
    /// This should be used _only_ when initializing the executor
    pub fn with_bundle_prestate(mut self, pre_state: BundleState) -> Self {
        self.inner.evm_mut().db_mut().bundle_state.extend(pre_state);
        self
    }

    /// Extends the receipts and gas used to reflect the aggregated execution result
    /// of prior flashblocks as a pre state to the current.
    pub fn with_execution_result(
        mut self,
        execution_result: BlockExecutionResult<R::Receipt>,
    ) -> Self {
        self.receipts.extend(execution_result.receipts);
        self.cumulative_gas_used += execution_result.gas_used;
        self
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
        &self.inner.spec()
    }

    /// Exposes the EVM factory.
    pub const fn evm_factory(&self) -> &OpEvmFactory {
        &self.inner.evm_factory()
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
        &self.inner.evm_factory()
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
                self.spec().clone(),
                OpRethReceiptBuilder::default(),
                ctx,
            )
            .with_bundle_prestate(pre_state.clone()); // TODO: Terrible clone here
        }

        FlashblocksBlockExecutor::new(
            evm,
            self.spec().clone(),
            OpRethReceiptBuilder::default(),
            ctx,
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
        FlashblocksBlockExecutor<Evm, OpRethReceiptBuilder>,
        OpBlockAssembler<OpChainSpec>,
        N,
    >,
}

impl<'a, N: NodePrimitives, Evm> FlashblocksBlockBuilder<'a, N, Evm> {
    /// Creates a new [`FlashblocksBlockBuilder`] with the given executor factory and assembler.
    pub fn new(
        ctx: OpBlockExecutionCtx,
        parent: &'a SealedHeader<N::BlockHeader>,
        executor: FlashblocksBlockExecutor<Evm, OpRethReceiptBuilder>,
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
    >,
    E: Evm<
        DB = &'a mut State<DB>,
        Tx: FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
        Spec = OpSpecId,
        HaltReason = OpHaltReason,
    >,
{
    type Primitives = N;
    type Executor = FlashblocksBlockExecutor<E, OpRethReceiptBuilder>;

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
        self.inner.finish(state)
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
