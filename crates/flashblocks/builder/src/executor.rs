//! A block executor and builder for flashblocks that constructs a BAL (Block Access List) sidecar.

use alloy_op_evm::{
    OpBlockExecutionCtx, OpBlockExecutor, OpBlockExecutorFactory, OpEvmFactory,
    block::receipt_builder::OpReceiptBuilder,
};
use op_alloy_consensus::OpReceipt;
use reth_evm::{
    Database, Evm, EvmFactory, FromRecoveredTx, FromTxWithEncoded, OnStateHook,
    block::{
        BlockExecutionError, BlockExecutor, BlockExecutorFactory, BlockExecutorFor,
        StateChangeSource, StateDB,
    },
    op_revm::{OpSpecId, OpTransaction},
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpRethReceiptBuilder;
use reth_optimism_primitives::OpTransactionSigned;
use reth_payload_primitives::BuiltPayload;
use reth_provider::BlockExecutionResult;
use revm::{
    DatabaseCommit,
    context::{BlockEnv, TxEnv, result::ResultAndState},
    database::{BundleState, states::StateChangeset},
};

use crate::{
    access_list::BlockAccessIndex,
    database::bal_builder_db::{AccessIndex, AsyncBalBuilderDb},
};
use alloy_consensus::{Block, BlockHeader, Header, transaction::TxHashRef};
use alloy_primitives::{FixedBytes, U256};
use flashblocks_primitives::access_list::FlashblockAccessList;
use reth_evm::{
    block::CommitChanges,
    execute::{
        BasicBlockBuilder, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome, ExecutorTx,
    },
    op_revm::OpHaltReason,
};
use reth_optimism_node::{OpBlockAssembler, OpBuiltPayload};
use reth_primitives::{NodePrimitives, Recovered, RecoveredBlock, SealedHeader};
use reth_provider::StateProvider;
use revm::{
    context::result::ExecutionResult,
    database::states::{bundle_state::BundleRetention, reverts::Reverts},
};
use std::{
    borrow::Cow,
    collections::HashSet,
    sync::{Arc, OnceLock, atomic::AtomicU16},
};

/// Errors that occur during block validation (hash/root comparisons).
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum ValidationError {
    #[error("State root mismatch: expected {expected:?}, got {got:?}")]
    StateRootMismatch {
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
    },
    #[error("Receipts root mismatch: expected {expected:?}, got {got:?}")]
    ReceiptsRootMismatch {
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
    },
    #[error("Access list hash mismatch: expected {expected:?}, got {got:?}")]
    AccessListHashMismatch {
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
        expected_access_list: FlashblockAccessList,
        constructed_access_list: FlashblockAccessList,
    },
    #[error("Block hash mismatch: expected {expected:?}, got {got:?}")]
    BlockHashMismatch {
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
    },
}

#[derive(thiserror::Error, Debug)]
pub enum BalExecutorError {
    #[error("Block execution error: {0}")]
    BlockExecutionError(#[from] BlockExecutionError),
    #[error("Missing executed block in built payload")]
    MissingExecutedBlock,
    #[error("Missing access list data in flashblock diff")]
    MissingAccessListData,
    #[error("Validation failed: {0}")]
    Validation(#[from] ValidationError),
    #[error("Channel error: {0}")]
    ChannelError(String),
    #[error("Internal error: {0}")]
    Other(#[from] Box<dyn core::error::Error + Send + Sync>),
}

impl BalExecutorError {
    pub fn other<E: core::error::Error + Send + Sync + 'static>(err: E) -> Self {
        BalExecutorError::Other(Box::new(err))
    }
}

pub struct BalBlockExecutor<E, R: OpReceiptBuilder> {
    pub inner: OpBlockExecutor<E, R, Arc<OpChainSpec>>,
}

impl<E, R> BalBlockExecutor<E, R>
where
    E: Evm,
    R: OpReceiptBuilder,
{
    /// Creates a new [`OpBlockExecutor`].
    pub fn new(
        evm: E,
        ctx: OpBlockExecutionCtx,
        spec: Arc<OpChainSpec>,
        receipt_builder: R,
    ) -> Self {
        let inner = OpBlockExecutor::new(evm, ctx, spec, receipt_builder);
        Self { inner }
    }

    pub fn with_gas_used(mut self, gas_used: u64) -> Self {
        self.inner.gas_used = gas_used;
        self
    }

    pub fn with_receipts(mut self, receipts: Vec<R::Receipt>) -> Self {
        self.inner.receipts = receipts;
        self
    }
}

impl<E, DB, R> BlockExecutor for BalBlockExecutor<E, R>
where
    DB: StateDB + DatabaseCommit + Database,
    E: Evm<DB = DB, Tx = OpTransaction<TxEnv>, Spec = OpSpecId, BlockEnv = BlockEnv>,
    OpTransaction<TxEnv>: FromTxWithEncoded<R::Transaction> + FromRecoveredTx<R::Transaction>,
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt>,
{
    type Evm = E;
    type Receipt = R::Receipt;
    type Transaction = R::Transaction;

    fn apply_pre_execution_changes(&mut self) -> Result<(), reth_evm::block::BlockExecutionError> {
        self.inner.apply_pre_execution_changes()
    }

    fn evm(&self) -> &Self::Evm {
        self.inner.evm()
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        self.inner.evm_mut()
    }

    fn execute_transaction(
        &mut self,
        tx: impl reth_evm::block::ExecutableTx<Self>,
    ) -> Result<u64, reth_evm::block::BlockExecutionError> {
        self.inner.execute_transaction(tx)
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl reth_evm::block::ExecutableTx<Self>,
    ) -> Result<ResultAndState<<Self::Evm as Evm>::HaltReason>, BlockExecutionError> {
        self.inner.execute_transaction_without_commit(tx)
    }

    fn commit_transaction(
        &mut self,
        output: revm::context::result::ResultAndState<<Self::Evm as reth_evm::Evm>::HaltReason>,
        tx: impl reth_evm::block::ExecutableTx<Self>,
    ) -> Result<u64, reth_evm::block::BlockExecutionError> {
        self.inner.commit_transaction(output, tx)
    }

    fn apply_post_execution_changes(
        self,
    ) -> Result<BlockExecutionResult<Self::Receipt>, BlockExecutionError>
    where
        Self: Sized,
    {
        self.inner.apply_post_execution_changes()
    }

    fn finish(
        self,
    ) -> Result<(Self::Evm, BlockExecutionResult<Self::Receipt>), BlockExecutionError> {
        self.inner.finish()
    }

    fn set_state_hook(&mut self, hook: Option<Box<dyn reth_evm::OnStateHook>>) {
        self.inner.set_state_hook(hook)
    }
}

/// Ethereum block executor factory.
#[derive(Debug, Clone)]
pub struct BalBlockExecutorFactory {
    inner: OpBlockExecutorFactory<OpRethReceiptBuilder, OpChainSpec>,
}

impl BalBlockExecutorFactory {
    /// Creates a new [`OpBlockExecutorFactory`] with the given spec, [`EvmFactory`], and
    /// [`OpReceiptBuilder`].
    pub const fn new(
        receipt_builder: OpRethReceiptBuilder,
        spec: OpChainSpec,
        evm_factory: OpEvmFactory,
    ) -> Self {
        Self {
            inner: OpBlockExecutorFactory::new(receipt_builder, spec, evm_factory),
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
}

impl BlockExecutorFactory for BalBlockExecutorFactory {
    type EvmFactory = OpEvmFactory;
    type ExecutionCtx<'a> = OpBlockExecutionCtx;
    type Transaction = OpTransactionSigned;
    type Receipt = OpReceipt;

    fn evm_factory(&self) -> &Self::EvmFactory {
        self.inner.evm_factory()
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: <OpEvmFactory as EvmFactory>::Evm<DB, I>,
        ctx: Self::ExecutionCtx<'a>,
    ) -> impl BlockExecutorFor<'a, Self, DB, I>
    where
        DB: StateDB + Database + DatabaseCommit + 'a,
        I: revm::Inspector<<OpEvmFactory as EvmFactory>::Context<DB>> + 'a,
        OpEvmFactory: EvmFactory<Tx = OpTransaction<TxEnv>>,
    {
        BalBlockExecutor::new(
            evm,
            ctx,
            self.spec().clone().into(),
            OpRethReceiptBuilder::default(),
        )
    }
}

/// A wrapper around the [`BasicBlockBuilder`] for flashblocks.
pub struct BalBlockBuilder<'a, R: OpReceiptBuilder, N: NodePrimitives, Evm> {
    pub inner: BasicBlockBuilder<
        'a,
        BalBlockExecutorFactory,
        BalBlockExecutor<Evm, R>,
        OpBlockAssembler<OpChainSpec>,
        N,
    >,
    pub access_list_sender: crossbeam_channel::Sender<FlashblockAccessList>,
    pub indexes: (u16, AccessIndex),
}

impl<'a, DB, R, N: NodePrimitives, E> BalBlockBuilder<'a, R, N, E>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt>,
    DB: StateDB + DatabaseCommit + Database + 'a,
    E: Evm<
            DB = AsyncBalBuilderDb<DB>,
            Tx = OpTransaction<TxEnv>,
            Spec = OpSpecId,
            BlockEnv = BlockEnv,
        >,
    OpTransaction<TxEnv>: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
{
    /// Creates a new [`BalBlockBuilder`] with the given executor factory and assembler.
    ///
    /// The `base_index` parameter sets the starting index for this segment.
    /// The index is automatically managed by the [`BalIndexHook`] which should be
    /// set on the executor before calling this.
    ///
    /// Index pattern:
    /// - Pre-execution: `base_index`
    /// - Transactions: `base_index + 1`, `base_index + 2`, etc.
    /// - Post-execution: `base_index + tx_count + 1`
    pub fn new(
        ctx: OpBlockExecutionCtx,
        parent: &'a SealedHeader<N::BlockHeader>,
        executor: BalBlockExecutor<E, R>,
        transactions: Vec<Recovered<N::SignedTx>>,
        chain_spec: Arc<OpChainSpec>,
        tx: crossbeam_channel::Sender<FlashblockAccessList>,
        index: AccessIndex,
    ) -> Self {
        Self {
            inner: BasicBlockBuilder {
                executor,
                assembler: OpBlockAssembler::new(chain_spec),
                ctx,
                parent,
                transactions,
            },
            access_list_sender: tx,
            indexes: (index.index(), index),
        }
    }
}

impl<'a, DB, R, N, E> BlockBuilder for BalBlockBuilder<'a, R, N, E>
where
    DB: StateDB + DatabaseCommit + Database + 'a,
    N: NodePrimitives<
            Receipt = OpReceipt,
            SignedTx = OpTransactionSigned,
            Block = Block<OpTransactionSigned>,
            BlockHeader = Header,
        >,
    E: Evm<
            DB = AsyncBalBuilderDb<DB>,
            Tx = OpTransaction<TxEnv>,
            Spec = OpSpecId,
            HaltReason = OpHaltReason,
            BlockEnv = BlockEnv,
        >,
    R: OpReceiptBuilder<Receipt = OpReceipt, Transaction = OpTransactionSigned>,
    OpTransaction<TxEnv>:
        FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
{
    type Primitives = N;
    type Executor = BalBlockExecutor<E, R>;

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
        let (mut db, evm_env) = evm.finish();

        // merge all transitions into bundle state
        db.merge_transitions(BundleRetention::Reverts);

        // flatten reverts into a single reverts as the bundle is re-used across multiple payloads
        // which represent a single atomic state transition. therefore reverts should have length 1
        // we only retain the first occurance of a revert for any given account.
        let flattened = db
            .bundle_state()
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

        db.bundle_state_mut().reverts = Reverts::new(vec![flattened]);

        // calculate the state root
        let hashed_state = state.hashed_post_state(db.bundle_state());
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
            BalBlockExecutorFactory,
        >::new(
            evm_env,
            self.inner.ctx,
            self.inner.parent,
            transactions,
            &result,
            Cow::Borrowed(db.bundle_state()),
            &state,
            state_root,
        ))?;

        let block = RecoveredBlock::new_unhashed(block, senders);

        let access_list = db.finish()?.build((self.indexes.0, self.indexes.1.index()));

        self.access_list_sender
            .send(access_list)
            .map_err(BlockExecutionError::other)?;

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

#[derive(Default, Debug, Clone)]

pub struct CommittedState<R: OpReceiptBuilder + Default = OpRethReceiptBuilder> {
    pub gas_used: u64,
    pub fees: U256,
    pub bundle: BundleState,
    pub receipts: Vec<(BlockAccessIndex, R::Receipt)>,
    pub transactions: Vec<(BlockAccessIndex, Recovered<R::Transaction>)>,
}

impl<R> CommittedState<R>
where
    R: OpReceiptBuilder + Default,
{
    pub fn transactions_iter(&self) -> impl Iterator<Item = &'_ Recovered<R::Transaction>> + '_ {
        self.transactions.iter().map(|(_, tx)| tx)
    }

    pub fn transaction_hashes_iter(&self) -> impl Iterator<Item = FixedBytes<32>> + '_
    where
        R::Transaction: Clone + TxHashRef,
    {
        self.transactions
            .iter()
            .map(|(_, tx)| tx.tx_hash())
            .copied()
    }

    pub fn receipts_iter(&self) -> impl Iterator<Item = &'_ R::Receipt> + '_ {
        self.receipts.iter().map(|(_, r)| r)
    }
}

impl<R> TryFrom<Option<&OpBuiltPayload>> for CommittedState<R>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt> + Default,
{
    type Error = BalExecutorError;

    fn try_from(value: Option<&OpBuiltPayload>) -> Result<Self, Self::Error> {
        if let Some(value) = value {
            let executed_block = value
                .executed_block()
                .ok_or(BalExecutorError::MissingExecutedBlock)?;

            let gas_used = executed_block.recovered_block.gas_used();

            let bundle = value
                .executed_block()
                .ok_or(BalExecutorError::MissingExecutedBlock)?
                .execution_output
                .bundle
                .clone();

            let fees = value.fees();

            let transactions: Vec<_> = executed_block
                .recovered_block
                .clone_transactions_recovered()
                .enumerate()
                .map(|(index, tx)| (index as BlockAccessIndex, tx))
                .collect();

            let receipts: Vec<_> = executed_block
                .execution_output
                .receipts()
                .iter()
                .flatten()
                .cloned()
                .enumerate()
                .map(|(index, r)| (index as BlockAccessIndex, r))
                .collect();

            Ok(Self {
                transactions,
                receipts,
                gas_used,
                fees,
                bundle,
            })
        } else {
            Ok(Self {
                transactions: vec![],
                receipts: vec![],
                gas_used: 0,
                fees: U256::ZERO,
                bundle: BundleState::default(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
}
