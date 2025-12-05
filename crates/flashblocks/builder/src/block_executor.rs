//! A block executor and builder for flashblocks that constructs a BAL (Block Access List) sidecar.

use alloy_op_evm::{
    OpBlockExecutionCtx, OpBlockExecutor, OpBlockExecutorFactory, OpEvmFactory,
    block::receipt_builder::OpReceiptBuilder,
};
use op_alloy_consensus::OpReceipt;
use reth_evm::{
    Database, Evm, EvmFactory, FromRecoveredTx, FromTxWithEncoded,
    block::{BlockExecutionError, BlockExecutor, BlockExecutorFactory, BlockExecutorFor, StateDB},
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
    database::{AccountStatus, BundleState},
};
use revm_database_interface::DBErrorMarker;
use tracing::{info, trace};

use crate::{access_list::BlockAccessIndex, executor::bal_builder_db::BalBuilderDb};
use alloy_consensus::{Block, BlockHeader, Header, Transaction, transaction::TxHashRef};
use alloy_primitives::{Address, FixedBytes, U256};
use flashblocks_primitives::access_list::FlashblockAccessList;
use reth::revm::State;
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

#[derive(thiserror::Error, Debug)]
pub enum BalExecutorError {
    #[error("Block execution error: {0}")]
    BlockExecutionError(#[from] BlockExecutionError),
    #[error("Missing executed block in built payload")]
    MissingExecutedBlock,
    #[error("Validation error: {0}")]
    BalValidationError(String),
    #[error("Evm error: {0}")]
    EvmError(#[from] Box<dyn core::error::Error + Send + Sync + 'static>),
}

impl BalExecutorError {
    pub fn evm_err<E: core::error::Error + Send + Sync + 'static>(err: E) -> Self {
        BalExecutorError::EvmError(Box::new(err))
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
        let block_executor = BalBlockExecutor::new(
            evm,
            ctx,
            self.spec().clone().into(),
            OpRethReceiptBuilder::default(),
        );

        block_executor
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
        executor: BalBlockExecutor<E, R>,
        transactions: Vec<Recovered<N::SignedTx>>,
        chain_spec: Arc<OpChainSpec>,
        tx: crossbeam_channel::Sender<FlashblockAccessList>,
    ) -> Self {
        let start_index = if transactions.is_empty() {
            0
        } else {
            transactions.len() as u16 + 1
        };

        let counter = BlockAccessIndexCounter::new(start_index);

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
        let next_index = self.counter.next_index();
        trace!(
            target: "flashblocks::builder::bal_block_builder",
            next_index,
            "Setting up BAL database with next access index"
        );

        let db = self.inner.executor.inner.evm_mut().db_mut();
        db.set_index(next_index);
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
        self.prepare_database()?;
        self.inner.execute_transaction_with_commit_condition(tx, f)
    }

    fn finish(
        mut self,
        state: impl StateProvider,
    ) -> Result<BlockBuilderOutcome<N>, BlockExecutionError> {
        self.prepare_database()?;

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

        let access_list = db.finish()?.build(self.counter.finish());

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

    pub fn next_index(&mut self) -> u16 {
        let index = self.current_index;
        self.current_index += 1;
        index
    }

    pub fn finish(self) -> (u16, u16) {
        (self.start_index, self.current_index)
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

/// Normalizes account statuses in a bundle state for use as a prestate.
///
/// When a bundle is used as a prestate for new execution, accounts with `InMemoryChange`
/// status need to be converted to `Changed` status. This is because:
/// - `InMemoryChange` means the account was created in memory without being loaded from DB
/// - When applying new transitions with `Changed` status, revm expects the account to be
///   in `Loaded`, `Changed`, or `LoadedEmptyEIP161` status
/// - `InMemoryChange -> Changed` is not a valid state transition and will panic
/// - `Changed -> Changed` is a valid state transition
fn normalize_bundle_for_prestate(mut bundle: BundleState) -> BundleState {
    for (_address, account) in bundle.state.iter_mut() {
        if account.status == AccountStatus::InMemoryChange {
            account.status = AccountStatus::Changed;
        }
    }
    bundle
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
            
            // Normalize the bundle state for use as a prestate.
            // This converts InMemoryChange accounts to Changed status to allow
            // valid state transitions when applying new execution results.
            let bundle =
                normalize_bundle_for_prestate(executed_block.execution_output.bundle.clone());

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
    use alloy_primitives::{address, uint};
    use reth::revm::db::states::StorageSlot;
    use revm::{
        database::BundleAccount,
        primitives::KECCAK_EMPTY,
        state::{AccountInfo, Bytecode},
    };

    // Helper to create an account info
    fn create_account(balance: U256, nonce: u64) -> AccountInfo {
        AccountInfo {
            balance,
            nonce,
            code_hash: KECCAK_EMPTY,
            code: Some(Bytecode::default()),
        }
    }

    #[test]
    fn test_normalize_bundle_for_prestate_converts_in_memory_change() {
        let addr = address!("0000000000000000000000000000000000000001");

        let mut bundle = BundleState::default();
        bundle.state.insert(
            addr,
            BundleAccount {
                info: Some(create_account(uint!(1000_U256), 1)),
                original_info: Some(create_account(uint!(500_U256), 0)),
                storage: Default::default(),
                status: AccountStatus::InMemoryChange,
            },
        );

        let normalized = normalize_bundle_for_prestate(bundle);

        let account = normalized.state.get(&addr).unwrap();
        assert_eq!(
            account.status,
            AccountStatus::Changed,
            "InMemoryChange should be converted to Changed"
        );
    }

    #[test]
    fn test_normalize_bundle_for_prestate_preserves_changed() {
        let addr = address!("0000000000000000000000000000000000000002");

        let mut bundle = BundleState::default();
        bundle.state.insert(
            addr,
            BundleAccount {
                info: Some(create_account(uint!(2000_U256), 5)),
                original_info: Some(create_account(uint!(1000_U256), 4)),
                storage: Default::default(),
                status: AccountStatus::Changed,
            },
        );

        let normalized = normalize_bundle_for_prestate(bundle);

        let account = normalized.state.get(&addr).unwrap();
        assert_eq!(
            account.status,
            AccountStatus::Changed,
            "Changed status should be preserved"
        );
    }

    #[test]
    fn test_normalize_bundle_for_prestate_preserves_loaded() {
        let addr = address!("0000000000000000000000000000000000000003");

        let mut bundle = BundleState::default();
        bundle.state.insert(
            addr,
            BundleAccount {
                info: Some(create_account(uint!(3000_U256), 10)),
                original_info: Some(create_account(uint!(3000_U256), 10)),
                storage: Default::default(),
                status: AccountStatus::Loaded,
            },
        );

        let normalized = normalize_bundle_for_prestate(bundle);

        let account = normalized.state.get(&addr).unwrap();
        assert_eq!(
            account.status,
            AccountStatus::Loaded,
            "Loaded status should be preserved"
        );
    }

    #[test]
    fn test_normalize_bundle_for_prestate_multiple_accounts() {
        let addr1 = address!("0000000000000000000000000000000000000001");
        let addr2 = address!("0000000000000000000000000000000000000002");
        let addr3 = address!("0000000000000000000000000000000000000003");

        let mut bundle = BundleState::default();

        // InMemoryChange -> should become Changed
        bundle.state.insert(
            addr1,
            BundleAccount {
                info: Some(create_account(uint!(100_U256), 1)),
                original_info: None,
                storage: Default::default(),
                status: AccountStatus::InMemoryChange,
            },
        );

        // Changed -> should stay Changed
        bundle.state.insert(
            addr2,
            BundleAccount {
                info: Some(create_account(uint!(200_U256), 2)),
                original_info: Some(create_account(uint!(100_U256), 1)),
                storage: Default::default(),
                status: AccountStatus::Changed,
            },
        );

        // Loaded -> should stay Loaded
        bundle.state.insert(
            addr3,
            BundleAccount {
                info: Some(create_account(uint!(300_U256), 3)),
                original_info: Some(create_account(uint!(300_U256), 3)),
                storage: Default::default(),
                status: AccountStatus::Loaded,
            },
        );

        let normalized = normalize_bundle_for_prestate(bundle);

        assert_eq!(
            normalized.state.get(&addr1).unwrap().status,
            AccountStatus::Changed
        );
        assert_eq!(
            normalized.state.get(&addr2).unwrap().status,
            AccountStatus::Changed
        );
        assert_eq!(
            normalized.state.get(&addr3).unwrap().status,
            AccountStatus::Loaded
        );
    }

    #[test]
    fn test_normalize_bundle_for_prestate_preserves_storage() {
        let addr = address!("0000000000000000000000000000000000000001");
        let slot = uint!(1_U256);
        let value = uint!(42_U256);

        let mut bundle = BundleState::default();
        let mut storage = std::collections::HashMap::default();
        storage.insert(
            slot,
            StorageSlot {
                previous_or_original_value: uint!(0_U256),
                present_value: value,
            },
        );

        bundle.state.insert(
            addr,
            BundleAccount {
                info: Some(create_account(uint!(1000_U256), 1)),
                original_info: None,
                storage,
                status: AccountStatus::InMemoryChange,
            },
        );

        let normalized = normalize_bundle_for_prestate(bundle);

        let account = normalized.state.get(&addr).unwrap();
        assert_eq!(account.storage.len(), 1, "Storage should be preserved");
        assert_eq!(
            account.storage.get(&slot).unwrap().present_value,
            value,
            "Storage value should be preserved"
        );
    }

    #[test]
    fn test_normalize_bundle_for_prestate_preserves_account_info() {
        let addr = address!("0000000000000000000000000000000000000001");
        let balance = uint!(12345_U256);
        let nonce = 99;

        let mut bundle = BundleState::default();
        bundle.state.insert(
            addr,
            BundleAccount {
                info: Some(create_account(balance, nonce)),
                original_info: None,
                storage: Default::default(),
                status: AccountStatus::InMemoryChange,
            },
        );

        let normalized = normalize_bundle_for_prestate(bundle);

        let account = normalized.state.get(&addr).unwrap();
        let info = account.info.as_ref().unwrap();
        assert_eq!(info.balance, balance, "Balance should be preserved");
        assert_eq!(info.nonce, nonce, "Nonce should be preserved");
    }

    #[test]
    fn test_block_access_index_counter_new() {
        let counter = BlockAccessIndexCounter::new(10);
        let (start, end) = counter.finish();

        assert_eq!(start, 10);
        assert_eq!(end, 10, "No indices consumed yet");
    }

    #[test]
    fn test_block_access_index_counter_next_index() {
        let mut counter = BlockAccessIndexCounter::new(0);

        assert_eq!(counter.next_index(), 0);
        assert_eq!(counter.next_index(), 1);
        assert_eq!(counter.next_index(), 2);
    }

    #[test]
    fn test_block_access_index_counter_finish() {
        let mut counter = BlockAccessIndexCounter::new(5);

        counter.next_index(); // 5
        counter.next_index(); // 6
        counter.next_index(); // 7

        let (start, end) = counter.finish();
        assert_eq!(start, 5);
        assert_eq!(end, 8);
    }
}
