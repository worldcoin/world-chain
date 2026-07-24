//! A block executor and builder for flashblocks that constructs a BAL (Block Access List) sidecar.

use alloy_op_evm::{
    OpBlockExecutionCtx, OpBlockExecutor, OpBlockExecutorFactory, OpTx,
    block::receipt_builder::OpReceiptBuilder, post_exec::PostExecEvm,
};
use op_alloy_consensus::OpReceipt;
use op_revm::{
    OpHaltReason, OpSpecId,
    constants::{
        DA_FOOTPRINT_GAS_SCALAR_SLOT, ECOTONE_L1_BLOB_BASE_FEE_SLOT, ECOTONE_L1_FEE_SCALARS_SLOT,
        L1_BASE_FEE_SLOT, L1_BLOCK_CONTRACT, L1_OVERHEAD_SLOT, L1_SCALAR_SLOT,
    },
};
use reth_evm::{
    Database, Evm, RecoveredTx,
    block::{BlockExecutionError, BlockExecutor, InternalBlockExecutionError},
};
use reth_node_api::NodePrimitives;
use reth_optimism_payload_builder::OpBuiltPayload;
use reth_optimism_primitives::OpTransactionSigned;
use reth_payload_primitives::BuiltPayload;
use reth_primitives_traits::{Recovered, RecoveredBlock, SealedHeader};
use reth_trie_common::updates::TrieUpdates;
use revm::{context::BlockEnv, database::BundleState};
use revm_database::State;
use tracing::trace;

use crate::{
    BlockBuilderExt, FlashblockExecutionMetrics, OpBlockAssembler, OpRethReceiptBuilder,
    PayloadBuildStage,
};
use alloy_consensus::{Block, BlockHeader, Header, transaction::TxHashRef};
use alloy_eip7928::BlockAccessIndex;
use alloy_primitives::{B256, FixedBytes, U256};
use reth_evm::{
    block::CommitChanges,
    execute::{BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome, ExecutorTx, GasOutput},
};
use reth_provider::StateProvider;
use revm::database::states::bundle_state::BundleRetention;
use std::{sync::Arc, time::Instant};
use world_chain_chainspec::WorldChainSpec;
use world_chain_primitives::access_list::FlashblockAccessList;

const OP_L1_BLOCK_BAL_READ_SLOTS: [U256; 6] = [
    L1_BASE_FEE_SLOT,
    L1_OVERHEAD_SLOT,
    L1_SCALAR_SLOT,
    ECOTONE_L1_BLOB_BASE_FEE_SLOT,
    ECOTONE_L1_FEE_SCALARS_SLOT,
    DA_FOOTPRINT_GAS_SCALAR_SLOT,
];

/// Records the OP L1 block predeploy slots that OP fee accounting reads through
/// the database outside the returned transaction state.
///
/// The L1 info deposit updates the predeploy through normal EVM execution, but
/// Jovian DA-footprint and post-exec fee accounting read these slots before the
/// transaction result is committed. Upstream BAL construction only sees the
/// committed result state, so these read-only slots need to be inserted into
/// upstream's BAL builder explicitly.
pub fn record_op_l1_block_bal_reads<DB>(db: &mut State<DB>) -> Result<(), BlockExecutionError> {
    let Some(bal_builder) = &mut db.bal_state.bal_builder else {
        return Err(BlockExecutionError::msg("missing BAL builder state"));
    };

    let account = bal_builder.accounts.entry(L1_BLOCK_CONTRACT).or_default();
    for slot in OP_L1_BLOCK_BAL_READ_SLOTS {
        account.storage.storage.entry(slot).or_default();
    }

    Ok(())
}

/// Reconstructs pre-refund EVM gas for committed transactions.
///
/// The block header and receipts carry canonical gas after SDM/post-exec refunds. The matching
/// post-exec transaction carries the refund entries, so adding them back gives the executor's
/// pre-refund gas counter for the committed payload.
pub fn pre_refund_gas_used<'a>(
    gas_used: u64,
    transactions: impl IntoIterator<Item = &'a Recovered<OpTransactionSigned>>,
) -> u64 {
    let gas_refunded = transactions
        .into_iter()
        .filter_map(|tx| tx.tx().as_post_exec())
        .flat_map(|tx| tx.inner().payload.gas_refund_entries.iter())
        .fold(0u64, |sum, entry| sum.saturating_add(entry.gas_refund));

    gas_used.saturating_add(gas_refunded)
}

