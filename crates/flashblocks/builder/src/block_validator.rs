use std::{borrow::Cow, collections::HashSet, sync::Arc};

use alloy_consensus::{Block, BlockHeader, Header, Receipt, Transaction, TxReceipt};
use alloy_eips::{Decodable2718, merge};
use alloy_op_evm::{OpBlockExecutionCtx, OpEvmFactory, block::receipt_builder::OpReceiptBuilder};
use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_rpc_types_engine::PayloadId;
use eyre::eyre::eyre;
use flashblocks_primitives::{
    access_list::{self, FlashblockAccessList},
    ed25519_dalek::ed25519::signature::{digest::consts::U25, rand_core::le},
    primitives::ExecutionPayloadFlashblockDeltaV1,
};
use op_alloy_consensus::{DEPOSIT_TX_TYPE_ID, OpReceipt};
use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};
use reth::{
    core::primitives::receipt,
    revm::{State, database::StateProviderDatabase},
    rpc::api::eth::helpers::trace,
};
use reth_primitives::transaction::SignedTransaction;

use reth_evm::{
    Evm, EvmEnv, EvmEnvFor, EvmFactory, FromRecoveredTx, FromTxWithEncoded,
    block::{BlockExecutionError, BlockExecutor, CommitChanges, StateDB},
    execute::{
        BasicBlockBuilder, BlockAssembler, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome,
        ExecutorTx,
    },
    op_revm::{OpHaltReason, OpSpecId, OpTransaction, transaction::OpTxTr},
};
use reth_node_api::{BuiltPayloadExecutedBlock, NodePrimitives};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpBlockAssembler, OpEvmConfig};
use reth_optimism_node::OpBuiltPayload;
use reth_optimism_primitives::{OpPrimitives, OpTransactionSigned};
use reth_primitives::{Recovered, RecoveredBlock, SealedHeader};
use reth_provider::{BlockExecutionResult, ExecutionOutcome, StateProvider};
use reth_trie_common::{HashedPostState, KeccakKeyHasher, updates::TrieUpdates};
use revm::{
    Database, DatabaseCommit, DatabaseRef,
    bytecode::bitvec::access,
    context::{BlockEnv, TxEnv, result::ExecutionResult},
    database::{
        AccountStatus, BundleAccount, BundleState, TransitionAccount,
        states::{bundle_state::BundleRetention, reverts::Reverts},
    },
    handler::execution,
    interpreter::instructions::tx_info,
};
use tracing::{info, trace};

use crate::{
    access_list::{BlockAccessIndex, FlashblockAccessListConstruction},
    assembler::FlashblocksBlockAssembler,
    block_executor::{
        BalBlockExecutor, BalBlockExecutorFactory, BalExecutorError, BlockAccessIndexCounter,
        CommittedState,
    },
    executor::{
        bal_builder_db::BalBuilderDb,
        bundle_db::BundleDb,
        temporal_db::{self, TemporalDb, TemporalDbFactory},
    },
};

