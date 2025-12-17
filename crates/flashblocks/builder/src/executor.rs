use alloy_op_evm::{
    OpBlockExecutionCtx, OpBlockExecutor, OpBlockExecutorFactory,
    block::{OpTxEnv, receipt_builder::OpReceiptBuilder},
};

use reth_evm::{
    Database, Evm, FromRecoveredTx, FromTxWithEncoded,
    block::{BlockExecutionError, BlockExecutor, BlockExecutorFactory, CommitChanges, StateDB},
    execute::{
        BasicBlockBuilder, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome, ExecutorTx,
    },
    op_revm::{OpHaltReason, OpSpecId},
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::OpBlockAssembler;
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use reth_primitives::{NodePrimitives, Recovered, RecoveredBlock, SealedHeader};
use reth_provider::StateProvider;
use revm::{
    DatabaseCommit,
    context::{BlockEnv, result::ExecutionResult},
    database::states::{
        bundle_state::BundleRetention,
        reverts::{AccountInfoRevert, Reverts},
    },
};
use std::{
    borrow::Cow,
    collections::{HashMap, hash_map::Entry},
    sync::Arc,
};

/// A wrapper around the [`BasicBlockBuilder`] for flashblocks.
pub struct FlashblocksBlockBuilder<'a, N: NodePrimitives, Evm, R: OpReceiptBuilder> {
    pub inner: BasicBlockBuilder<
        'a,
        OpBlockExecutorFactory,
        OpBlockExecutor<Evm, R, Arc<OpChainSpec>>,
        OpBlockAssembler<OpChainSpec>,
        N,
    >,
}