#[derive(thiserror::Error, Debug, serde::Serialize)]
pub enum BalValidationError {
    #[error("BAL hash mismatch: expected {expected:?}, got {got:?}")]
    BalHashMismatch {
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
        expected_bal: FlashblockAccessList,
        got_bal: FlashblockAccessList,
    },
    #[error("BAL {index} tx index mismatch: expected {expected}, got {got}")]
    BalIndexMismatch {
        index: &'static str,
        expected: u64,
        got: u64,
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
    #[error("Missing access list data")]
    MissingAccessListData,
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
    pub executor: OpBlockExecutor<Evm, R, Arc<WorldChainSpec>>,
    pub ctx: OpBlockExecutionCtx,
    pub transactions: Vec<Recovered<N::SignedTx>>,
    pub parent: &'a SealedHeader<N::BlockHeader>,
    pub assembler: OpBlockAssembler<WorldChainSpec>,
    pub access_list_sender: crossbeam_channel::Sender<FlashblockAccessList>,
    pub counter: BlockAccessIndexCounter,
    pub committed_bundle: BundleState,
}

impl<'a, DB, R, N: NodePrimitives, E> BalBlockBuilder<'a, R, N, E>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt>,
    DB: Database + 'a,
    E: Evm<DB = &'a mut State<DB>, Tx = OpTx, Spec = OpSpecId, BlockEnv = BlockEnv>,
    E: PostExecEvm,
    OpBlockExecutor<E, R, Arc<WorldChainSpec>>:
        BlockExecutor<Evm = E, Transaction = OpTransactionSigned, Receipt = OpReceipt>,
{
    /// Creates a new [`FlashblocksBlockBuilder`] with the given executor factory and assembler.
    pub fn new(
        ctx: OpBlockExecutionCtx,
        parent: &'a SealedHeader<N::BlockHeader>,
        mut executor: OpBlockExecutor<E, R, Arc<WorldChainSpec>>,
        transactions: Vec<Recovered<N::SignedTx>>,
        chain_spec: Arc<WorldChainSpec>,
        tx: crossbeam_channel::Sender<FlashblockAccessList>,
        committed_bundle: BundleState,
    ) -> Self {
        let start_index = if transactions.is_empty() {
            0
        } else {
            transactions.len() as u64 + 1
        };

        executor
            .evm_mut()
            .db_mut()
            .set_bal_index(BlockAccessIndex::new(start_index));
        let counter = BlockAccessIndexCounter::new(start_index);

        trace!(target: "bal_executor", parent = %parent.hash(), block_access_index = %start_index, "Setting initial database index for block builder");

        Self {
            executor,
            ctx,
            transactions,
            parent,
            assembler: OpBlockAssembler::new(chain_spec),
            access_list_sender: tx,
            counter,
            committed_bundle,
        }
    }

    pub fn prepare_database(&mut self) -> Result<(), BlockExecutionError> {
        let length = self.transactions.len() as u64;
        let db = self.executor.evm_mut().db_mut();
        let current = self.counter.inc();
        trace!(target: "bal_executor", block_access_index = %current,  receipts_length = %length, "Preparing database for next transaction with index");
        db.set_bal_index(BlockAccessIndex::new(current));
        Ok(())
    }
}

impl<'a, DB, R, N, E> BlockBuilder for BalBlockBuilder<'a, R, N, E>
where
    DB: Database + 'a,
    N: NodePrimitives<
            Receipt = OpReceipt,
            SignedTx = OpTransactionSigned,
            Block = Block<OpTransactionSigned>,
            BlockHeader = Header,
        >,
    E: Evm<
            DB = &'a mut State<DB>,
            Tx = OpTx,
            Spec = OpSpecId,
            HaltReason = OpHaltReason,
            BlockEnv = BlockEnv,
        >,
    E: PostExecEvm,
    R: OpReceiptBuilder<Receipt = OpReceipt, Transaction = OpTransactionSigned>,
    OpBlockExecutor<E, R, Arc<WorldChainSpec>>:
        BlockExecutor<Evm = E, Transaction = OpTransactionSigned, Receipt = OpReceipt>,
{
    type Primitives = N;
    type Executor = OpBlockExecutor<E, R, Arc<WorldChainSpec>>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.executor.apply_pre_execution_changes()?;
        // prepare the transaction index for the next transaction 1
        self.prepare_database()
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutorTx<Self::Executor>,
        f: impl FnOnce(&<Self::Executor as BlockExecutor>::Result) -> CommitChanges,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let (tx_env, recovered) = tx.into_parts();
        if !recovered.is_deposit() {
            record_op_l1_block_bal_reads(self.executor.evm_mut().db_mut())?;
        }
        if let Some(gas_used) = self
            .executor
            .execute_transaction_with_commit_condition((tx_env, &recovered), f)?
        {
            self.transactions.push(recovered);
            // only prepare the database index for the next transaction if this one was committed
            self.prepare_database()?;
            Ok(Some(gas_used))
        } else {
            Ok(None)
        }
    }

