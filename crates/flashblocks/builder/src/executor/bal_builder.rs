use std::{borrow::Cow, collections::HashMap};

use alloy_consensus::{Header, Transaction, TxReceipt};
use alloy_eips::{eip2935::HISTORY_STORAGE_ADDRESS, eip4788::BEACON_ROOTS_ADDRESS, Encodable2718};
use alloy_op_evm::{
    block::receipt_builder::OpReceiptBuilder, OpBlockExecutionCtx, OpBlockExecutor,
};
use alloy_primitives::{keccak256, Address};
use flashblocks_primitives::access_list::FlashblockAccessListData;
use reth::revm::State;

use reth_evm::{
    block::{
        BlockExecutionError, BlockExecutor, BlockValidationError, CommitChanges, ExecutableTx,
        StateChangePostBlockSource, StateChangeSource,
    },
    op_revm::OpTransaction,
    state_change::{balance_increment_state, post_block_balance_increments},
    Database, Evm, FromRecoveredTx, FromTxWithEncoded, OnStateHook,
};
use reth_optimism_forks::OpHardforks;
use reth_provider::BlockExecutionResult;
use revm::{
    context::{
        result::{ExecResultAndState, ExecutionResult, ResultAndState},
        TxEnv,
    },
    database::BundleState,
    state::{Account, AccountInfo, EvmState},
    DatabaseCommit,
};
use tracing::info;

use crate::{
    access_list::{AccountChangesConstruction, BlockAccessIndex, FlashblockAccessListConstruction},
    executor::utils::{
        ensure_create2_deployer, transact_beacon_root_contract_call,
        transact_blockhashes_contract_call, CREATE_2_DEPLOYER_ADDR,
    },
};

/// A Block Executor for Optimism that can load pre state from previous flashblocks
///
/// A Block Access List is constucted during execution
pub struct BalBuilderBlockExecutor<Evm, R, Spec>
where
    R: OpReceiptBuilder,
{
    inner: OpBlockExecutor<Evm, R, Spec>,
    flashblock_access_list: FlashblockAccessListConstruction,
    block_access_index: Option<BlockAccessIndex>,
    min_tx_index: u64,
}

