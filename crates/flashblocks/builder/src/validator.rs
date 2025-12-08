use std::{borrow::Cow, collections::HashSet, sync::Arc};

use alloy_consensus::{BlockHeader, Header, Transaction};
use alloy_eips::Decodable2718;
use alloy_op_evm::{
    OpBlockExecutionCtx, OpBlockExecutor, OpBlockExecutorFactory, OpEvmFactory,
    block::receipt_builder::OpReceiptBuilder,
};
use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_rpc_types_engine::PayloadId;
use flashblocks_primitives::{
    access_list::{FlashblockAccessList, FlashblockAccessListData},
    primitives::ExecutionPayloadFlashblockDeltaV1,
};
use op_alloy_consensus::OpReceipt;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use reth::revm::{State, database::StateProviderDatabase};
use reth_primitives::transaction::SignedTransaction;

use reth_evm::{
    Evm, EvmEnv, EvmEnvFor, EvmFactory, FromRecoveredTx, FromTxWithEncoded,
    block::{
        BlockExecutionError, BlockExecutor, CommitChanges, InternalBlockExecutionError, StateDB,
    },
    execute::{
        BasicBlockBuilder, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome, ExecutorTx,
    },
    op_revm::{OpHaltReason, OpSpecId, OpTransaction},
};
use reth_node_api::BuiltPayloadExecutedBlock;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpBlockAssembler, OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_node::OpBuiltPayload;
use reth_optimism_primitives::{OpPrimitives, OpTransactionSigned};
use reth_primitives::{Recovered, RecoveredBlock, SealedHeader};
use reth_provider::{ExecutionOutcome, StateProvider};
use reth_trie_common::{HashedPostState, KeccakKeyHasher, updates::TrieUpdates};
use revm::{
    DatabaseRef,
    context::{BlockEnv, TxEnv, result::ExecutionResult},
    database::{
        BundleAccount, BundleState, TransitionAccount,
        states::{bundle_state::BundleRetention, reverts::Reverts},
    },
};
use revm_database_interface::WrapDatabaseRef;
use tracing::{error, trace};

use crate::{
    access_list::{BlockAccessIndex, FlashblockAccessListConstruction},
    database::{
        bal_builder_db::AsyncBalBuilderDb,
        bundle_db::BundleDb,
        temporal_db::{TemporalDb, TemporalDbFactory},
    },
    executor::{BalExecutorError, CommittedState},
};

/// Context required for flashblocks block validation.
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