    fn finish(
        self,
        _state: impl StateProvider,
        _state_root_precomputed: Option<(B256, TrieUpdates)>,
    ) -> Result<BlockBuilderOutcome<N>, BlockExecutionError> {
        unimplemented!(
            "finish is not supported on FlashblocksBlockBuilder; use finish_with_bundle instead"
        )
    }

    fn executor_mut(&mut self) -> &mut Self::Executor {
        &mut self.executor
    }

    fn executor(&self) -> &Self::Executor {
        &self.executor
    }

    fn into_executor(self) -> Self::Executor {
        self.executor
    }
}

impl<'a, DB, R, N, E> BlockBuilderExt for BalBlockBuilder<'a, R, N, E>
where
    DB: Database + 'a,
    N: NodePrimitives<
            Receipt = OpReceipt,
            SignedTx = OpTransactionSigned,
            Block = Block<OpTransactionSigned>,
            BlockHeader = Header,
        >,
    E: Evm<
            DB = &'a mut State<DB>,
            Tx = OpTx,
            Spec = OpSpecId,
            HaltReason = OpHaltReason,
            BlockEnv = BlockEnv,
        >,
    E: PostExecEvm,
    R: OpReceiptBuilder<Receipt = OpReceipt, Transaction = OpTransactionSigned>,
    OpBlockExecutor<E, R, Arc<WorldChainSpec>>:
        BlockExecutor<Evm = E, Transaction = OpTransactionSigned, Receipt = OpReceipt>,
{
    fn finish_with_bundle(
        self,
        state: impl StateProvider,
        mut metrics: impl FlashblockExecutionMetrics,
    ) -> Result<(BlockBuilderOutcome<Self::Primitives>, BundleState), BlockExecutionError> {
        let (evm, result) = self.executor.finish()?;
        let (db, evm_env) = evm.finish();

        // merge all transitions into bundle state
        let merge_started = Instant::now();
        db.merge_transitions(BundleRetention::Reverts);
        metrics.record_stage_duration(PayloadBuildStage::MergeTransitions, merge_started.elapsed());

        // Flatten reverts into a single transition:
        // - per account: keep earliest `previous_status`
        // - per account: keep earliest non-`DoNothing` account-info revert
        // - per account+slot: keep earliest revert-to value
        // - per account: OR `wipe_storage`
        //
        // This keeps `bundle_state.reverts.len() == 1`, which matches the expectation that this
        // bundle represents a single block worth of changes even if we built multiple payloads.
        let bundle =
            crate::utils::extend_flashblock_bundle(&self.committed_bundle, db.take_bundle());

        // calculate the state root
        let state_root_started = Instant::now();
        let hashed_state = state.hashed_post_state(&bundle);
        let (state_root, trie_updates) = state
            .state_root_with_updates(hashed_state.clone())
            .map_err(BlockExecutionError::other)?;
        metrics.record_stage_duration(PayloadBuildStage::StateRoot, state_root_started.elapsed());

        let (transactions, senders) = self
            .transactions
            .into_iter()
            .map(|tx| tx.into_parts())
            .unzip();

        let block_assembly_started = Instant::now();
        let block = self.assembler.assemble_block(BlockAssemblerInput::<
            '_,
            '_,
            OpBlockExecutorFactory<OpRethReceiptBuilder, WorldChainSpec>,
        >::new(
            evm_env,
            self.ctx,
            self.parent,
            transactions,
            &result,
            &bundle,
            &state,
            state_root,
            None,
        ))?;
        metrics.record_stage_duration(
            PayloadBuildStage::BlockAssembly,
            block_assembly_started.elapsed(),
        );

        let block = RecoveredBlock::new_unhashed(block, senders);

        let block_access_list = db
            .take_built_alloy_bal()
            .ok_or_else(|| InternalBlockExecutionError::msg("missing BAL builder state"))?;
        let access_list = FlashblockAccessList::from_block_access_list(
            block_access_list.clone(),
            self.counter.finish(),
        );

        self.access_list_sender
            .send(access_list)
            .map_err(BlockExecutionError::other)?;

        Ok((
            BlockBuilderOutcome {
                execution_result: result,
                hashed_state,
                trie_updates,
                block,
                block_access_list: Some(block_access_list),
            },
            bundle,
        ))
    }
}