pub trait FlashblockBlockValidator {
    /// Validates a block in parallel using BAL.
    fn validate_flashblock_parallel(
        &self,
        state_provider: Arc<dyn StateProvider>,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent: &SealedHeader<Header>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError>;

    /// Validates a block sequentially without using BAL.
    fn validate_flashblock_sequential(
        &self,
        state_provider: Arc<dyn StateProvider>,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent: &SealedHeader<Header>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError>;
}

pub struct FlashblocksValidatorCtx<R: OpReceiptBuilder + Default> {
    pub chain_spec: Arc<OpChainSpec>,
    pub committed_state: CommittedState<R>,
    pub evm_env: EvmEnvFor<OpEvmConfig>,
    pub evm_config: OpEvmConfig,
    pub execution_context: OpBlockExecutionCtx,
    pub executor_transactions: Vec<(BlockAccessIndex, Recovered<OpTransactionSigned>)>,
}

pub struct FlashblocksBlockValidator<R: OpReceiptBuilder + Default> {
    pub ctx: FlashblocksValidatorCtx<R>,
}

impl<R: OpReceiptBuilder + Default> FlashblocksBlockValidator<R> {
    pub fn new(ctx: FlashblocksValidatorCtx<R>) -> Self {
        Self { ctx }
    }
}

impl<R> FlashblockBlockValidator for FlashblocksBlockValidator<R>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt>
        + Default
        + Clone
        + Send
        + Sync,
{
    fn validate_flashblock_parallel(
        &self,
        state_provider: Arc<dyn StateProvider>,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent: &SealedHeader<Header>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError> {
        let access_list_data = diff.clone().access_list_data.unwrap();

        // 1. Setup database layers for the base evm/executor
        let state_provider_database = StateProviderDatabase::new(state_provider.clone());
        let bundle_db = BundleDb::new(
            state_provider_database,
            self.ctx.committed_state.bundle.clone(),
        );

        let temporal_db_factory =
            TemporalDbFactory::new(Arc::new(bundle_db), access_list_data.access_list.clone());

        let start_index = self.ctx.committed_state.transactions.len() as u64;

        let mut state = State::builder()
            .with_database_ref(temporal_db_factory.db(start_index))
            .with_bundle_prestate(self.ctx.committed_state.bundle.clone())
            .with_bundle_update()
            .build();

        let state_dummy = State::builder()
            .with_database_ref(temporal_db_factory.db(start_index))
            .with_bundle_prestate(self.ctx.committed_state.bundle.clone())
            .with_bundle_update()
            .build();

        let database = BalBuilderDb::new(&mut state, state_dummy);

        // 2. Create channel for state root computation
        let (state_root_sender, state_root_receiver) = crossbeam_channel::bounded(1);

        let mut state_root_bundle = self.ctx.committed_state.bundle.clone();
        state_root_bundle.extend_state(access_list_data.access_list.clone().into());

        let state_provider_clone = state_provider.clone();

        // 3. Spawn the state root computation in a separate thread
        rayon::spawn(move || {
            let result = compute_state_root(state_provider_clone, state_root_bundle.state());
            let _ = state_root_sender.send(result);
        });

        let evm = OpEvmFactory::default().create_evm(database, self.ctx.evm_env.clone());

        let executor = BalBlockExecutor::new(
            evm,
            self.ctx.execution_context.clone(),
            self.ctx.chain_spec.clone(),
            R::default(),
        )
        .with_receipts(self.ctx.committed_state.receipts_iter().cloned().collect())
        .with_gas_used(self.ctx.committed_state.gas_used);

        let start_index = if self.ctx.committed_state.transactions.is_empty() {
            0
        } else {
            self.ctx.committed_state.transactions.len() as u16 + 1
        };

        let index_range = (
            start_index,
            start_index + self.ctx.executor_transactions.len() as u16 +  1,
        );

        let (validator, access_list_receiver) = BalBlockValidator::new(
            self.ctx.execution_context.clone(),
            parent,
            executor,
            self.ctx
                .committed_state
                .transactions_iter()
                .cloned()
                .collect(),
            self.ctx.chain_spec.clone(),
            temporal_db_factory,
            FlashblockAccessList::default(),
            state_root_receiver,
            self.ctx.evm_env.clone(),
            index_range,
        );

        // 4. Compute the block using BAL in parallel
        let (outcome, fees): (BlockBuilderOutcome<OpPrimitives>, u128) = validator.execute_block(
            state_provider.clone(),
            self.ctx.executor_transactions.clone(),
        )?;

        let computed_access_list = access_list_receiver.recv().unwrap(); // TODO:
        let access_list_hash = keccak256(alloy_rlp::encode(&computed_access_list));

        // 5. Verify computed results
        if access_list_hash != access_list_data.clone().access_list_hash {
            trace!(
                target: "flashblocks::state_executor",
                ?access_list_hash,
                expected = ?access_list_data.access_list_hash,
                access_list = ?computed_access_list,
                expected_access_list = ?access_list_data.access_list.clone(),
                "Access list hash mismatch"
            );

            panic!(
                "Access list hash mismatch: expected {:?}, got {:?}",
                access_list_data.access_list_hash, access_list_hash
            );
        }

        if outcome.block.hash() != diff.block_hash {
            trace!(
                target: "flashblocks::state_executor",
                block = ?outcome.block,
                expected = ?diff,
                receipts = ?outcome.execution_result.receipts,
                "Block hash mismatch"
            );
            panic!(
                "Block hash mismatch: expected {:?}, got {:?}",
                diff.block_hash,
                outcome.block.hash()
            );
        }

        // 6. Seal the block
        let BlockBuilderOutcome {
            execution_result,
            block,
            hashed_state,
            trie_updates,
        } = outcome;

        let sealed_block = Arc::new(block.sealed_block().clone());

        let execution_outcome = ExecutionOutcome::new(
            state.take_bundle(),
            vec![execution_result.receipts.clone()],
            block.number(),
            Vec::new(),
        );

        let executed_block: BuiltPayloadExecutedBlock<OpPrimitives> = BuiltPayloadExecutedBlock {
            recovered_block: Arc::new(block),
            execution_output: Arc::new(execution_outcome),
            hashed_state: either::Left(Arc::new(hashed_state)),
            trie_updates: either::Left(Arc::new(trie_updates)),
        };

        let payload = OpBuiltPayload::new(
            payload_id,
            sealed_block,
            U256::from(fees),
            Some(executed_block),
        );

        Ok(payload)
    }

    fn validate_flashblock_sequential(
        &self,
        state_provider: Arc<dyn StateProvider>,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent: &SealedHeader<Header>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError> {
        todo!()
    }
}
/// Result of executing a single transaction in parallel.
///
/// This struct accumulates execution artifacts that will be merged
/// after all transactions complete.
#[derive(Default)]
pub struct ParalleExecutionResult {
    /// State transitions from this transaction
    pub transitions: revm::primitives::map::HashMap<Address, TransitionAccount>,
    /// Receipt generated by this transaction
    pub receipts: Vec<OpReceipt>,
    /// Gas consumed by this transaction
    pub gas_used: u64,
    /// Fees earned from this transaction
    pub fees: u128,
    /// Access list entries from this transaction
    pub access_list: FlashblockAccessListConstruction,
    /// Transaction index within the block
    pub index: BlockAccessIndex,
}

/// A wrapper around the [`BasicBlockBuilder`] for flashblocks.
pub struct BalBlockValidator<'a, DbRef: DatabaseRef + 'static, R: OpReceiptBuilder, Evm> {
    pub inner: BasicBlockBuilder<
        'a,
        BalBlockExecutorFactory,
        BalBlockExecutor<Evm, R>,
        OpBlockAssembler<OpChainSpec>,
        OpPrimitives,
    >,
    pub access_list_sender: crossbeam_channel::Sender<FlashblockAccessList>,
    pub state_root_receiver:
        crossbeam_channel::Receiver<Result<StateRootResult, BlockExecutionError>>,
    pub counter: BlockAccessIndexCounter,
    pub temporal_db_factory: TemporalDbFactory<DbRef>,
    pub access_list: FlashblockAccessList,
    /// The EVM environment for transaction execution (contains chain_id, etc.)
    pub evm_env: EvmEnv<OpSpecId>,
    pub index_range: (u16, u16),
}

impl<'a, DbRef, DB, R, E> BalBlockValidator<'a, DbRef, R, E>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt>,
    DB: StateDB + DatabaseCommit + reth_evm::Database + 'a,
    DbRef: DatabaseRef + 'static,
    E: Evm<DB = BalBuilderDb<DB>, Tx = OpTransaction<TxEnv>, Spec = OpSpecId, BlockEnv = BlockEnv>,
    OpTransaction<TxEnv>: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
{
    /// Creates a new [`FlashblocksBlockBuilder`] with the given executor factory and assembler.
    pub fn new(
        ctx: OpBlockExecutionCtx,
        parent: &'a SealedHeader<Header>,
        executor: BalBlockExecutor<E, R>,
        transactions: Vec<Recovered<OpTransactionSigned>>,
        chain_spec: Arc<OpChainSpec>,
        temporal_db_factory: TemporalDbFactory<DbRef>,
        access_list: FlashblockAccessList,
        state_root_receiver: crossbeam_channel::Receiver<
            Result<StateRootResult, BlockExecutionError>,
        >,
        evm_env: EvmEnv<OpSpecId>,
        index_range: (u16, u16),
    ) -> (Self, crossbeam_channel::Receiver<FlashblockAccessList>) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let counter = BlockAccessIndexCounter::new(transactions.len() as u16);

        (
            Self {
                inner: BasicBlockBuilder {
                    executor,
                    assembler: OpBlockAssembler::new(chain_spec),
                    ctx,
                    parent,
                    transactions,
                },
                access_list_sender: tx,
                state_root_receiver,
                counter,
                temporal_db_factory,
                access_list,
                evm_env,
                index_range,
            },
            rx,
        )
    }

    pub fn prepare_database(&mut self, index: u16) -> Result<(), BlockExecutionError> {
        let db = self.inner.executor.inner.evm_mut().db_mut();
        db.set_index(index);
        Ok(())
    }
}

impl<'a, DB, DbRef, R, E> BlockBuilder for BalBlockValidator<'a, DbRef, R, E>
where
    DbRef: DatabaseRef,
    DB: StateDB + DatabaseCommit + reth_evm::Database + 'a,
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
    type Primitives = OpPrimitives;
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
    ) -> Result<BlockBuilderOutcome<OpPrimitives>, BlockExecutionError> {
        let (evm, result) = self.inner.executor.finish()?;
        let (mut db, evm_env) = evm.finish();

        // Normalize bundle state account statuses before merging transitions.
        // This ensures valid state machine transitions by converting problematic statuses:
        // - InMemoryChange -> Changed
        // - LoadedNotExisting -> LoadedEmptyEIP161 (for accounts that will be changed)
        for (_address, account) in db.bundle_state_mut().state.iter_mut() {
            match account.status {
                AccountStatus::InMemoryChange => {
                    account.status = AccountStatus::Changed;
                }
                AccountStatus::LoadedNotExisting => {
                    // If account doesn't exist but will be changed, treat as empty
                    account.status = AccountStatus::LoadedEmptyEIP161;
                }
                _ => {}
            }
        }

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

        // Wait for the state root result from the async computation
        let StateRootResult {
            state_root,
            trie_updates,
            hashed_state,
        } = self
            .state_root_receiver
            .recv()
            .map_err(|e| BlockExecutionError::other(e))??;

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
        let access_list = db.finish()?.build(self.index_range);

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

impl<'a, DbRef, DB, R, E> BalBlockValidator<'a, DbRef, R, E>
where
    DbRef: DatabaseRef + Send + Sync + std::fmt::Debug + Clone + 'a,
    DB: reth_evm::Database + 'a,
    E: Evm<
            DB = BalBuilderDb<&'a mut State<DB>>,
            Tx = OpTransaction<TxEnv>,
            Spec = OpSpecId,
            HaltReason = OpHaltReason,
            BlockEnv = BlockEnv,
        >,
    R: OpReceiptBuilder<Receipt = OpReceipt, Transaction = OpTransactionSigned>
        + Send
        + Sync
        + Clone,
    OpTransaction<TxEnv>:
        FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
{
    pub fn execute_block(
        mut self,
        state_provider: impl StateProvider + Clone,
        transactions: impl IntoParallelIterator<
            Item = (BlockAccessIndex, Recovered<OpTransactionSigned>),
        > + IntoIterator<Item = (BlockAccessIndex, Recovered<OpTransactionSigned>)>
        + Clone,
    ) -> Result<(BlockBuilderOutcome<OpPrimitives>, u128), BlockExecutionError> {
        let spec = self.inner.executor.inner.spec.clone();
        let receipt_builder: R = self.inner.executor.inner.receipt_builder.clone();
        let execution_context = self.inner.ctx.clone();
        let evm_env = self.evm_env.clone();
        let gas_used = self.inner.executor.inner.gas_used;

        let db_factory = &self.temporal_db_factory;

        trace!(
            target: "flashblocks::builder::block_validator",
            tx_count = transactions.clone().into_iter().count(),
            "Starting parallel block execution"
        );

        let mut results = transactions
            .clone()
            .into_par_iter()
            .map(|(index, tx)| {
                let tx = tx.clone();
                execute_transaction(
                    (index, tx),
                    receipt_builder.clone(),
                    spec.clone(),
                    execution_context.clone(),
                    evm_env.clone(),
                    &db_factory,
                )
            })
            .collect::<Vec<_>>();

        results.sort_unstable_by_key(|r| r.index);

        let merged_result = merge_transaction_results(results, gas_used);

        if self.inner.executor.inner.receipts.is_empty() {
            self.inner.executor.inner.evm.db_mut().set_index(0);
            self.apply_pre_execution_changes()?;
        }

        let database = self.inner.executor_mut().evm_mut().db_mut();

        // Normalize transitions for compatibility with bundle prestate.
        // This converts InMemoryChange statuses to Changed to avoid invalid state transitions.
        let normalized_transitions = normalize_transitions_for_prestate(merged_result.transitions);

        // merge all transitions into bundle state
        database.db_mut().apply_transition(normalized_transitions);

        // merge the aggregated access list into the database
        database.merge_access_list(merged_result.access_list);

        // update the index to the post-execution changes index
        database.set_index(merged_result.index as u16 + 1);

        self.inner
            .executor
            .inner
            .receipts
            .extend_from_slice(&merged_result.receipts);

        self.inner.executor.inner.gas_used += merged_result.gas_used;

        self.inner.transactions.extend_from_slice(
            &transactions
                .into_iter()
                .map(|(_, tx)| tx)
                .collect::<Vec<_>>(),
        );

        Ok((self.finish(state_provider)?, merged_result.fees))
    }
}

/// Normalizes transition account statuses for merging into a bundle with prestate.
///
/// When transitions from parallel execution are applied to a bundle that has accounts
/// from a previous execution, the status machine in revm may reject certain transitions.
/// Valid transitions to `Changed` are only from: `Changed`, `Loaded`, `LoadedEmptyEIP161`.
///
/// This function normalizes statuses to ensure compatibility:
/// - `InMemoryChange` -> `Changed` (for current status)
/// - `InMemoryChange` -> `Loaded` (for previous status)
/// - Also handles previous_status normalization for valid state machine transitions
fn normalize_transitions_for_prestate(
    transitions: revm::primitives::map::HashMap<Address, TransitionAccount>,
) -> revm::primitives::map::HashMap<Address, TransitionAccount> {
    transitions
        .into_iter()
        .map(|(addr, mut transition)| {
            // Normalize current status: InMemoryChange -> Changed
            if transition.status == AccountStatus::InMemoryChange {
                transition.status = AccountStatus::Changed;
            }

            // Normalize previous status to ensure valid state transitions
            // Valid sources for Changed: Changed, Loaded, LoadedEmptyEIP161
            match transition.previous_status {
                AccountStatus::InMemoryChange => {
                    transition.previous_status = AccountStatus::Loaded;
                }
                AccountStatus::LoadedNotExisting => {
                    // If account didn't exist but is now changed, treat as loaded empty
                    transition.previous_status = AccountStatus::LoadedEmptyEIP161;
                }
                _ => {}
            }

            (addr, transition)
        })
        .collect()
}

/// Merge individual transaction results into an aggregated result.
fn merge_transaction_results(
    results: Vec<ParalleExecutionResult>,
    initial_gas_used: u64,
) -> ParalleExecutionResult {
    results.into_iter().fold(
        ParalleExecutionResult {
            gas_used: initial_gas_used,
            ..Default::default()
        },
        |mut acc, mut res| {
            acc.transitions.extend(res.transitions);
            acc.gas_used += res.gas_used;

            if let Some(mut receipt) = res.receipts.pop() {
                receipt.as_receipt_mut().cumulative_gas_used = acc.gas_used;
                acc.receipts.push(receipt);
            }

            acc.access_list.merge(res.access_list);
            acc.fees += res.fees;
            acc.index = res.index;
            acc
        },
    )
}

pub fn execute_transaction<'a, R, DBRef>(
    (index, tx): (BlockAccessIndex, Recovered<OpTransactionSigned>),
    receipt_builder: R,
    spec: Arc<OpChainSpec>,
    execution_context: OpBlockExecutionCtx,
    evm_env: EvmEnv<OpSpecId>,
    db_factory: &TemporalDbFactory<DBRef>,
) -> ParalleExecutionResult
where
    DBRef: DatabaseRef + Clone + Send + Sync + std::fmt::Debug + 'static,
    R: OpReceiptBuilder<Receipt = OpReceipt, Transaction = OpTransactionSigned>
        + Send
        + Sync
        + Clone,
    OpTransaction<TxEnv>:
        FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
{
    use alloy_consensus::Typed2718;

    let temporal_db = db_factory.db(index as u64);

    let state = State::builder()
        .with_database_ref(temporal_db.clone())
        .with_bundle_update()
        .build();

    let cached_db = State::builder()
        .with_database_ref(temporal_db)
        .with_bundle_update()
        .build();

    let mut database = BalBuilderDb::new(state, cached_db);
    database.set_index(index);

    let evm = OpEvmFactory::default().create_evm(database, evm_env);

    let mut executor = BalBlockExecutor::new(
        evm,
        execution_context.clone(),
        spec.clone(),
        receipt_builder.clone(),
    );
    executor
        .execute_transaction_with_commit_condition(tx.as_executable(), |_| CommitChanges::Yes)
        .unwrap(); // TODO:

    let (evm, result) = executor.finish().unwrap();

    let fees = if tx.is_type(DEPOSIT_TX_TYPE_ID) {
        0
    } else {
        tx.effective_tip_per_gas(evm.block().basefee)
            .expect("fee is always valid; execution succeeded")
    };

    let (mut db, _evm_env) = evm.finish();

    let transitions = db
        .db_mut()
        .transition_state
        .as_mut()
        .map(|t| t.take())
        .unwrap_or_default();

    let access_list = db.finish().unwrap();

    ParalleExecutionResult {
        transitions: transitions.transitions,
        receipts: result.receipts,
        gas_used: result.gas_used,
        fees: fees,
        access_list,
        index,
    }
}

/// Result of computing the state root from a bundle state.
pub struct StateRootResult {
    pub state_root: B256,
    pub trie_updates: TrieUpdates,
    pub hashed_state: HashedPostState,
}

pub fn compute_state_root(
    state_provider: Arc<dyn StateProvider>,
    bundle_state: &alloy_primitives::map::HashMap<Address, BundleAccount>,
) -> Result<StateRootResult, BlockExecutionError> {
    // compute hashed post state
    let hashed_state = HashedPostState::from_bundle_state::<KeccakKeyHasher>(bundle_state);

    // compute state root & trie updates
    let (state_root, trie_updates) = state_provider
        .state_root_with_updates(hashed_state.clone())
        .map_err(BlockExecutionError::other)?;

    Ok(StateRootResult {
        state_root,
        trie_updates,
        hashed_state,
    })
}

/// Decodes transactions from raw bytes and recovers signer addresses.
pub fn decode_transactions(
    encoded_transactions: &[Bytes],
    start_index: BlockAccessIndex,
) -> Result<Vec<(BlockAccessIndex, Recovered<OpTransactionSigned>)>, BalExecutorError> {
    encoded_transactions
        .iter()
        .enumerate()
        .map(|(i, tx)| {
            let tx_envelope = OpTransactionSigned::decode_2718(&mut tx.as_ref())
                .map_err(|e| eyre!("failed to decode transaction: {e}"))
                .unwrap(); // TODO:

            let recovered = tx_envelope
                .try_clone_into_recovered()
                .map_err(|e| eyre!("failed to recover transaction from signed envelope: {e}"))
                .unwrap(); // TODO:

            Ok((start_index + i as BlockAccessIndex, recovered))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::TxEip1559;
    use alloy_primitives::{address, uint, Signature, TxKind, U256};
    use op_alloy_consensus::OpTypedTransaction;
    use revm::{
        database::InMemoryDB,
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

    // Helper to create a simple unsigned EIP-1559 transaction
    #[allow(dead_code)]
    fn create_eip1559_tx(
        chain_id: u64,
        nonce: u64,
        to: Address,
        value: U256,
        gas_limit: u64,
    ) -> TxEip1559 {
        TxEip1559 {
            chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas: 20_000_000_000,
            max_priority_fee_per_gas: 1_000_000_000,
            to: TxKind::Call(to),
            value,
            access_list: Default::default(),
            input: Default::default(),
        }
    }

    // Helper to create a signed OP transaction
    #[allow(dead_code)]
    fn create_signed_op_tx(
        chain_id: u64,
        nonce: u64,
        to: Address,
        value: U256,
        gas_limit: u64,
    ) -> OpTransactionSigned {
        let inner_tx = create_eip1559_tx(chain_id, nonce, to, value, gas_limit);
        let typed_tx = OpTypedTransaction::Eip1559(inner_tx);

        // Create a dummy signature (for testing purposes only)
        let signature = Signature::new(
            uint!(0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef_U256),
            uint!(0xfedcba0987654321fedcba0987654321fedcba0987654321fedcba0987654321_U256),
            false,
        );

        OpTransactionSigned::new_unhashed(typed_tx, signature)
    }

    #[test]
    fn test_normalize_transitions_converts_in_memory_change_to_changed() {
        let addr = address!("0000000000000000000000000000000000000001");

        let mut transitions = revm::primitives::map::HashMap::default();
        transitions.insert(
            addr,
            TransitionAccount {
                info: Some(create_account(uint!(1000_U256), 1)),
                status: AccountStatus::InMemoryChange,
                previous_info: Some(create_account(uint!(500_U256), 0)),
                previous_status: AccountStatus::InMemoryChange,
                storage: Default::default(),
                storage_was_destroyed: false,
            },
        );

        let normalized = normalize_transitions_for_prestate(transitions);

        let transition = normalized.get(&addr).unwrap();
        assert_eq!(
            transition.status,
            AccountStatus::Changed,
            "InMemoryChange status should be converted to Changed"
        );
        assert_eq!(
            transition.previous_status,
            AccountStatus::Loaded,
            "InMemoryChange previous_status should be converted to Loaded"
        );
    }

    #[test]
    fn test_normalize_transitions_converts_loaded_not_existing() {
        let addr = address!("0000000000000000000000000000000000000002");

        let mut transitions = revm::primitives::map::HashMap::default();
        transitions.insert(
            addr,
            TransitionAccount {
                info: Some(create_account(uint!(1000_U256), 1)),
                status: AccountStatus::Changed,
                previous_info: None,
                previous_status: AccountStatus::LoadedNotExisting,
                storage: Default::default(),
                storage_was_destroyed: false,
            },
        );

        let normalized = normalize_transitions_for_prestate(transitions);

        let transition = normalized.get(&addr).unwrap();
        assert_eq!(
            transition.previous_status,
            AccountStatus::LoadedEmptyEIP161,
            "LoadedNotExisting previous_status should be converted to LoadedEmptyEIP161"
        );
    }

    #[test]
    fn test_normalize_transitions_preserves_valid_statuses() {
        let addr = address!("0000000000000000000000000000000000000003");

        let mut transitions = revm::primitives::map::HashMap::default();
        transitions.insert(
            addr,
            TransitionAccount {
                info: Some(create_account(uint!(2000_U256), 5)),
                status: AccountStatus::Changed,
                previous_info: Some(create_account(uint!(1000_U256), 4)),
                previous_status: AccountStatus::Loaded,
                storage: Default::default(),
                storage_was_destroyed: false,
            },
        );

        let normalized = normalize_transitions_for_prestate(transitions);

        let transition = normalized.get(&addr).unwrap();
        assert_eq!(
            transition.status,
            AccountStatus::Changed,
            "Changed status should be preserved"
        );
        assert_eq!(
            transition.previous_status,
            AccountStatus::Loaded,
            "Loaded previous_status should be preserved"
        );
    }

    #[test]
    fn test_normalize_multiple_transitions() {
        let addr1 = address!("0000000000000000000000000000000000000001");
        let addr2 = address!("0000000000000000000000000000000000000002");
        let addr3 = address!("0000000000000000000000000000000000000003");

        let mut transitions = revm::primitives::map::HashMap::default();

        // InMemoryChange -> should become Changed
        transitions.insert(
            addr1,
            TransitionAccount {
                info: Some(create_account(uint!(100_U256), 1)),
                status: AccountStatus::InMemoryChange,
                previous_info: None,
                previous_status: AccountStatus::LoadedNotExisting,
                storage: Default::default(),
                storage_was_destroyed: false,
            },
        );

        // Changed -> should stay Changed
        transitions.insert(
            addr2,
            TransitionAccount {
                info: Some(create_account(uint!(200_U256), 2)),
                status: AccountStatus::Changed,
                previous_info: Some(create_account(uint!(100_U256), 1)),
                previous_status: AccountStatus::Loaded,
                storage: Default::default(),
                storage_was_destroyed: false,
            },
        );

        // InMemoryChange with InMemoryChange previous
        transitions.insert(
            addr3,
            TransitionAccount {
                info: Some(create_account(uint!(300_U256), 3)),
                status: AccountStatus::InMemoryChange,
                previous_info: Some(create_account(uint!(150_U256), 2)),
                previous_status: AccountStatus::InMemoryChange,
                storage: Default::default(),
                storage_was_destroyed: false,
            },
        );

        let normalized = normalize_transitions_for_prestate(transitions);

        // Check addr1
        let t1 = normalized.get(&addr1).unwrap();
        assert_eq!(t1.status, AccountStatus::Changed);
        assert_eq!(t1.previous_status, AccountStatus::LoadedEmptyEIP161);

        // Check addr2
        let t2 = normalized.get(&addr2).unwrap();
        assert_eq!(t2.status, AccountStatus::Changed);
        assert_eq!(t2.previous_status, AccountStatus::Loaded);

        // Check addr3
        let t3 = normalized.get(&addr3).unwrap();
        assert_eq!(t3.status, AccountStatus::Changed);
        assert_eq!(t3.previous_status, AccountStatus::Loaded);
    }

    #[test]
    fn test_merge_transaction_results_empty() {
        let results = vec![];
        let merged = merge_transaction_results(results, 0);

        assert_eq!(merged.gas_used, 0);
        assert!(merged.transitions.is_empty());
        assert!(merged.receipts.is_empty());
        assert_eq!(merged.fees, 0);
    }

    #[test]
    fn test_merge_transaction_results_accumulates_gas() {
        let result1 = ParalleExecutionResult {
            transitions: Default::default(),
            receipts: vec![],
            gas_used: 21000,
            fees: 100,
            access_list: Default::default(),
            index: 0,
        };

        let result2 = ParalleExecutionResult {
            transitions: Default::default(),
            receipts: vec![],
            gas_used: 50000,
            fees: 200,
            access_list: Default::default(),
            index: 1,
        };

        let merged = merge_transaction_results(vec![result1, result2], 0);

        assert_eq!(merged.gas_used, 71000, "Gas should be accumulated");
        assert_eq!(merged.fees, 300, "Fees should be accumulated");
    }

    #[test]
    fn test_merge_transaction_results_merges_transitions() {
        let addr1 = address!("0000000000000000000000000000000000000001");
        let addr2 = address!("0000000000000000000000000000000000000002");

        let mut transitions1 = revm::primitives::map::HashMap::default();
        transitions1.insert(
            addr1,
            TransitionAccount {
                info: Some(create_account(uint!(100_U256), 1)),
                status: AccountStatus::Changed,
                previous_info: None,
                previous_status: AccountStatus::Loaded,
                storage: Default::default(),
                storage_was_destroyed: false,
            },
        );

        let mut transitions2 = revm::primitives::map::HashMap::default();
        transitions2.insert(
            addr2,
            TransitionAccount {
                info: Some(create_account(uint!(200_U256), 2)),
                status: AccountStatus::Changed,
                previous_info: None,
                previous_status: AccountStatus::Loaded,
                storage: Default::default(),
                storage_was_destroyed: false,
            },
        );

        let result1 = ParalleExecutionResult {
            transitions: transitions1,
            receipts: vec![],
            gas_used: 21000,
            fees: 100,
            access_list: Default::default(),
            index: 0,
        };

        let result2 = ParalleExecutionResult {
            transitions: transitions2,
            receipts: vec![],
            gas_used: 21000,
            fees: 100,
            access_list: Default::default(),
            index: 1,
        };

        let merged = merge_transaction_results(vec![result1, result2], 0);

        assert_eq!(
            merged.transitions.len(),
            2,
            "Both transitions should be merged"
        );
        assert!(merged.transitions.contains_key(&addr1));
        assert!(merged.transitions.contains_key(&addr2));
    }

    #[test]
    fn test_parallel_execution_result_default() {
        let result = ParalleExecutionResult::default();

        assert!(result.transitions.is_empty());
        assert!(result.receipts.is_empty());
        assert_eq!(result.gas_used, 0);
        assert_eq!(result.fees, 0);
        assert_eq!(result.index, 0);
    }

    #[test]
    fn test_block_access_index_counter() {
        use crate::block_executor::BlockAccessIndexCounter;

        let mut counter = BlockAccessIndexCounter::new(5);

        assert_eq!(counter.next_index(), 5);
        assert_eq!(counter.next_index(), 6);
        assert_eq!(counter.next_index(), 7);

        let (start, end) = counter.finish();
        assert_eq!(start, 5);
        assert_eq!(end, 8);
    }

    #[test]
    fn test_block_access_index_counter_from_zero() {
        use crate::block_executor::BlockAccessIndexCounter;

        let mut counter = BlockAccessIndexCounter::new(0);

        assert_eq!(counter.next_index(), 0);
        assert_eq!(counter.next_index(), 1);
        assert_eq!(counter.next_index(), 2);

        let (start, end) = counter.finish();
        assert_eq!(start, 0);
        assert_eq!(end, 3);
    }
}