impl<R> FlashblocksBlockValidator<R>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt>
        + Default
        + Clone
        + Send
        + Sync,
{
    pub fn validate(
        &self,
        state_provider: Arc<dyn StateProvider>,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent: &SealedHeader<Header>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError> {
        let FlashblockAccessListData {
            access_list,
            access_list_hash,
        } = diff.access_list_data.unwrap();

        let index_range = (access_list.min_tx_index, access_list.max_tx_index);

        // 1. Setup database layers for the base evm/executor
        let state_provider_database = StateProviderDatabase::new(state_provider.clone());

        let bundle_db = BundleDb::new(
            state_provider_database.clone(),
            self.ctx.committed_state.bundle.clone(),
        );

        let temporal_db_factory = TemporalDbFactory::new(Arc::new(bundle_db), access_list.clone());

        let mut state = State::builder()
            .with_database_ref(temporal_db_factory.db(index_range.0 as u64))
            .with_bundle_prestate(self.ctx.committed_state.bundle.clone())
            .with_bundle_update()
            .build();

        let dummy = State::builder()
            .with_database_ref(temporal_db_factory.db(index_range.0 as u64))
            .with_bundle_prestate(self.ctx.committed_state.bundle.clone())
            .with_bundle_update()
            .build();

        let database = AsyncBalBuilderDb::new(&mut state, dummy);

        // 2. Create channel for state root computation
        let (state_root_sender, state_root_receiver) = crossbeam_channel::bounded(1);

        // The full bundle we expect to compute after execution
        // [committed bundle + access list changes]
        let mut state_root_bundle = self.ctx.committed_state.bundle.clone();
        access_list
            .extend_bundle(&mut state_root_bundle, &state_provider_database)
            .map_err(BalExecutorError::other)?;

        let state_provider_clone = state_provider.clone();

        // 3. Spawn the state root computation in a separate thread
        rayon::spawn(move || {
            let result =
                compute_state_root(state_provider_clone, state_root_bundle.clone().state());
            let _ = state_root_sender.send(result);
        });

        let evm = OpEvmFactory::default().create_evm(database, self.ctx.evm_env.clone());

        let mut executor = OpBlockExecutor::new(
            evm,
            self.ctx.execution_context.clone(),
            self.ctx.chain_spec.clone(),
            R::default(),
        );

        executor.gas_used = self.ctx.committed_state.gas_used;
        executor.receipts = self.ctx.committed_state.receipts_iter().cloned().collect();

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
            state_root_receiver,
            self.ctx.evm_env.clone(),
            index_range,
        );

        // 4. Compute the block using BAL in parallel
        let (outcome, fees): (BlockBuilderOutcome<OpPrimitives>, u128) = validator.execute_block(
            state_provider.clone(),
            self.ctx.executor_transactions.clone(),
        )?;

        let computed_access_list = access_list_receiver
            .recv()
            .map_err(BalExecutorError::other)?;

        let computed_access_list_hash =
            flashblocks_primitives::access_list::access_list_hash(&computed_access_list);

        // 5. Verify computed results
        if computed_access_list_hash != access_list_hash {
            error!(
                target: "flashblocks::state_executor",
                ?access_list_hash,
                expected = ?access_list_hash,
                access_list = ?computed_access_list,
                expected_access_list = ?access_list.clone(),
                "Access list hash mismatch"
            );

            return Err(BalExecutorError::BalHashMismatch {
                expected: access_list_hash,
                got: computed_access_list_hash,
            });
        }

        if outcome.block.receipts_root != diff.receipts_root {
            error!(
                target: "flashblocks::state_executor",
                got = ?outcome.block.receipts_root,
                expected = ?diff.receipts_root,
                "Receipts root mismatch"
            );

            return Err(BalExecutorError::ReceiptsRootMismatch {
                expected: diff.receipts_root,
                got: outcome.block.receipts_root,
            });
        }

        if outcome.block.state_root != diff.state_root {
            error!(
                target: "flashblocks::state_executor",
                got = ?outcome.block.state_root,
                expected = ?diff.state_root,
                "State root mismatch"
            );

            return Err(BalExecutorError::StateRootMismatch {
                expected: diff.state_root,
                got: outcome.block.state_root,
            });
        }

        if outcome.block.hash() != diff.block_hash {
            error!(
                target: "flashblocks::state_executor",
                got = ?outcome.block.hash(),
                expected = ?diff.block_hash,
                "Block hash mismatch"
            );

            return Err(BalExecutorError::BalHashMismatch {
                expected: diff.block_hash,
                got: outcome.block.hash(),
            });
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
        OpBlockExecutorFactory<OpRethReceiptBuilder, OpChainSpec>,
        OpBlockExecutor<Evm, R, Arc<OpChainSpec>>,
        OpBlockAssembler<OpChainSpec>,
        OpPrimitives,
    >,
    pub access_list_sender: crossbeam_channel::Sender<FlashblockAccessList>,
    pub state_root_receiver:
        crossbeam_channel::Receiver<Result<StateRootResult, BlockExecutionError>>,
    pub temporal_db_factory: TemporalDbFactory<DbRef>,
    pub evm_env: EvmEnv<OpSpecId>,
    pub index_range: (u16, u16),
}

impl<'a, DB, R, E> BalBlockValidator<'a, DB, R, E>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt>,
    DB: DatabaseRef + Send + Sync + std::fmt::Debug + Clone + 'static,
    E: Evm<
            DB = AsyncBalBuilderDb<&'a mut State<WrapDatabaseRef<TemporalDb<DB>>>>,
            Tx = OpTransaction<TxEnv>,
            Spec = OpSpecId,
            BlockEnv = BlockEnv,
        >,
    OpTransaction<TxEnv>: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
{
    /// Creates a new [`FlashblocksBlockBuilder`] with the given executor factory and assembler.
    pub fn new(
        ctx: OpBlockExecutionCtx,
        parent: &'a SealedHeader<Header>,
        executor: OpBlockExecutor<E, R, Arc<OpChainSpec>>,
        transactions: Vec<Recovered<OpTransactionSigned>>,
        chain_spec: Arc<OpChainSpec>,
        temporal_db_factory: TemporalDbFactory<DB>,
        state_root_receiver: crossbeam_channel::Receiver<
            Result<StateRootResult, BlockExecutionError>,
        >,
        evm_env: EvmEnv<OpSpecId>,
        index_range: (u16, u16),
    ) -> (Self, crossbeam_channel::Receiver<FlashblockAccessList>) {
        let (tx, rx) = crossbeam_channel::bounded(1);

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
                temporal_db_factory,
                evm_env,
                index_range,
            },
            rx,
        )
    }

    /// Prepares the underlying temporal database for execution.
    pub fn prepare_database(&mut self, index: u16) -> Result<(), BlockExecutionError> {
        let db = self.inner.executor.evm_mut().db_mut();
        // Set the temporal db index
        db.db_mut().database.0.set_index(index as u64);
        // Set the bal builder db index
        db.set_index(index);
        Ok(())
    }
}

