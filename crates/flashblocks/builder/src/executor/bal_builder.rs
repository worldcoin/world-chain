use std::{borrow::Cow, collections::HashMap, sync::Arc};

use alloy_consensus::{Header, Transaction, TxReceipt};
use alloy_eips::{eip2935::HISTORY_STORAGE_ADDRESS, eip4788::BEACON_ROOTS_ADDRESS, Encodable2718};
use alloy_op_evm::{
    block::receipt_builder::OpReceiptBuilder, OpBlockExecutionCtx, OpBlockExecutor, OpEvmFactory,
};
use alloy_primitives::{keccak256, Address};
use flashblocks_primitives::access_list::FlashblockAccessListData;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use reth::revm::State;

use reth_chainspec::EthereumHardforks;
use reth_evm::{
    block::{
        BlockExecutionError, BlockExecutor, BlockValidationError, CommitChanges, ExecutableTx,
        StateChangePostBlockSource, StateChangeSource,
    },
    op_revm::{OpSpecId, OpTransaction},
    state_change::{balance_increment_state, post_block_balance_increments},
    Database, Evm, EvmEnv, EvmFactory, FromRecoveredTx, FromTxWithEncoded, OnStateHook,
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use reth_provider::BlockExecutionResult;
use revm::{
    context::{
        result::{ExecResultAndState, ExecutionResult, ResultAndState},
        Block, BlockEnv, TxEnv,
    },
    database::{BundleState, TransitionAccount},
    state::{Account, AccountInfo, EvmState},
    DatabaseCommit, DatabaseRef,
};

use crate::{
    access_list::{AccountChangesConstruction, BlockAccessIndex, FlashblockAccessListConstruction},
    executor::{
        bal_executor::BalExecutionState,
        temporal_db::TemporalDbFactory,
        utils::{
            ensure_create2_deployer, transact_beacon_root_contract_call,
            transact_blockhashes_contract_call, CREATE_2_DEPLOYER_ADDR,
        },
        BalExecutorError, BalValidationError,
    },
};

/// A Block Executor for Optimism that can load pre state from previous flashblocks
///
/// A Block Access List is constucted during execution
pub struct BalBuilderBlockExecutor<Evm, R>
where
    R: OpReceiptBuilder,
{
    inner: OpBlockExecutor<Evm, R, Arc<OpChainSpec>>,
    flashblock_access_list: FlashblockAccessListConstruction,
    block_access_index: Option<BlockAccessIndex>,
    min_tx_index: BlockAccessIndex,
}

impl<'db, DB, E, R> Clone for BalBuilderBlockExecutor<E, R>
where
    DB: Database + 'db,
    E: Evm<
            DB = &'db mut State<DB>,
            Tx = OpTransaction<TxEnv>,
            Spec = OpSpecId,
            BlockEnv = BlockEnv,
        > + Clone,
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718> + Clone,
    OpTransaction<TxEnv>: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
{
    fn clone(&self) -> Self {
        let new_executor = OpBlockExecutor::new(
            self.inner.evm.clone(),
            self.inner.ctx.clone(),
            self.inner.spec.clone(),
            self.inner.receipt_builder.clone(),
        );

        Self {
            inner: new_executor,
            flashblock_access_list: self.flashblock_access_list.clone(),
            block_access_index: self.block_access_index,
            min_tx_index: self.min_tx_index,
        }
    }
}

impl<'db, DB, E, R> BalBuilderBlockExecutor<E, R>
where
    DB: Database + 'db,
    E: Evm<
        DB = &'db mut State<DB>,
        Tx = OpTransaction<TxEnv>,
        Spec = OpSpecId,
        BlockEnv = BlockEnv,
    >,
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt>,
    OpTransaction<TxEnv>: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
{
    /// Creates a new [`BalBuilderBlockExecutor`].
    pub fn new(
        evm: E,
        ctx: OpBlockExecutionCtx,
        spec: Arc<OpChainSpec>,
        receipt_builder: R,
    ) -> Self {
        let executor = OpBlockExecutor::new(evm, ctx, spec, receipt_builder);

        Self {
            inner: executor,
            flashblock_access_list: FlashblockAccessListConstruction::default(),
            min_tx_index: 0,
            block_access_index: None,
        }
    }

    /// Sets the minimum transaction index for the constructed BAL
    pub fn with_min_tx_index(mut self, min_tx_index: BlockAccessIndex) -> Self {
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

    fn inc_transactions(&mut self) {
        if let Some(index) = &mut self.block_access_index {
            *index += 1;
        }
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

        let access_list = access_list.build(min_tx_index as u64, block_access_index.into());

        let data = FlashblockAccessListData {
            access_list_hash: keccak256(alloy_rlp::encode(&access_list)),
            access_list,
        };
        let legacy_gas_used = inner
            .receipts
            .last()
            .map(|r| r.cumulative_gas_used())
            .unwrap_or_default();

        let (e, res) = (
            inner.evm,
            BlockExecutionResult {
                receipts: inner.receipts,
                requests: Default::default(),
                gas_used: legacy_gas_used,
                blob_gas_used: inner.da_footprint_used,
            },
        );

        Ok((e, res, data, min_tx_index.into(), block_access_index.into()))
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

    pub fn execute_block_parallel(
        mut self,
        execution_state: Arc<&BalExecutionState<R>>,
        expected_access_list: FlashblockAccessListData,
    ) -> Result<
        (
            BundleState,
            BlockExecutionResult<R::Receipt>,
            EvmEnv<OpSpecId>,
            OpBlockExecutionCtx,
            u128,
        ),
        BalExecutorError,
    >
    where
        DB: DatabaseRef<Error: Send + Sync + 'static> + Clone + Send + Sync + 'db,
        R: Default + Send + Sync + Clone + 'static,
    {
        if self.inner.receipts.is_empty() {
            self.apply_pre_execution_changes()?;
        }

        let mut present_access_list = self.take_access_list();
        let present_gas_used = self.inner.gas_used;
        let (db, env) = self.inner.evm.finish();

        let database = db.database.clone();
        let present_bundle = db.take_bundle();

        let mut present_receipts = core::mem::take(&mut self.inner.receipts);
        let temporal_db_factory =
            TemporalDbFactory::new(&database, expected_access_list.access_list.clone());

        let context = self.inner.ctx.clone();
        let receipt_builder = self.inner.receipt_builder.clone();

        #[derive(Default)]
        struct ParallelExecutionResult {
            transitions: revm::primitives::map::HashMap<Address, TransitionAccount>,
            receipts: Vec<OpReceipt>,
            gas_used: u64,
            fees: u128,
            access_list: FlashblockAccessListConstruction,
            index: BlockAccessIndex,
        }

        let execution_state_arc = Arc::new(execution_state);
        let execution_transactions = execution_state_arc.executor_transactions.clone();

        let base_fee = env.block_env().basefee;

        let mut results: Vec<_> = execution_transactions
            .into_par_iter()
            .map(|(index, tx)| {
                let temporal_db = temporal_db_factory.db(index as u64);

                let mut state = State::builder()
                    .with_database_ref(temporal_db)
                    .with_bundle_prestate(present_bundle.clone())
                    .with_bundle_update()
                    .build();

                let evm = OpEvmFactory::default().create_evm(&mut state, env.clone());

                let mut executor = execution_state_arc.executor_at_index(
                    self.inner.spec.clone(),
                    receipt_builder.clone(),
                    evm,
                    index,
                    FlashblockAccessListConstruction::default(),
                );

                let gas_used = executor
                    .execute_transaction_with_commit_condition(tx.clone(), |_| {
                        CommitChanges::Yes
                    })?;

                let fees = if !tx.is_deposit() {
                    tx.effective_tip_per_gas(base_fee)
                        .expect("fee is always valid; execution succeeded")
                } else {
                    0
                };
                let access_list = executor.take_access_list();

                let (db, _) = executor.inner.evm.finish();

                let transitions = db.transition_state.take();
                let receipt = executor.inner.receipts.into_iter().last().unwrap();

                Ok(ParallelExecutionResult {
                    transitions: transitions.map(|t| t.transitions).unwrap_or_default(),
                    receipts: vec![receipt],
                    gas_used: gas_used.unwrap_or_default(),
                    fees,
                    access_list,
                    index,
                })
            })
            .collect::<Result<Vec<_>, BlockExecutionError>>()?;

        results.sort_unstable_by_key(|r| r.index);

        // Merge results
        let merged_result = results.into_iter().fold(
            ParallelExecutionResult {
                gas_used: present_gas_used,
                ..Default::default()
            },
            |mut acc: ParallelExecutionResult, mut res| {
                acc.transitions.extend(res.transitions);
                acc.gas_used += res.gas_used;
                let mut receipt = res.receipts.pop().expect("safe expect");
                receipt.as_receipt_mut().cumulative_gas_used = acc.gas_used;
                acc.receipts.push(receipt);
                acc.access_list.merge(res.access_list);
                acc.fees += res.fees;
                acc.index = res.index;
                acc
            },
        );

        // Apply merged transitions to the main executor
        db.apply_transition(merged_result.transitions.into_iter().collect());
        let ref_db = temporal_db_factory.db(merged_result.index as u64 + 1);

        let mut state = State::builder()
            .with_database_ref(ref_db)
            .with_bundle_prestate(present_bundle)
            .with_bundle_update()
            .build();

        state.apply_transition(
            db.transition_state
                .take()
                .map(|t| t.transitions.into_iter().collect())
                .unwrap_or_default(),
        );

        present_receipts.extend_from_slice(&merged_result.receipts);
        present_access_list.merge(merged_result.access_list);

        let executor = BalBuilderBlockExecutor::new(
            OpEvmFactory::default().create_evm(&mut state, env),
            self.inner.ctx,
            self.inner.spec,
            self.inner.receipt_builder,
        )
        .with_access_list(present_access_list)
        .with_receipts(present_receipts)
        .with_block_access_index(merged_result.index + 1)
        .with_gas_used(merged_result.gas_used);

        let (evm, result, access_list, _min_tx_index, _max_tx_index) =
            executor.finish_with_access_list()?;

        if access_list.access_list_hash != expected_access_list.access_list_hash {
            std::fs::write(
                "computed_access_list.json",
                serde_json::to_string_pretty(&access_list.access_list).unwrap(),
            )
            .unwrap();

            std::fs::write(
                "expected_access_list.json",
                serde_json::to_string_pretty(&expected_access_list.access_list).unwrap(),
            )
            .unwrap();

            return Err(BalValidationError::AccessListHashMismatch {
                expected: expected_access_list.access_list_hash,
                got: access_list.access_list_hash,
            }
            .into());
        }

        let (db, env) = evm.finish();

        Ok((db.take_bundle(), result, env, context, merged_result.fees))
    }
}

impl<'db, DB, E, R> BlockExecutor for BalBuilderBlockExecutor<E, R>
where
    DB: Database + 'db,
    E: Evm<
        DB = &'db mut State<DB>,
        Tx = OpTransaction<TxEnv>,
        Spec = OpSpecId,
        BlockEnv = BlockEnv,
    >,
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt>,
    OpTransaction<TxEnv>: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Evm = E;

    // TODO: Clean this up, open a PR into alloy-evm to make these system calls reusable
    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        // Set state clear flag if the block is after the Spurious Dragon hardfork.
        let state_clear_flag = self
            .inner
            .spec
            .is_spurious_dragon_active_at_block(self.inner.evm.block().number().saturating_to());

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
                        0,
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
                        0,
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
            self.inner.evm.block().timestamp().saturating_to(),
            self.inner.evm.db_mut(),
        )
        .map_err(BlockExecutionError::other)?;

        if let Some((initial_account, account)) = result {
            self.flashblock_access_list.map_account_change(
                CREATE_2_DEPLOYER_ADDR,
                |account_changes| {
                    Self::modify_account_changes(0, &account, &initial_account, account_changes);

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
        // Commit state changes to BAL
        self.with_state(&result.state)?;
        self.inc_transactions();

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
        self.inc_transactions();

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
