use alloy_consensus::{Transaction, TxReceipt};
use alloy_eips::Encodable2718;
use alloy_op_evm::block::receipt_builder::OpReceiptBuilder;
use alloy_op_evm::{OpBlockExecutionCtx, OpBlockExecutor};
use dashmap::DashMap;
use reth::revm::State;
use reth_evm::op_revm::OpTransaction;
use reth_evm::Evm;
use reth_evm::{
    block::{BlockExecutionError, BlockExecutor, CommitChanges, ExecutableTx},
    Database, FromRecoveredTx, FromTxWithEncoded, OnStateHook,
};
use reth_optimism_forks::OpHardforks;
use reth_provider::BlockExecutionResult;
use revm::context::result::{ExecutionResult, ResultAndState};
use revm::context::TxEnv;
use revm::database::BundleState;
use revm::state::{Account, AccountInfo, EvmState};

use crate::access_list::{
    AccountChangesConstruction, BlockAccessIndex, FlashblockAccessListConstruction, GenesisAlloc,
};

/// A Block Executor for Optimism that can load pre state from previous flashblocks
///
/// A Block Access List is constucted during execution
pub struct BalBuilderBlockExecutor<'a, Evm, R, Spec>
where
    R: OpReceiptBuilder,
{
    inner: OpBlockExecutor<Evm, R, Spec>,
    flashblock_access_list: FlashblockAccessListConstruction<'a>,
    min_tx_index: u64,
    max_tx_index: u64,
}

impl<'a, 'db, DB, E, R, Spec> BalBuilderBlockExecutor<'a, E, R, Spec>
where
    DB: Database + 'db,
    E: Evm<DB = &'db mut State<DB>, Tx = OpTransaction<TxEnv>>,
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt>,
    Spec: OpHardforks + Clone,
    OpTransaction<TxEnv>: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
{
    /// Creates a new [`FlashblocksBlockExecutor`].
    pub fn new(
        evm: E,
        ctx: OpBlockExecutionCtx,
        spec: Spec,
        receipt_builder: R,
        min_tx_index: u64,
        genesis_alloc: GenesisAlloc<'a>,
    ) -> Self {
        let executor = OpBlockExecutor::new(evm, ctx, spec, receipt_builder);

        Self {
            inner: executor,
            flashblock_access_list: FlashblockAccessListConstruction {
                changes: DashMap::new(),
                genesis_alloc,
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
    pub fn access_list(&self) -> &FlashblockAccessListConstruction<'_> {
        &self.flashblock_access_list
    }

    pub fn modify_account_changes(
        index: BlockAccessIndex,
        account: &Account,
        initial_account: &AccountInfo,
        account_changes: &mut AccountChangesConstruction,
    ) {
        // Storage writes
        for (slot, value) in account.changed_storage_slots() {
            // Check if the present value differs from the original value
            if value.present_value() != value.original_value() {
                let slot_value = account_changes.storage_changes.entry(*slot).or_default();
                slot_value
                    .entry(index)
                    .and_modify(|v| *v = value.present_value())
                    .or_insert(value.present_value());
            }
        }

        // Storage Reads
        for (slot, _) in account.storage.iter() {
            if account_changes.storage_changes.contains_key(slot) {
                continue; // Already recorded in writes
            } else {
                account_changes.storage_reads.insert(*slot);
            }
        }

        // Balance Changes
        let final_balance = account.info.balance;
        if final_balance != initial_account.balance {
            account_changes
                .balance_changes
                .entry(index)
                .and_modify(|v| *v = final_balance)
                .or_insert(final_balance);
        }

        // Nonce Changes
        let final_nonce = account.info.nonce;
        if final_nonce != initial_account.nonce {
            account_changes
                .nonce_changes
                .entry(index)
                .and_modify(|v| *v = final_nonce)
                .or_insert(final_nonce);
        }

        // Code Changes
        let final_code = account.info.code.as_ref().map(|b| b.clone());
        if let Some(final_code) = final_code {
            if initial_account.code_hash != account.info.code_hash {
                account_changes.code_changes.insert(index, final_code);
            }
        }

        // Handle Self Destructs explicitly
        if account.is_selfdestructed() || account.is_selfdestructed_locally() {
            // Clear nonce changes
            account_changes.nonce_changes.remove(&index);

            // Retain Balance changes since selfdestruct will update account balance

            // Wipe storage writes - Setting storage to ZERO will be handled as a read.
            // Aggregate all storage slots written to and mark them as read.
            for (slot, _) in account.changed_storage_slots() {
                account_changes.storage_changes.remove(slot);
                account_changes.storage_reads.insert(*slot);
            }
        }
    }

    /// Commits a single transaction execution to the BAL at a given transaction index.
    pub fn with_state(
        &mut self,
        state: &EvmState,
        index: BlockAccessIndex,
    ) -> Result<(), BlockExecutionError> {
        // Update target account if it exists
        for (address, account) in state.iter() {
            let initial_account = self
                .evm_mut()
                .db_mut()
                .database
                .basic(*address)
                .ok()
                .and_then(|acc| acc.map(|a| a))
                .unwrap_or_default();

            self.flashblock_access_list
                .with_account_change(*address, |account_changes| {
                    Self::modify_account_changes(index, account, &initial_account, account_changes)
                });
        }

        Ok(())
    }

    #[expect(clippy::type_complexity)]
    pub fn finish_with_access_list(
        self,
    ) -> Result<
        (
            E,
            BlockExecutionResult<R::Receipt>,
            FlashblockAccessListConstruction<'a>,
            u64,
            u64,
        ),
        BlockExecutionError,
    > {
        let (min_tx_index, max_tx_index) = (self.min_tx_index, self.max_tx_index);
        let (evm, result) = self.inner.finish()?;

        Ok((evm, result, access_list, min_tx_index, max_tx_index))
    }
}

impl<'db, DB, E, R, Spec> BlockExecutor for BalBuilderBlockExecutor<'_, E, R, Spec>
where
    DB: Database + 'db,
    E: Evm<DB = &'db mut State<DB>, Tx = OpTransaction<TxEnv>>,
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt>,
    Spec: OpHardforks + Clone,
    OpTransaction<TxEnv>: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Evm = E;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        let res = self.inner.apply_pre_execution_changes();
        res
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        f: impl FnOnce(&ExecutionResult<<Self::Evm as Evm>::HaltReason>) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        let res = self.inner.execute_transaction_with_commit_condition(tx, f);
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
        let index = self.inner.receipts.len() as BlockAccessIndex;
        let state = &output.state;
        self.with_state(state, index)?;
        self.inner.commit_transaction(output, tx)
    }

    fn finish(self) -> Result<(Self::Evm, BlockExecutionResult<R::Receipt>), BlockExecutionError> {
        self.inner.finish()
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