impl<'a, DbRef, R, E> BlockBuilder for BalBlockValidator<'a, DbRef, R, E>
where
    DbRef: DatabaseRef + Send + Sync + std::fmt::Debug + Clone + 'a,
    E: Evm<
            DB = AsyncBalBuilderDb<&'a mut State<WrapDatabaseRef<TemporalDb<DbRef>>>>,
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
        mut self,
        state: impl StateProvider,
    ) -> Result<BlockBuilderOutcome<OpPrimitives>, BlockExecutionError> {
        self.prepare_database(self.index_range.1)?;

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

        // Wait for the state root result from the async computation
        let StateRootResult {
            state_root,
            trie_updates,
            hashed_state,
        } = self
            .state_root_receiver
            .recv()
            .map_err(BlockExecutionError::other)??;

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
            .map_err(|e| InternalBlockExecutionError::other(e))?
            .build(self.index_range);

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

impl<'a, DbRef, R, E> BalBlockValidator<'a, DbRef, R, E>
where
    DbRef: DatabaseRef + Send + Sync + std::fmt::Debug + Clone + 'a,
    E: Evm<
            DB = AsyncBalBuilderDb<&'a mut State<WrapDatabaseRef<TemporalDb<DbRef>>>>,
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
    ) -> Result<(BlockBuilderOutcome<OpPrimitives>, u128), BalExecutorError> {
        let spec = self.inner.executor.spec.clone();
        let receipt_builder: R = self.inner.executor.receipt_builder.clone();
        let execution_context = self.inner.ctx.clone();
        let evm_env = self.evm_env.clone();
        let gas_used = self.inner.executor.gas_used;

        let db_factory = &self.temporal_db_factory;

        trace!(
            target: "flashblocks::builder::block_validator",
            tx_count = transactions.clone().into_iter().count(),
            "Starting parallel block execution"
        );

        // TODO: Remove this in favor of using BundleDb directly
        let bundle_state = self.inner.executor.evm.db().bundle_state().clone();

        // Execute each transaction in parallel over a temporal view of the database
        // formed from the BAL provided. We reduce the results to aggregate the state transitions,
        // receipts, gas used, and access list. Then pre-load the aggregated results into the base
        // executor to finalize the block.
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
                    db_factory,
                    bundle_state.clone(),
                )
            })
            .collect::<Result<Vec<_>, BalExecutorError>>()?;

        // Sort results by transaction ascending index
        results.sort_unstable_by_key(|r| r.index);

        let merged_result = merge_transaction_results(results, gas_used);

        // If we have no pre-loaded receipts then we need to apply pre-execution changes
        if self.inner.executor.receipts.is_empty() {
            self.prepare_database(0)?;
            self.apply_pre_execution_changes()?;
        }

        let database = self.inner.executor_mut().evm_mut().db_mut();

        // apply all transitions into the cache state
        database
            .db_mut()
            .apply_transition(merged_result.transitions);

        // merge the aggregated access list into the AsyncBalBuilderDb
        database.merge_access_list(merged_result.access_list);

        // append the accumulated receipts and gas used into the executor
        self.inner
            .executor
            .receipts
            .extend_from_slice(&merged_result.receipts);

        self.inner.executor.gas_used = merged_result.gas_used;

        // append the _executed_ transactions into the executor
        self.inner.transactions.extend_from_slice(
            &transactions
                .into_iter()
                .map(|(_, tx)| tx)
                .collect::<Vec<_>>(),
        );

        Ok((self.finish(state_provider)?, merged_result.fees))
    }
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