impl<'db, DB, E, R, Spec> BalBuilderBlockExecutor<E, R, Spec>
where
    DB: Database + 'db,
    E: Evm<DB = &'db mut State<DB>, Tx = OpTransaction<TxEnv>>,
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt>,
    Spec: OpHardforks + Clone,
    OpTransaction<TxEnv>: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
{
    /// Creates a new [`BalBuilderBlockExecutor`].
    pub fn new(evm: E, ctx: OpBlockExecutionCtx, spec: Spec, receipt_builder: R) -> Self {
        let executor = OpBlockExecutor::new(evm, ctx, spec, receipt_builder);

        Self {
            inner: executor,
            flashblock_access_list: FlashblockAccessListConstruction::default(),
            min_tx_index: 0,
            block_access_index: None,
        }
    }

    /// Sets the minimum transaction index for the constructed BAL
    pub fn with_min_tx_index(mut self, min_tx_index: u64) -> Self {
        self.min_tx_index = min_tx_index;
        self
    }

    /// Sets the [`FlashblockAccessListConstruction`] for the executor
    pub fn with_access_list(mut self, access_list: FlashblockAccessListConstruction) -> Self {
        self.flashblock_access_list = access_list;
        self
    }

    /// Hardcode the [`BlockAccessIndex`] for the executor
    pub fn with_block_access_index(mut self, index: BlockAccessIndex) -> Self {
        self.block_access_index = Some(index);
        self
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

    /// Returns a reference to [`FlashblockAccessListConstruction`].
    pub fn access_list(&self) -> &FlashblockAccessListConstruction {
        &self.flashblock_access_list
    }

    /// Takes ownership of the [`FlashblockAccessListConstruction`]
    pub fn take_access_list(&mut self) -> FlashblockAccessListConstruction {
        core::mem::take(&mut self.flashblock_access_list)
    }

    /// Returns the current [`BlockAccessIndex`].
    pub fn block_access_index(&self) -> BlockAccessIndex {
        self.block_access_index
            .unwrap_or(self.inner.receipts.len() as BlockAccessIndex + 1)
    }

    /// Commits state at a given [`BlockAccessIndex`] to the BAL.
    ///
    /// State should be cleared between indices to ensure that the [`EvmState`] passed here corresponds to only [`Account`] changes
    /// that have occured in the transaction at [`BlockAccessIndex`].
    pub fn with_state(&mut self, state: &EvmState) -> Result<(), BlockExecutionError> {
        let index = self.block_access_index();

        // Update target account if it exists
        for (address, account) in state.iter() {
            let initial_account = self
                .evm_mut()
                .db_mut()
                .database
                .basic(*address)
                .ok()
                .and_then(|acc| acc)
                .unwrap_or_default();

            self.flashblock_access_list
                .map_account_change(*address, |account_changes| {
                    Self::modify_account_changes(index, account, &initial_account, account_changes);
                });

            // Remove empty account changes
            if self
                .flashblock_access_list
                .changes
                .get(address)
                .is_some_and(|changes| changes.is_empty())
            {
                self.flashblock_access_list.changes.remove(address);
            }
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
            FlashblockAccessListData,
            u64,
            u64,
        ),
        BlockExecutionError,
    > {
        let block_access_index = self.block_access_index();

        let Self {
            flashblock_access_list: access_list,
            min_tx_index,
            mut inner,
            ..
        } = self;

        let balance_increments =
            post_block_balance_increments::<Header>(&inner.spec, inner.evm.block(), &[], None);

        // Accumulate initial account info for `balance_increment` accounts.
        let initial_accounts: HashMap<Address, AccountInfo> = balance_increments
            .keys()
            .map(|a| {
                let initial_account = inner
                    .evm_mut()
                    .db_mut()
                    .database
                    .basic(*a)
                    .ok()
                    .and_then(|acc| acc)
                    .unwrap_or_default();

                (*a, initial_account)
            })
            .collect();

        // increment balances
        inner
            .evm
            .db_mut()
            .increment_balances(balance_increments.clone())
            .map_err(|_| BlockValidationError::IncrementBalanceFailed)?;

        // call state hook with changes due to balance increments.
        inner.system_caller.try_on_state_with(|| {
            balance_increment_state(&balance_increments, inner.evm.db_mut()).map(|state| {
                for (address, account) in state.iter() {
                    access_list.map_account_change(*address, |account_changes| {
                        Self::modify_account_changes(
                            block_access_index,
                            account,
                            &initial_accounts.get(address).unwrap().clone(),
                            account_changes,
                        );

                        if account_changes.is_empty() {
                            access_list.changes.remove(address);
                        }
                    });
                }

                (
                    StateChangeSource::PostBlock(StateChangePostBlockSource::BalanceIncrements),
                    Cow::Owned(state),
                )
            })
        })?;

        let gas_used = inner
            .receipts
            .last()
            .map(|r| r.cumulative_gas_used())
            .unwrap_or_default();

        let access_list = access_list.build(min_tx_index, block_access_index.into());

        let data = FlashblockAccessListData {
            access_list_hash: keccak256(alloy_rlp::encode(&access_list)),
            access_list,
        };

        let result = BlockExecutionResult {
            receipts: inner.receipts,
            requests: Default::default(),
            gas_used,
        };

        Ok((
            inner.evm,
            result,
            data,
            min_tx_index,
            block_access_index.into(),
        ))
    }

    pub(crate) fn modify_account_changes(
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
                slot_value.insert(index, value.present_value());
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
        if account.info.balance != initial_account.balance {
            account_changes
                .balance_changes
                .insert(index, account.info.balance);
        }

        // Nonce Changes
        account_changes
            .nonce_changes
            .insert(index, account.info.nonce);

        // Code Changes
        let final_code = account.info.code.as_ref().map(|b| b.to_owned());
        if let Some(final_code) = final_code {
            // if initial_account.code_hash != account.info.code_hash {
            account_changes.code_changes.insert(index, final_code);
            // }
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
}

impl<'db, DB, E, R, Spec> BlockExecutor for BalBuilderBlockExecutor<E, R, Spec>
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

    // TODO: Clean this up, open a PR into alloy-evm to make these system calls reusable
    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        let block_access_index = 0;

        // Set state clear flag if the block is after the Spurious Dragon hardfork.
        let state_clear_flag = self
            .inner
            .spec
            .is_spurious_dragon_active_at_block(self.inner.evm.block().number.saturating_to());

        self.inner
            .evm
            .db_mut()
            .set_state_clear_flag(state_clear_flag);

        let history_storage_info = self
            .inner
            .evm
            .db_mut()
            .database
            .basic(HISTORY_STORAGE_ADDRESS)
            .ok()
            .and_then(|acc| acc)
            .unwrap_or_default();

        let result_and_state = transact_blockhashes_contract_call(
            &self.inner.spec,
            self.inner.ctx.parent_hash,
            &mut self.inner.evm,
        )?;

        if let Some(res) = result_and_state {
            let history_storage_account = res
                .state
                .get(&HISTORY_STORAGE_ADDRESS)
                .cloned()
                .unwrap_or_default();

            self.flashblock_access_list.map_account_change(
                HISTORY_STORAGE_ADDRESS,
                |account_changes| {
                    Self::modify_account_changes(
                        block_access_index,
                        &history_storage_account,
                        &history_storage_info,
                        account_changes,
                    );

                    if account_changes.is_empty() {
                        self.flashblock_access_list
                            .changes
                            .remove(&HISTORY_STORAGE_ADDRESS);
                    }
                },
            );

            self.inner.evm.db_mut().commit(res.state);
        }

        let initial_parent_beacon_block_root_info = self
            .inner
            .evm
            .db_mut()
            .database
            .basic(BEACON_ROOTS_ADDRESS)
            .ok()
            .and_then(|acc| acc)
            .unwrap_or_default();

        let result_and_state = transact_beacon_root_contract_call(
            &self.inner.spec,
            self.inner.ctx.parent_beacon_block_root,
            &mut self.inner.evm,
        )?;

        if let Some(ExecResultAndState { state, .. }) = result_and_state {
            let parent_beacon_block_root_account = state
                .get(&BEACON_ROOTS_ADDRESS)
                .cloned()
                .unwrap_or_default();

            self.flashblock_access_list.map_account_change(
                BEACON_ROOTS_ADDRESS,
                |account_changes| {
                    Self::modify_account_changes(
                        block_access_index,
                        &parent_beacon_block_root_account,
                        &initial_parent_beacon_block_root_info,
                        account_changes,
                    );

                    if account_changes.is_empty() {
                        self.flashblock_access_list
                            .changes
                            .remove(&BEACON_ROOTS_ADDRESS);
                    }
                },
            );

            self.inner.evm.db_mut().commit(state);
        }

        // Ensure that the create2deployer is force-deployed at the canyon transition. Optimism
        // blocks will always have at least a single transaction in them (the L1 info transaction),
        // so we can safely assume that this will always be triggered upon the transition and that
        // the above check for empty blocks will never be hit on OP chains.
        let result = ensure_create2_deployer(
            &self.inner.spec,
            self.inner.evm.block().timestamp.saturating_to(),
            self.inner.evm.db_mut(),
        )
        .map_err(BlockExecutionError::other)?;

        if let Some((initial_account, account)) = result {
            self.flashblock_access_list.map_account_change(
                CREATE_2_DEPLOYER_ADDR,
                |account_changes| {
                    Self::modify_account_changes(
                        block_access_index,
                        &account,
                        &initial_account,
                        account_changes,
                    );

                    if account_changes.is_empty() {
                        self.flashblock_access_list
                            .changes
                            .remove(&CREATE_2_DEPLOYER_ADDR);
                    }
                },
            );
        }
        Ok(())
    }

    fn execute_transaction(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<u64, BlockExecutionError> {
        let result = self.execute_transaction_without_commit(&tx)?;
        self.with_state(&result.state)?;
        self.commit_transaction(result, tx)
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<ResultAndState<<Self::Evm as Evm>::HaltReason>, BlockExecutionError> {
        self.inner.execute_transaction_without_commit(&tx)
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        f: impl FnOnce(&ExecutionResult<<Self::Evm as Evm>::HaltReason>) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        // Execute transaction without committing
        let output = self.execute_transaction_without_commit(&tx)?;

        if !f(&output.result).should_commit() {
            return Ok(None);
        }

        // Commit state changes to BAL
        self.with_state(&output.state)?;

        let gas_used = self.commit_transaction(output, tx)?;

        Ok(Some(gas_used))
    }

    fn commit_transaction(
        &mut self,
        output: ResultAndState<<Self::Evm as Evm>::HaltReason>,
        tx: impl ExecutableTx<Self>,
    ) -> Result<u64, BlockExecutionError> {
        self.inner.commit_transaction(output, tx)
    }

    /// Use [`BalBuilderBlockExecutor::finish_with_access_list`] to fetch the access list after
    /// post execution changes.
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
