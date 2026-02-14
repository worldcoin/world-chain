use std::{borrow::Cow, sync::Arc};

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
use rayon::iter::IntoParallelIterator;
use reth::revm::database::StateProviderDatabase;
use reth_primitives::transaction::SignedTransaction;

use reth_evm::{
    Evm, EvmEnv, EvmEnvFor, EvmFactory, FromRecoveredTx, FromTxWithEncoded,
    block::{BlockExecutionError, BlockExecutor, CommitChanges, InternalBlockExecutionError},
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
use reth_provider::{BlockExecutionOutput, StateProvider, StateProviderFactory};
use reth_trie_common::{HashedPostState, KeccakKeyHasher, updates::TrieUpdates};
use revm::{
    DatabaseRef,
    context::{BlockEnv, TxEnv, result::ExecutionResult},
    database::{BundleAccount, BundleState},
};
use tracing::{error, info, trace};

use crate::{
    access_list::{BlockAccessIndex, FlashblockAccessListConstruction},
    bal_executor::{BalExecutorError, BalValidationError, CommittedState},
    database::{
        bal_builder_db::{BalBuilderDb, NoOpCommitDB},
        bundle_db::BundleDb,
        temporal_db::{TemporalDb, TemporalDbFactory},
    },
};

/// A type alias for the BAL builder database with a cache layer.
pub type ValidatorDb<'a, DB> = BalBuilderDb<&'a mut NoOpCommitDB<TemporalDb<DB>>>;

pub struct FlashblocksBlockValidator<R: OpReceiptBuilder + Default> {
    pub chain_spec: Arc<OpChainSpec>,
    pub committed_state: CommittedState<R>,
    pub evm_env: EvmEnvFor<OpEvmConfig>,
    pub evm_config: OpEvmConfig,
    pub execution_context: OpBlockExecutionCtx,
    pub executor_transactions: Vec<(BlockAccessIndex, Recovered<OpTransactionSigned>)>,
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
        client: impl StateProviderFactory + Clone,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent: &SealedHeader<Header>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError> {
        let FlashblockAccessListData {
            access_list,
            access_list_hash,
        } = diff
            .access_list_data
            .ok_or(BalValidationError::MissingAccessListData.boxed())
            .map_err(BalExecutorError::from)?;

        // 1. Setup database layers for the base evm/executor
        let state_provider_ref = client
            .state_by_block_hash(parent.hash())
            .map_err(BalExecutorError::other)?;
        let state_provider_database = StateProviderDatabase::new(state_provider_ref.as_ref());
        let block_access_index = access_list.min_tx_index;

        // 2. Create channel for state root computation
        let (state_root_sender, state_root_receiver) = crossbeam_channel::bounded(1);

        let mut bundle_state = self.committed_state.bundle.clone();

        let bundle_database =
            BundleDb::new(state_provider_database.clone(), bundle_state.clone().into());

        let temporal_db_factory = TemporalDbFactory::new(bundle_database, &access_list);
        let temporal_db = temporal_db_factory.db(block_access_index as u64);

        // The cumulative bundle for state root computation contains all state changes
        // from prior flashblocks plus the current access list.
        access_list
            .extend_bundle(&mut bundle_state, &state_provider_database)
            .map_err(BalExecutorError::other)?;

        let mut state = NoOpCommitDB::new(temporal_db);

        let mut database = BalBuilderDb::new(&mut state);
        database.set_index(block_access_index);

        let bundle_clone = bundle_state.clone();

        let state_provider = client
            .state_by_block_hash(parent.hash())
            .map_err(BalExecutorError::other)?;

        // 3. Spawn the state root computation in a separate thread
        rayon::spawn(move || {
            let result = compute_state_root(state_provider.into(), &bundle_clone.state);
            let _ = state_root_sender.send(result);
        });

        let evm = OpEvmFactory::default().create_evm(database, self.evm_env.clone());

        let mut executor = OpBlockExecutor::new(
            evm,
            self.execution_context.clone(),
            self.chain_spec.clone(),
            R::default(),
        );

        executor.gas_used = self.committed_state.gas_used;
        executor.receipts = self.committed_state.receipts_iter().cloned().collect();

        let (validator, access_list_receiver) = BalBlockValidator::new(
            self.execution_context.clone(),
            parent,
            executor,
            bundle_state.clone().into(),
            self.committed_state.transactions_iter().cloned().collect(),
            self.chain_spec.clone(),
            &temporal_db_factory,
            state_root_receiver,
            self.evm_env.clone(),
            (access_list.min_tx_index, access_list.max_tx_index),
        );

        let state_provider = client
            .state_by_block_hash(parent.hash())
            .map_err(BalExecutorError::other)?;

        // 4. Compute the block using BAL in parallel
        let (outcome, fees): (BlockBuilderOutcome<OpPrimitives>, u128) =
            validator.execute_block(state_provider.as_ref(), self.executor_transactions.clone())?;

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
                block_number = outcome.block.number(),
                block_hash = ?outcome.block.hash(),
                execution_context = ?self.execution_context,
                "Access list hash mismatch"
            );

            return Err(BalValidationError::BalHashMismatch {
                expected: access_list_hash,
                got: computed_access_list_hash,
                expected_bal: access_list,
                got_bal: computed_access_list,
            }
            .boxed()
            .into());
        }

        if outcome.block.receipts_root != diff.receipts_root {
            error!(
                target: "flashblocks::state_executor",
                got = ?outcome.block.receipts_root,
                expected = ?diff.receipts_root,
                "Receipts root mismatch"
            );

            return Err(BalValidationError::ReceiptsRootMismatch {
                expected: diff.receipts_root,
                got: outcome.block.receipts_root,
            }
            .boxed()
            .into());
        }

        if outcome.block.state_root != diff.state_root {
            error!(
                target: "flashblocks::state_executor",
                got = ?outcome.block.state_root,
                expected = ?diff.state_root,
                expected_bundle = ?bundle_state,
                "State root mismatch"
            );

            return Err(BalValidationError::StateRootMismatch {
                expected: diff.state_root,
                got: outcome.block.state_root,
                bundle_state: bundle_state.clone(),
            }
            .boxed()
            .into());
        }

        if outcome.block.hash() != diff.block_hash {
            error!(
                target: "flashblocks::state_executor",
                got = ?outcome.block.hash(),
                expected = ?diff.block_hash,
                "Block hash mismatch"
            );

            return Err(BalValidationError::BalHashMismatch {
                expected: diff.block_hash,
                got: outcome.block.hash(),
                expected_bal: access_list,
                got_bal: computed_access_list,
            }
            .boxed()
            .into());
        }

        // 6. Seal the block
        let BlockBuilderOutcome {
            execution_result,
            block,
            hashed_state,
            trie_updates,
        } = outcome;

        let sealed_block = Arc::new(block.sealed_block().clone());

        let execution_output = BlockExecutionOutput {
            state: bundle_state.clone(),
            result: execution_result,
        };

        let executed_block: BuiltPayloadExecutedBlock<OpPrimitives> = BuiltPayloadExecutedBlock {
            recovered_block: Arc::new(block),
            execution_output: Arc::new(execution_output),
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
pub struct BalBlockValidator<'a, DbRef: DatabaseRef + 'a, R: OpReceiptBuilder, Evm> {
    pub inner: BasicBlockBuilder<
        'a,
        OpBlockExecutorFactory<OpRethReceiptBuilder, OpChainSpec>,
        OpBlockExecutor<Evm, R, Arc<OpChainSpec>>,
        OpBlockAssembler<OpChainSpec>,
        OpPrimitives,
    >,
    pub bundle_state: Arc<BundleState>,
    pub access_list_sender: crossbeam_channel::Sender<FlashblockAccessList>,
    pub state_root_receiver:
        crossbeam_channel::Receiver<Result<StateRootResult, BlockExecutionError>>,
    pub temporal_db_factory: &'a TemporalDbFactory<DbRef>,
    pub evm_env: EvmEnv<OpSpecId>,
    pub index_range: (u16, u16),
}

impl<'a, DBRef, R, E> BalBlockValidator<'a, DBRef, R, E>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt>,
    DBRef: DatabaseRef + Clone + std::fmt::Debug + 'a,
    E: Evm<
            DB = ValidatorDb<'a, DBRef>,
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
        bundle_state: Arc<BundleState>,
        transactions: Vec<Recovered<OpTransactionSigned>>,
        chain_spec: Arc<OpChainSpec>,
        temporal_db_factory: &'a TemporalDbFactory<DBRef>,
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
                bundle_state,
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
        db.db_mut().db_mut().set_index(index as u64);
        // Set the bal builder db index
        db.set_index(index);
        Ok(())
    }
}