pub fn execute_transaction<R, DBRef>(
    (index, tx): (BlockAccessIndex, Recovered<OpTransactionSigned>),
    receipt_builder: R,
    spec: Arc<OpChainSpec>,
    execution_context: OpBlockExecutionCtx,
    evm_env: EvmEnv<OpSpecId>,
    db_factory: &TemporalDbFactory<DBRef>,
    bundle_state: BundleState,
) -> Result<ParalleExecutionResult, BalExecutorError>
where
    DBRef: DatabaseRef + Clone + Send + Sync + std::fmt::Debug + 'static,
    R: OpReceiptBuilder<Receipt = OpReceipt, Transaction = OpTransactionSigned>
        + Send
        + Sync
        + Clone,
    OpTransaction<TxEnv>:
        FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
{
    let temporal_db = db_factory.db(index as u64);

    let state = State::builder()
        .with_database_ref(temporal_db.clone())
        .with_bundle_prestate(bundle_state.clone()) // TODO: Switch to BundleDb
        .with_bundle_update()
        .build();

    let dummy = State::builder()
        .with_database_ref(temporal_db.clone())
        .with_bundle_prestate(bundle_state.clone())
        .with_bundle_update()
        .build();

    let mut database = AsyncBalBuilderDb::new(state, dummy);
    database.set_index(index);

    let evm = OpEvmFactory::default().create_evm(database, evm_env);

    let mut executor = OpBlockExecutor::new(
        evm,
        execution_context.clone(),
        spec.clone(),
        receipt_builder.clone(),
    );

    executor
        .execute_transaction_with_commit_condition(tx.as_executable(), |_| CommitChanges::Yes)
        .map_err(BalExecutorError::BlockExecutionError)?;

    let (evm, result) = executor
        .finish()
        .map_err(BalExecutorError::BlockExecutionError)?;

    let fees = if tx.is_deposit() {
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

    let access_list = db.finish().map_err(|e| BalExecutorError::other(e))?;

    Ok(ParalleExecutionResult {
        transitions: transitions.transitions,
        receipts: result.receipts,
        gas_used: result.gas_used,
        fees,
        access_list,
        index,
    })
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
                .map_err(BalExecutorError::other)?;

            let recovered = tx_envelope
                .try_clone_into_recovered()
                .map_err(BalExecutorError::other)?;

            Ok((start_index + i as BlockAccessIndex, recovered))
        })
        .collect()
}
