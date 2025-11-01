use std::{borrow::Cow, collections::HashMap};

use alloy_consensus::{Header, Transaction, TxReceipt};
use alloy_eips::{eip2935::HISTORY_STORAGE_ADDRESS, eip4788::BEACON_ROOTS_ADDRESS, Encodable2718};
use alloy_op_evm::{
    block::receipt_builder::OpReceiptBuilder, OpBlockExecutionCtx, OpBlockExecutor,
};
use alloy_primitives::{keccak256, Address, Bytes};
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
    state::{Account, AccountInfo, Bytecode, EvmState},
    DatabaseCommit,
};

use crate::{
    access_list::{AccountChangesConstruction, BlockAccessIndex, FlashblockAccessListConstruction},
    executor::utils::{
        transact_beacon_root_contract_call, transact_blockhashes_contract_call,
        CREATE_2_DEPLOYER_ADDR, CREATE_2_DEPLOYER_BYTECODE, CREATE_2_DEPLOYER_CODEHASH,
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
    min_tx_index: u64,
    max_tx_index: u64,
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
            flashblock_access_list: FlashblockAccessListConstruction::default(),
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

    /// Returns a reference to [`FlashblockAccessListConstruction`].
    pub fn access_list(&self) -> &FlashblockAccessListConstruction {
        &self.flashblock_access_list
    }

    /// Takes ownership of the [`FlashblockAccessListConstruction`]
    pub fn take_access_list(&mut self) -> FlashblockAccessListConstruction {
        core::mem::take(&mut self.flashblock_access_list)
    }

    /// Commits state at a given [`BlockAccessIndex`] to the BAL.
    ///
    /// State should be cleared between indices to ensure that the [`EvmState`] passed here corresponds to only [`Account`] changes
    /// that have occured in the transaction at [`BlockAccessIndex`].
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
                .map_account_change(*address, |account_changes| {
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
            FlashblockAccessListData,
            u64,
            u64,
        ),
        BlockExecutionError,
    > {
        let Self {
            flashblock_access_list: access_list,
            max_tx_index,
            min_tx_index,
            mut inner,
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
                    .and_then(|acc| acc.map(|a| a))
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
                            inner.receipts.len() as BlockAccessIndex,
                            account,
                            &initial_accounts.get(address).unwrap().clone(),
                            account_changes,
                        )
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

        let access_list = access_list.build(min_tx_index, max_tx_index);

        let data = FlashblockAccessListData {
            access_list_hash: keccak256(alloy_rlp::encode(&access_list)),
            access_list,
        };

        let result = BlockExecutionResult {
            receipts: inner.receipts,
            requests: Default::default(),
            gas_used,
        };

        Ok((inner.evm, result, data, min_tx_index, max_tx_index))
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
        if account.info.nonce != initial_account.nonce {
            account_changes
                .nonce_changes
                .insert(index, account.info.nonce);
        }

        // Code Changes
        let final_code = account.info.code.as_ref().map(|b| b.to_owned());
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

    /// The Canyon hardfork issues an irregular state transition that force-deploys the create2
    /// deployer contract. This is done by directly setting the code of the create2 deployer account
    /// prior to executing any transactions on the timestamp activation of the fork.
    pub(crate) fn ensure_create2_deployer(
        chain_spec: impl OpHardforks,
        timestamp: u64,
        db: &mut State<DB>,
    ) -> Result<Option<(AccountInfo, Account)>, DB::Error> {
        // If the canyon hardfork is active at the current timestamp, and it was not active at the
        // previous block timestamp (heuristically, block time is not perfectly constant at 2s), and the
        // chain is an optimism chain, then we need to force-deploy the create2 deployer contract.
        if chain_spec.is_canyon_active_at_timestamp(timestamp)
            && !chain_spec.is_canyon_active_at_timestamp(timestamp.saturating_sub(2))
        {
            // Load the create2 deployer account from the cache.
            let acc = db.load_cache_account(CREATE_2_DEPLOYER_ADDR)?;

            // Update the account info with the create2 deployer codehash and bytecode.
            let mut acc_info = acc.account_info().unwrap_or_default();
            let initial_account = acc_info.clone();

            acc_info.code_hash = CREATE_2_DEPLOYER_CODEHASH;
            acc_info.code = Some(Bytecode::new_raw(Bytes::from_static(
                &CREATE_2_DEPLOYER_BYTECODE,
            )));

            // Convert the cache account back into a revm account and mark it as touched.
            let mut revm_acc: revm::state::Account = acc_info.into();
            revm_acc.mark_touch();

            // Commit the create2 deployer account to the database.
            db.commit(HashMap::from_iter([(
                CREATE_2_DEPLOYER_ADDR,
                revm_acc.clone(),
            )]));

            return Ok(Some((initial_account, revm_acc)));
        }

        Ok(None)
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

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
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
            .and_then(|acc| acc.map(|a| a))
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
                        0,
                        &history_storage_account,
                        &history_storage_info,
                        account_changes,
                    )
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
            .and_then(|acc| acc.map(|a| a))
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
                        0,
                        &parent_beacon_block_root_account,
                        &initial_parent_beacon_block_root_info,
                        account_changes,
                    )
                },
            );

            self.inner.evm.db_mut().commit(state);
        }

        // Ensure that the create2deployer is force-deployed at the canyon transition. Optimism
        // blocks will always have at least a single transaction in them (the L1 info transaction),
        // so we can safely assume that this will always be triggered upon the transition and that
        // the above check for empty blocks will never be hit on OP chains.
        let result = Self::ensure_create2_deployer(
            &self.inner.spec,
            self.inner.evm.block().timestamp.saturating_to(),
            self.inner.evm.db_mut(),
        )
        .map_err(BlockExecutionError::other)?;

        if let Some((initial_account, account)) = result {
            self.flashblock_access_list.map_account_change(
                CREATE_2_DEPLOYER_ADDR,
                |account_changes| {
                    Self::modify_account_changes(0, &account, &initial_account, account_changes)
                },
            );
        }
        Ok(())
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
        let index = self.inner.receipts.len() as BlockAccessIndex;
        self.with_state(&output.state, index)?;
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
