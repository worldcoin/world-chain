//! A block executor and builder for flashblocks that constructs a BAL (Block Access List) sidecar.

use alloy_op_evm::{
    OpBlockExecutionCtx, OpBlockExecutor, OpBlockExecutorFactory,
    block::receipt_builder::OpReceiptBuilder,
};
use op_alloy_consensus::OpReceipt;
use reth_evm::{
    Database, Evm, FromRecoveredTx, FromTxWithEncoded,
    block::{BlockExecutionError, BlockExecutor, InternalBlockExecutionError, StateDB},
    op_revm::{OpSpecId, OpTransaction},
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpRethReceiptBuilder;
use reth_optimism_primitives::OpTransactionSigned;
use reth_payload_primitives::BuiltPayload;
use revm::{
    DatabaseCommit,
    context::{BlockEnv, TxEnv},
    database::BundleState,
};
use tracing::trace;

use crate::{access_list::BlockAccessIndex, database::bal_builder_db::BalBuilderDb};
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
use std::{borrow::Cow, collections::HashSet, sync::Arc};

#[derive(thiserror::Error, Debug, serde::Serialize)]
pub enum BalValidationError {
    #[error("Block execution error")]
    BalHashMismatch {
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
        expected_bal: FlashblockAccessList,
        got_bal: FlashblockAccessList,
    },
    #[error("State Root Mismatch: expected {expected:?}, got {got:?}")]
    StateRootMismatch {
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
        bundle_state: BundleState,
    },
    #[error("Receipts Root Mismatch: expected {expected:?}, got: {got:?}")]
    ReceiptsRootMismatch {
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
    },
}

impl BalValidationError {
    pub fn boxed(self) -> Box<Self> {
        Box::new(self)
    }
}

#[derive(thiserror::Error, Debug, serde::Serialize)]
pub enum BalExecutorError {
    #[error(transparent)]
    #[serde(skip_serializing)]
    BlockExecutionError(#[from] BlockExecutionError),
    #[error("Missing executed block in built payload")]
    MissingExecutedBlock,
    #[error(transparent)]
    BalValidationError(#[from] Box<BalValidationError>),
    #[error("Inernal Error: {0}")]
    #[serde(skip_serializing)]
    Other(#[from] Box<dyn core::error::Error + Send + Sync>),
}

impl BalExecutorError {
    pub fn other<E: core::error::Error + Send + Sync + 'static>(err: E) -> Self {
        BalExecutorError::Other(Box::new(err))
    }
}

/// A wrapper around the [`BasicBlockBuilder`] for flashblocks.
pub struct BalBlockBuilder<'a, R: OpReceiptBuilder, N: NodePrimitives, Evm> {
    pub inner: BasicBlockBuilder<
        'a,
        OpBlockExecutorFactory<OpRethReceiptBuilder, OpChainSpec>,
        OpBlockExecutor<Evm, R, Arc<OpChainSpec>>,
        OpBlockAssembler<OpChainSpec>,
        N,
    >,
    pub access_list_sender: crossbeam_channel::Sender<FlashblockAccessList>,
    pub counter: BlockAccessIndexCounter,
}

impl<'a, DB, R, N: NodePrimitives, E> BalBlockBuilder<'a, R, N, E>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt>,
    DB: StateDB + DatabaseCommit + Database + 'a,
    E: Evm<DB = BalBuilderDb<DB>, Tx = OpTransaction<TxEnv>, Spec = OpSpecId, BlockEnv = BlockEnv>,
    OpTransaction<TxEnv>: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
{
    /// Creates a new [`FlashblocksBlockBuilder`] with the given executor factory and assembler.
    pub fn new(
        ctx: OpBlockExecutionCtx,
        parent: &'a SealedHeader<N::BlockHeader>,
        mut executor: OpBlockExecutor<E, R, Arc<OpChainSpec>>,
        transactions: Vec<Recovered<N::SignedTx>>,
        chain_spec: Arc<OpChainSpec>,
        tx: crossbeam_channel::Sender<FlashblockAccessList>,
    ) -> Self {
        let start_index = if transactions.is_empty() {
            0
        } else {
            transactions.len() as u16 + 1
        };

        executor.evm_mut().db_mut().set_index(start_index as u16);
        let counter = BlockAccessIndexCounter::new(start_index as u16);

        trace!(target: "bal_executor", parent = %parent.hash(), block_access_index = %start_index, "Setting initial database index for block builder");

        Self {
            inner: BasicBlockBuilder {
                executor,
                assembler: OpBlockAssembler::new(chain_spec),
                ctx,
                parent,
                transactions,
            },
            access_list_sender: tx,
            counter,
        }
    }

    pub fn prepare_database(&mut self) -> Result<(), BlockExecutionError> {
        let length = self.inner.transactions.len() as u16;
        let db = self.inner.executor.evm_mut().db_mut();
        let current = self.counter.inc();
        trace!(target: "bal_executor", block_access_index = %current,  receipts_length = %length, "Preparing database for next transaction with index");
        db.set_index(current);
        Ok(())
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
            DB = BalBuilderDb<DB>,
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
    type Executor = OpBlockExecutor<E, R, Arc<OpChainSpec>>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        let res = self.inner.apply_pre_execution_changes();
        // prepare the transaction index for the next transaction 1
        self.prepare_database()?;
        res
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutorTx<Self::Executor>,
        f: impl FnOnce(
            &ExecutionResult<<<Self::Executor as BlockExecutor>::Evm as Evm>::HaltReason>,
        ) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        let res = self.inner.execute_transaction_with_commit_condition(tx, f);
        // prepare the transaction index for the next transaction
        self.prepare_database()?;
        res
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
            OpBlockExecutorFactory<OpRethReceiptBuilder, OpChainSpec>,
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

        let access_list = db
            .finish()
            .map_err(InternalBlockExecutionError::other)?
            .build(self.counter.finish());

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

/// Simple counter to track sub pre-confirmation block access index bounds.
pub struct BlockAccessIndexCounter {
    current_index: u16,
    start_index: u16,
}

impl BlockAccessIndexCounter {
    pub fn new(start_index: u16) -> Self {
        Self {
            current_index: start_index,
            start_index,
        }
    }

    pub fn inc(&mut self) -> u16 {
        self.current_index += 1;
        self.current_index
    }

    pub fn finish(self) -> (u16, u16) {
        (self.start_index, self.current_index)
    }
}

/// [`CommittedState`] holds all relevant information about the intra block state committment
/// for which we are executing on top of.
#[derive(Default, Debug, Clone)]

pub struct CommittedState<R: OpReceiptBuilder + Default = OpRethReceiptBuilder> {
    /// The total gas used in previous committed transactions.
    pub gas_used: u64,
    /// The total fees accumulated in previous committed transactions.
    pub fees: U256,
    /// The bundle state accumulated so far from the State Transitions
    pub bundle: BundleState,
    /// Ordered receipts of previous committed transactions.
    pub receipts: Vec<(BlockAccessIndex, R::Receipt)>,
    /// Ordered transactions which have been executed
    pub transactions: Vec<(BlockAccessIndex, Recovered<R::Transaction>)>,
}

impl<R> CommittedState<R>
where
    R: OpReceiptBuilder + Default,
{
    /// Iterator over committed transactions
    pub fn transactions_iter(&self) -> impl Iterator<Item = &'_ Recovered<R::Transaction>> + '_ {
        self.transactions.iter().map(|(_, tx)| tx)
    }

    /// Iterator over committed transaction hashes
    pub fn transaction_hashes_iter(&self) -> impl Iterator<Item = FixedBytes<32>> + '_
    where
        R::Transaction: Clone + TxHashRef,
    {
        self.transactions
            .iter()
            .map(|(_, tx)| tx.tx_hash())
            .copied()
    }

    /// Iterator over committed receipts
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

    // #[test]
    // fn test_block_access_index_counter_new() {
    //     let counter = BlockAccessIndexCounter::new(10);
    //     let (start, end) = counter.finish();

    //     assert_eq!(start, 10);
    //     assert_eq!(end, 10, "No indices consumed yet");
    // }

    // #[test]
    // fn test_block_access_index_counter_next_index() {
    //     let mut counter = BlockAccessIndexCounter::new(0);

    //     assert_eq!(counter.inc(), 0);
    //     assert_eq!(counter.next_index(), 1);
    //     assert_eq!(counter.next_index(), 2);
    // }

    // #[test]
    // fn test_block_access_index_counter_finish() {
    //     let mut counter = BlockAccessIndexCounter::new(5);

    //     counter.next_index(); // 5
    //     counter.next_index(); // 6
    //     counter.next_index(); // 7

    //     let (start, end) = counter.finish();
    //     assert_eq!(start, 5);
    //     assert_eq!(end, 8);
    // }
}
