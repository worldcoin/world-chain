use alloy_consensus::{Transaction, TxReceipt};
use alloy_eips::Encodable2718;
use alloy_op_evm::block::receipt_builder::OpReceiptBuilder;
use alloy_op_evm::{OpBlockExecutionCtx, OpBlockExecutor};
use dashmap::DashMap;
use reth::revm::State;
use reth_evm::Evm;
use reth_evm::{
    block::{BlockExecutionError, BlockExecutor, CommitChanges, ExecutableTx},
    Database, FromRecoveredTx, FromTxWithEncoded, OnStateHook,
};
use reth_optimism_forks::OpHardforks;
use reth_provider::BlockExecutionResult;
use revm::context::result::{ExecutionResult, ResultAndState};
use revm::database::BundleState;
use tracing::info;

use crate::access_list::FlashblockAccessListConstruction;

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