impl<'a, DB, R, E> BlockBuilder for BalBlockValidator<'a, DB, R, E>
where
    DB: DatabaseRef + Clone + std::fmt::Debug + 'a,
    E: Evm<
            DB = ValidatorDb<'a, DB>,
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
        self,
        state: impl StateProvider,
    ) -> Result<BlockBuilderOutcome<OpPrimitives>, BlockExecutionError> {
        let (evm, result) = self.inner.executor.finish()?;
        let (db, evm_env) = evm.finish();

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
            Cow::Borrowed(self.bundle_state.as_ref()),
            &state,
            state_root,
        ))?;

        let block = RecoveredBlock::new_unhashed(block, senders);

        let access_list = db
            .finish()
            .map_err(InternalBlockExecutionError::other)?
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
    DbRef: DatabaseRef + Clone + std::fmt::Debug + 'a,
    E: Evm<
            DB = ValidatorDb<'a, DbRef>,
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
        if self.index_range.0 == 0 {
            self.prepare_database(0)?;
            self.apply_pre_execution_changes()?;
        }

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

        // Execute each transaction in parallel over a temporal view of the database
        // formed from the BAL provided. We reduce the results to aggregate the state transitions,
        // receipts, gas used, and access list. Then pre-load the aggregated results into the base
        // executor to finalize the block.
        let mut results = transactions
            .clone()
            // .into_par_iter()
            // TODO: get rayon to work
            .into_iter()
            .map(|(index, tx)| {
                let tx = tx.clone();
                info!(
                    "Executing tx at index {} hash {}",
                    index,
                    tx.clone().into_encoded().encoded_bytes().clone()
                );
                execute_transaction(
                    (index, tx),
                    receipt_builder.clone(),
                    spec.clone(),
                    execution_context.clone(),
                    evm_env.clone(),
                    db_factory,
                )
            })
            .collect::<Result<Vec<_>, BalExecutorError>>()?;

        // Sort results by transaction ascending index
        results.sort_unstable_by_key(|r| r.index);

        let merged_result = merge_transaction_results(results, gas_used);
        let database = self.inner.executor_mut().evm_mut().db_mut();

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

        debug_assert_eq!(
            merged_result.index + 1,
            self.index_range.1,
            "Final transaction index should match the expected range"
        );

        // finalize the database index
        self.prepare_database(self.index_range.1)?;

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
            acc.gas_used += res.gas_used;
            if let Some(mut receipt) = res.receipts.pop() {
                receipt.as_receipt_mut().cumulative_gas_used = acc.gas_used;
                acc.receipts.push(receipt);
            }

            acc.access_list.merge(res.access_list);
            acc.fees += res.fees;
            acc.index = acc.index.max(res.index);
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
) -> Result<ParalleExecutionResult, BalExecutorError>
where
    DBRef: DatabaseRef + Clone + std::fmt::Debug,
    R: OpReceiptBuilder<Receipt = OpReceipt, Transaction = OpTransactionSigned>
        + Send
        + Sync
        + Clone,
    OpTransaction<TxEnv>:
        FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
{
    let temporal_db = db_factory.db(index as u64);
    let state = NoOpCommitDB::new(temporal_db);
    let mut database = BalBuilderDb::new(state);

    database.set_index(index);

    let evm = OpEvmFactory::default().create_evm(database, evm_env);

    let mut executor = OpBlockExecutor::new(
        evm,
        execution_context.clone(),
        spec.clone(),
        receipt_builder.clone(),
    );

    let res = executor
        .execute_transaction_with_commit_condition(&tx, |_| CommitChanges::Yes)
        .map_err(BalExecutorError::BlockExecutionError);

    trace!(
        target: "flashblocks::builder::block_validator",
        ?res,
        tx_index = index,
        tx_hash = ?tx.hash(),
        transaction = ?tx,
        "Finished executing tx in parallel"
    );

    let (evm, result) = executor
        .finish()
        .map_err(BalExecutorError::BlockExecutionError)?;

    let fees = if tx.is_deposit() {
        0
    } else {
        tx.effective_tip_per_gas(evm.block().basefee)
            .expect("fee is always valid; execution succeeded")
    };

    let (db, _evm_env) = evm.finish();
    let access_list = db.finish().map_err(BalExecutorError::other)?;

    Ok(ParalleExecutionResult {
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
    state_provider: Arc<dyn StateProvider + Send>,
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
pub fn decode_transactions_with_indices(
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