impl<'a, N: NodePrimitives, Evm, R: OpReceiptBuilder> FlashblocksBlockBuilder<'a, N, Evm, R> {
    /// Creates a new [`FlashblocksBlockBuilder`] with the given executor factory and assembler.
    pub fn new(
        ctx: OpBlockExecutionCtx,
        parent: &'a SealedHeader<N::BlockHeader>,
        executor: OpBlockExecutor<Evm, R, Arc<OpChainSpec>>,
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

impl<'a, DB, N, E, R> BlockBuilder for FlashblocksBlockBuilder<'a, N, E, R>
where
    R: OpReceiptBuilder<Receipt = N::Receipt, Transaction = N::SignedTx>,
    DB: StateDB + DatabaseCommit + Database + 'a,
    N: NodePrimitives<
            Receipt = OpReceipt,
            SignedTx = OpTransactionSigned,
            Block = alloy_consensus::Block<OpTransactionSigned>,
            BlockHeader = alloy_consensus::Header,
        >,
    E: Evm<
            DB = DB,
            Tx: FromRecoveredTx<OpTransactionSigned>
                    + FromTxWithEncoded<OpTransactionSigned>
                    + OpTxEnv,
            Spec = OpSpecId,
            HaltReason = OpHaltReason,
            BlockEnv = BlockEnv,
        >,
    OpBlockExecutorFactory: BlockExecutorFactory<Receipt = N::Receipt, Transaction = N::SignedTx>,
{
    type Primitives = N;
    type Executor = OpBlockExecutor<E, R, Arc<OpChainSpec>>;

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

        // Flatten reverts into a single transition:
        // - per account: keep earliest `previous_status`
        // - per account: keep earliest non-`DoNothing` account-info revert
        // - per account+slot: keep earliest revert-to value
        // - per account: OR `wipe_storage`
        //
        // This keeps `bundle_state.reverts.len() == 1`, which matches the expectation that this
        // bundle represents a single block worth of changes even if we built multiple payloads.
        let flattened = flatten_reverts(&db.bundle_state().reverts);
        db.bundle_state_mut().reverts = flattened;

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
            OpBlockExecutorFactory,
            _,
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

/// Flattens a multi-transition [`Reverts`] into a single transition, merging per-account data.
///
/// Merge rules (iterate earliest -> latest):
/// - For each account, keep the **earliest** `previous_status`.
/// - For each account, keep the **earliest non-`DoNothing`** account-info revert.
/// - For each account+slot, keep the **earliest** `RevertToSlot`.
/// - For each account, OR `wipe_storage`.
fn flatten_reverts(reverts: &Reverts) -> Reverts {
    let mut per_account = HashMap::new();

    for (addr, acc_revert) in reverts.iter().flatten() {
        match per_account.entry(*addr) {
            Entry::Vacant(v) => {
                v.insert(acc_revert.clone());
            }
            Entry::Occupied(mut o) => {
                let entry = o.get_mut();

                // Always OR wipe_storage (if any transition wiped storage, the block-level revert
                // must reflect it).
                entry.wipe_storage |= acc_revert.wipe_storage;

                // Merge storage: keep earliest revert-to value per slot.
                for (slot, revert_to) in &acc_revert.storage {
                    entry.storage.entry(*slot).or_insert(*revert_to);
                }

                // Merge account-info revert: keep earliest non-DoNothing.
                if matches!(entry.account, AccountInfoRevert::DoNothing)
                    && !matches!(acc_revert.account, AccountInfoRevert::DoNothing)
                {
                    entry.account = acc_revert.account.clone();
                }

                // Keep earliest previous_status: do not overwrite.
            }
        }
    }

    // Transform the map into a vec
    let flattened = per_account.into_iter().collect();
    Reverts::new(vec![flattened])
}

#[cfg(test)]
mod tests {
    use crate::executor::flatten_reverts;
    use alloy_primitives::{Address, U256};
    use revm::{
        database::{
            AccountRevert, AccountStatus, RevertToSlot,
            states::reverts::{AccountInfoRevert, Reverts},
        },
        state::AccountInfo,
    };

    #[bon::builder]
    fn revert(
        #[builder(start_fn)] status: AccountStatus,
        account: Option<AccountInfoRevert>,
        #[builder(into, default)] slots: Vec<(U256, U256)>,
        #[builder(default)] wipe_storage: bool,
    ) -> AccountRevert {
        AccountRevert {
            account: account.unwrap_or(AccountInfoRevert::DoNothing),
            storage: slots
                .iter()
                .map(|(k, v)| (*k, RevertToSlot::Some(*v)))
                .collect(),
            previous_status: status,
            wipe_storage,
        }
    }

    #[test]
    fn test_flatten_reverts_different_storage_slots() {
        let addr = Address::with_last_byte(1);
        let slot_0 = U256::ZERO;
        let slot_1 = U256::from(1);

        // Frame 1: slot_0 reverts to 1
        let first = vec![(
            addr,
            revert(AccountStatus::Loaded)
                .slots([(slot_0, U256::from(1))])
                .call(),
        )];

        // Frame 2: slot_0 reverts to 2 (should be ignored), slot_1 reverts to 1 (new, kept)
        let second = vec![(
            addr,
            revert(AccountStatus::InMemoryChange)
                .slots([(slot_0, U256::from(2)), (slot_1, U256::from(1))])
                .call(),
        )];

        let reverts = Reverts::new(vec![first, second]);
        let flattened = flatten_reverts(&reverts);

        assert_eq!(flattened.len(), 1);
        let (actual_addr, actual_revert) = flattened[0][0].clone();

        assert_eq!(actual_addr, addr);
        // slot_0 keeps first frame's value (1), slot_1 added from second frame
        let expected = revert(AccountStatus::Loaded)
            .slots([(slot_0, U256::from(1)), (slot_1, U256::from(1))])
            .call();
        assert_eq!(actual_revert, expected);
    }

    #[test]
    fn test_flatten_reverts_different_account_info() {
        let addr = Address::with_last_byte(1);
        let prev_acc_info = AccountInfo::default();

        // Frame 1: DoNothing
        let first = vec![(addr, revert(AccountStatus::Loaded).call())];

        // Frame 2: RevertTo (first non-DoNothing, should be kept)
        let second = vec![(
            addr,
            revert(AccountStatus::Loaded)
                .account(AccountInfoRevert::RevertTo(prev_acc_info.clone()))
                .call(),
        )];

        // Frame 3: DeleteIt (should be ignored, already have non-DoNothing)
        let third = vec![(
            addr,
            revert(AccountStatus::Loaded)
                .account(AccountInfoRevert::DeleteIt)
                .call(),
        )];

        let reverts = Reverts::new(vec![first, second, third]);
        let flattened = flatten_reverts(&reverts);

        assert_eq!(flattened.len(), 1);
        let (actual_addr, actual_revert) = flattened[0][0].clone();

        assert_eq!(actual_addr, addr);
        // Should keep RevertTo from frame 2 (first non-DoNothing)
        let expected = revert(AccountStatus::Loaded)
            .account(AccountInfoRevert::RevertTo(prev_acc_info))
            .call();
        assert_eq!(actual_revert, expected);
    }

    #[test]
    fn test_flatten_reverts_wipe_storage() {
        let addr = Address::with_last_byte(1);
        let prev_acc_info = AccountInfo::default();

        // Frame 1: wipe_storage = false (default)
        let first = vec![(addr, revert(AccountStatus::Loaded).call())];

        // Frame 2: wipe_storage = true (should be kept - sticky)
        let second = vec![(
            addr,
            revert(AccountStatus::Loaded)
                .account(AccountInfoRevert::RevertTo(prev_acc_info.clone()))
                .wipe_storage(true)
                .call(),
        )];

        // Frame 3: wipe_storage = false (should be ignored, already true)
        let third = vec![(
            addr,
            revert(AccountStatus::Loaded)
                .account(AccountInfoRevert::DeleteIt)
                .call(),
        )];

        let reverts = Reverts::new(vec![first, second, third]);
        let flattened = flatten_reverts(&reverts);

        assert_eq!(flattened.len(), 1);
        let (actual_addr, actual_revert) = flattened[0][0].clone();

        assert_eq!(actual_addr, addr);
        // wipe_storage should remain true (sticky once set)
        let expected = revert(AccountStatus::Loaded)
            .account(AccountInfoRevert::RevertTo(prev_acc_info))
            .wipe_storage(true)
            .call();
        assert_eq!(actual_revert, expected);
    }
}