/// Simple counter to track sub pre-confirmation block access index bounds.
pub struct BlockAccessIndexCounter {
    current_index: u64,
    start_index: u64,
}

impl BlockAccessIndexCounter {
    pub fn new(start_index: u64) -> Self {
        Self {
            current_index: start_index,
            start_index,
        }
    }

    pub fn inc(&mut self) -> u64 {
        self.current_index += 1;
        self.current_index
    }

    pub fn finish(self) -> (u64, u64) {
        (self.start_index, self.current_index)
    }
}

/// [`CommittedState`] holds all relevant information about the intra block state committment
/// for which we are executing on top of.
#[derive(Default, Debug, Clone)]
pub struct CommittedState<R: OpReceiptBuilder + Default = OpRethReceiptBuilder> {
    /// True when there is no prior committed payload (i.e. this is the first flashblock).
    pub is_first: bool,
    /// The total canonical gas used in previous committed transactions.
    pub gas_used: u64,
    /// The total pre-refund EVM gas used in previous committed transactions.
    pub evm_gas_used: u64,
    /// The total DA footprint in previous committed transactions.
    ///
    /// Post-Jovian this is stored in the block header's `blob_gas_used` field.
    pub blob_gas_used: u64,
    /// The total fees accumulated in previous committed transactions.
    pub fees: U256,
    /// The bundle state accumulated so far from the State Transitions
    pub bundle: BundleState,
    /// Ordered receipts of previous committed transactions.
    pub receipts: Vec<(u64, R::Receipt)>,
    /// Ordered transactions which have been executed
    pub transactions: Vec<(u64, Recovered<R::Transaction>)>,
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
            let blob_gas_used = executed_block
                .recovered_block
                .blob_gas_used()
                .unwrap_or_default();

            let bundle = value
                .executed_block()
                .ok_or(BalExecutorError::MissingExecutedBlock)?
                .execution_output
                .state
                .clone();

            let fees = value.fees();

            let transactions: Vec<_> = executed_block
                .recovered_block
                .clone_transactions_recovered()
                .enumerate()
                .map(|(index, tx)| (index as u64, tx))
                .collect();
            let evm_gas_used = pre_refund_gas_used(gas_used, transactions.iter().map(|(_, tx)| tx));

            let receipts: Vec<_> = executed_block
                .execution_output
                .result
                .receipts
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, r)| (index as u64, r))
                .collect();

            Ok(Self {
                is_first: false,
                transactions,
                receipts,
                gas_used,
                evm_gas_used,
                blob_gas_used,
                fees,
                bundle,
            })
        } else {
            Ok(Self {
                is_first: true,
                transactions: vec![],
                receipts: vec![],
                gas_used: 0,
                evm_gas_used: 0,
                blob_gas_used: 0,
                fees: U256::ZERO,
                bundle: BundleState::default(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BlockAccessIndexCounter, pre_refund_gas_used};
    use alloy_consensus::Sealable;
    use alloy_primitives::Address;
    use op_alloy_consensus::{SDMGasEntry, build_post_exec_tx};
    use reth_optimism_primitives::OpTransactionSigned;
    use reth_primitives_traits::Recovered;

    #[test]
    fn test_block_access_index_counter_finish() {
        let mut counter = BlockAccessIndexCounter::new(5);

        counter.inc();
        assert_eq!(counter.current_index, 6);
        counter.inc();
        assert_eq!(counter.current_index, 7);
        counter.inc();
        assert_eq!(counter.current_index, 8);

        let (start, end) = counter.finish();
        assert_eq!(start, 5);
        assert_eq!(end, 8);
    }

    #[test]
    fn pre_refund_gas_used_adds_post_exec_refunds() {
        let post_exec = OpTransactionSigned::from(
            build_post_exec_tx(
                42,
                vec![
                    SDMGasEntry {
                        index: 0,
                        gas_refund: 7,
                    },
                    SDMGasEntry {
                        index: 1,
                        gas_refund: 11,
                    },
                ],
            )
            .seal_slow(),
        );
        let transactions = [Recovered::new_unchecked(post_exec, Address::ZERO)];

        assert_eq!(pre_refund_gas_used(100, transactions.iter()), 118);
    }
}
