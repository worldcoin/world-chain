use alloy_consensus::{Header, Transaction};
use alloy_eips::Decodable2718;
use op_revm::{OpHaltReason, OpSpecId};
use reth_primitives_traits::{Recovered, RecoveredBlock, SealedHeader, SignerRecoverable};
use std::{io::Error, marker::PhantomData, sync::Arc, time::Instant};

use alloy_primitives::Bytes;

use alloy_op_evm::{
    OpBlockExecutionCtx, OpBlockExecutor, OpBlockExecutorFactory, OpEvmFactory, OpTx,
    block::receipt_builder::OpReceiptBuilder,
};
use alloy_rpc_types_engine::PayloadId;
use eyre::eyre::bail;
use op_alloy_consensus::OpReceipt;
use rayon::{iter::IntoParallelIterator, prelude::ParallelIterator};
use reth_chain_state::ExecutedBlock;
use reth_revm::database::StateProviderDatabase;
use world_chain_primitives::{
    access_list::FlashblockAccessList, primitives::ExecutionPayloadFlashblockDeltaV1,
};

use reth_evm::{
    ConfigureEvm, Evm, EvmEnv, EvmEnvFor, EvmFactory, FromRecoveredTx, FromTxWithEncoded,
    block::{BlockExecutionError, BlockExecutor, CommitChanges, InternalBlockExecutionError},
    execute::{
        BasicBlockBuilder, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome, ExecutorTx,
    },
};
use reth_node_api::BuiltPayload;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpBlockAssembler, OpRethReceiptBuilder};
use reth_optimism_node::OpBuiltPayload;
use reth_optimism_primitives::{OpPrimitives, OpTransactionSigned};
use reth_provider::{StateProvider, StateProviderFactory};
use revm::{
    DatabaseRef,
    context::{BlockEnv, result::ExecutionResult},
    database::BundleState,
};
use tracing::{error, info, trace};

use crate::state_root_strategy::{FlashblockTypes, StateRootHandle};
use crate::{
    access_list::{BlockAccessIndex, FlashblockAccessListConstruction},
    bal_executor::{BalExecutorError, CommittedState},
    database::{
        bal_builder_db::{BalBuilderDb, NoOpCommitDB},
        bundle_db::BundleDb,
        temporal_db::{TemporalDb, TemporalDbFactory},
    },
    execution_strategy::{ExecutionStrategy, StateRootResult, ValidationCtx},
    flashblock_validation_metrics::{
        FlashblockValidationAttemptMetrics, FlashblockValidationMetrics,
    },
    metrics::PayloadBuildStage,
    payload_builder_metrics::metered_fn,
};

/// A type alias for the BAL builder database with a cache layer.
pub type ValidatorDb<'a, DB> = BalBuilderDb<&'a mut NoOpCommitDB<TemporalDb<DB>>>;

pub struct FlashblocksBlockValidator<Evm: ConfigureEvm, T: FlashblockTypes<Evm>> {
    pub chain_spec: Arc<OpChainSpec>,
    pub evm_env: EvmEnvFor<Evm>,
    pub evm_config: Evm,
    pub execution_context: OpBlockExecutionCtx,
    pub header: Arc<SealedHeader>,
    pub flashblock_validation_metrics: Arc<FlashblockValidationMetrics>,
    pub execution_strategy: T::Execution,
    _phantom: PhantomData<fn() -> T>,
}

impl<Evm: ConfigureEvm + Clone, T: FlashblockTypes<Evm>> FlashblocksBlockValidator<Evm, T> {
    pub fn new(
        chain_spec: Arc<OpChainSpec>,
        evm_env: EvmEnvFor<Evm>,
        evm_config: Evm,
        execution_context: OpBlockExecutionCtx,
        header: Arc<SealedHeader>,
        flashblock_validation_metrics: Arc<FlashblockValidationMetrics>,
        execution_strategy: T::Execution,
    ) -> Self {
        Self {
            chain_spec,
            evm_env,
            evm_config,
            execution_context,
            header,
            flashblock_validation_metrics,
            execution_strategy,
            _phantom: PhantomData,
        }
    }

    pub fn validate_flashblock_with_state(
        &self,
        client: impl StateProviderFactory + Clone + Sync + 'static,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent: &SealedHeader<Header>,
        payload_id: PayloadId,
        state: Option<&OpBuiltPayload<OpPrimitives>>,
    ) -> Result<OpBuiltPayload, BalExecutorError> {
        let has_bal = diff.access_list_data.is_some();
        let diff_clone = diff.clone();
        let validation_started = Instant::now();
        let mut attempt_metrics = FlashblockValidationAttemptMetrics::default();

        let result = (|| -> Result<OpBuiltPayload, BalExecutorError> {
            let committed_state = CommittedState::<OpRethReceiptBuilder>::try_from(state)?;

            let next_payload = metered_fn(
                tracing::trace_span!(
                    target: "flashblocks::coordinator",
                    "validate",
                    id = %payload_id,
                    path = if has_bal { "bal" } else { "legacy" },
                    tx_count = diff.transactions.len(),
                    duration_ms = tracing::field::Empty,
                ),
                metrics::histogram!("flashblocks.validate", "access_list" => has_bal.to_string()),
                |_span| {
                    self.execution_strategy.execute(
                        ValidationCtx {
                            parent,
                            attempt_metrics: &mut attempt_metrics,
                            chain_spec: self.chain_spec.clone(),
                            evm_env: self.evm_env.clone(),
                            evm_config: self.evm_config.clone(),
                            execution_context: self.execution_context.clone(),
                            header: self.header.clone(),
                        },
                        client,
                        diff,
                        committed_state,
                        payload_id,
                    )
                },
            )?;

            let executed = next_payload
                .executed_block()
                .ok_or(BalExecutorError::MissingExecutedBlock)
                .inspect_err(|e| {
                    error!(
                        target: "flashblocks::coordinator",
                        ?payload_id,
                        error = ?e,
                        "Missing executed block for payload"
                    )
                })?;

            self.validate_payload(&executed.into_executed_payload(), diff_clone)
                .map_err(|e| BalExecutorError::Other(Box::from(e.to_string())))
                .inspect_err(|e| {
                    error!(
                        target: "flashblocks::coordinator",
                        ?payload_id,
                        error = ?e,
                        "Payload validation failed"
                    )
                })?;

            Ok(next_payload)
        })();

        match result {
            Ok(next_payload) => {
                attempt_metrics
                    .record_stage_duration(PayloadBuildStage::Total, validation_started.elapsed());
                attempt_metrics.publish(self.flashblock_validation_metrics.as_ref());
                Ok(next_payload)
            }
            Err(err) => {
                self.flashblock_validation_metrics
                    .increment_validation_errors();
                Err(err)
            }
        }
    }

    pub fn validate_payload(
        &self,
        block: &ExecutedBlock<OpPrimitives>,
        diff: ExecutionPayloadFlashblockDeltaV1,
    ) -> eyre::Result<()> {
        // make sure the block matches the diff
        if block.sealed_block().hash() != diff.block_hash {
            bail!(
                "Block hash mismatch: expected {}, got {}",
                diff.block_hash,
                block.sealed_block().hash()
            );
        }

        Ok(())
    }
}

/// Result of executing a single transaction in parallel.
///
/// This struct accumulates execution artifacts that will be merged
/// after all transactions complete.
#[derive(Default)]
pub struct ParallelExecutionResult {
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
pub struct BalBlockValidator<
    'a,
    DbRef: DatabaseRef + 'a,
    R: OpReceiptBuilder,
    Evm,
    H: StateRootHandle,
> {
    pub inner: BasicBlockBuilder<
        'a,
        OpBlockExecutorFactory<OpRethReceiptBuilder, OpChainSpec>,
        OpBlockExecutor<Evm, R, Arc<OpChainSpec>>,
        OpBlockAssembler<OpChainSpec>,
        OpPrimitives,
    >,
    pub bundle_state: Arc<BundleState>,
    pub fallback_bundle_state: Arc<BundleState>,
    pub access_list_sender: crossbeam_channel::Sender<FlashblockAccessList>,
    pub state_root_handle: H,
    pub temporal_db_factory: &'a TemporalDbFactory,
    pub evm_env: EvmEnv<OpSpecId>,
    pub index_range: (u16, u16),
    pub attempt_metrics: &'a mut FlashblockValidationAttemptMetrics,
    _db_ref: PhantomData<DbRef>,
}

impl<'a, DBRef, R, E, H> BalBlockValidator<'a, DBRef, R, E, H>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt>,
    DBRef: DatabaseRef + Clone + std::fmt::Debug + 'a,
    E: Evm<
            DB = ValidatorDb<'a, DBRef>,
            Tx = OpTx,
            Spec = OpSpecId,
            BlockEnv = BlockEnv,
        >,
    H: StateRootHandle,
    OpTx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
{
    /// Creates a new [`FlashblocksBlockBuilder`] with the given executor factory and assembler.
    pub fn new(
        ctx: OpBlockExecutionCtx,
        parent: &'a SealedHeader<Header>,
        executor: OpBlockExecutor<E, R, Arc<OpChainSpec>>,
        bundle_state: Arc<BundleState>,
        fallback_bundle_state: Arc<BundleState>,
        transactions: Vec<Recovered<OpTransactionSigned>>,
        chain_spec: Arc<OpChainSpec>,
        temporal_db_factory: &'a TemporalDbFactory,
        state_root_handle: H,
        evm_env: EvmEnv<OpSpecId>,
        index_range: (u16, u16),
        attempt_metrics: &'a mut FlashblockValidationAttemptMetrics,
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
                fallback_bundle_state,
                access_list_sender: tx,
                state_root_handle,
                temporal_db_factory,
                evm_env,
                index_range,
                attempt_metrics,
                _db_ref: PhantomData,
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

impl<'a, DB, R, E, H> BlockBuilder for BalBlockValidator<'a, DB, R, E, H>
where
    DB: DatabaseRef + Clone + std::fmt::Debug + 'a,
    E: Evm<
            DB = ValidatorDb<'a, DB>,
            Tx = OpTx,
            Spec = OpSpecId,
            HaltReason = OpHaltReason,
            BlockEnv = BlockEnv,
        >,
    R: OpReceiptBuilder<Receipt = OpReceipt, Transaction = OpTransactionSigned>,
    H: StateRootHandle,
    OpTx: FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
{
    type Primitives = OpPrimitives;
    type Executor = OpBlockExecutor<E, R, Arc<OpChainSpec>>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.inner.executor.apply_pre_execution_changes()
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutorTx<Self::Executor>,
        f: impl FnOnce(
            &ExecutionResult<<<Self::Executor as BlockExecutor>::Evm as Evm>::HaltReason>,
        ) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        let (tx_env, tx) = tx.into_parts();
        if let Some(gas_used) = self
            .inner
            .executor
            .execute_transaction_with_commit_condition((tx_env, &tx), f)?
        {
            self.inner.transactions.push(tx);
            Ok(Some(gas_used))
        } else {
            Ok(None)
        }
    }

    fn finish(
        mut self,
        state: impl StateProvider,
        _state_root_precomputed: Option<(
            alloy_primitives::B256,
            reth_trie_common::updates::TrieUpdates,
        )>,
    ) -> Result<BlockBuilderOutcome<OpPrimitives>, BlockExecutionError> {
        let finalize_started = Instant::now();
        // finalize the database index
        self.prepare_database(self.index_range.1)?;

        let (evm, result) = self.inner.executor.finish()?;
        let (db, evm_env) = evm.finish();

        // Wait for the state root result from the async computation
        let state_root_started = Instant::now();
        let StateRootResult {
            state_root,
            trie_updates,
            hashed_state,
        } = self.state_root_handle.finish()?;
        self.attempt_metrics
            .record_stage_duration(PayloadBuildStage::StateRoot, state_root_started.elapsed());

        let (transactions, senders) = self
            .inner
            .transactions
            .into_iter()
            .map(|tx| tx.into_parts())
            .unzip();

        let block_assembly_started = Instant::now();
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
            self.bundle_state.as_ref(),
            &state,
            state_root,
        ))?;
        self.attempt_metrics.record_stage_duration(
            PayloadBuildStage::BlockAssembly,
            block_assembly_started.elapsed(),
        );

        let block = RecoveredBlock::new_unhashed(block, senders);

        let access_list = db
            .finish()
            .map_err(InternalBlockExecutionError::other)?
            .build(self.index_range);

        self.access_list_sender
            .send(access_list)
            .map_err(BlockExecutionError::other)?;

        self.attempt_metrics
            .record_stage_duration(PayloadBuildStage::Finalize, finalize_started.elapsed());

        Ok(BlockBuilderOutcome {
            execution_result: result,
            hashed_state,
            trie_updates,
            block,
        })
    }

    fn executor_mut(&mut self) -> &mut Self::Executor {
        &mut self.inner.executor
    }

    fn executor(&self) -> &Self::Executor {
        &self.inner.executor
    }

    fn into_executor(self) -> Self::Executor {
        self.inner.executor
    }
}

impl<'a, DbRef, R, E, H> BalBlockValidator<'a, DbRef, R, E, H>
where
    DbRef: DatabaseRef + Clone + std::fmt::Debug + 'a,
    E: Evm<
            DB = ValidatorDb<'a, DbRef>,
            Tx = OpTx,
            Spec = OpSpecId,
            HaltReason = OpHaltReason,
            BlockEnv = BlockEnv,
        >,
    R: OpReceiptBuilder<Receipt = OpReceipt, Transaction = OpTransactionSigned>
        + Send
        + Sync
        + Clone,
    H: StateRootHandle,
    OpTx: FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
{
    pub fn execute_block<Client>(
        mut self,
        client: Client,
        state_provider: impl StateProvider + Clone,
        transactions: impl IntoParallelIterator<
            Item = (BlockAccessIndex, Recovered<OpTransactionSigned>),
        > + IntoIterator<Item = (BlockAccessIndex, Recovered<OpTransactionSigned>)>
        + Clone,
    ) -> Result<(BlockBuilderOutcome<OpPrimitives>, u128), BalExecutorError>
    where
        Client: StateProviderFactory + Clone + Sync,
    {
        if self.index_range.0 == 0 {
            self.prepare_database(0)?;
            let pre_execution_changes_started = Instant::now();
            self.apply_pre_execution_changes()?;
            self.attempt_metrics.record_stage_duration(
                PayloadBuildStage::PreExecutionChanges,
                pre_execution_changes_started.elapsed(),
            );
        }

        let spec = self.inner.executor.spec.clone();
        let receipt_builder: R = self.inner.executor.receipt_builder.clone();
        let execution_context = self.inner.ctx.clone();
        let evm_env = self.evm_env.clone();
        let gas_used = self.inner.executor.gas_used;

        let db_factory = self.temporal_db_factory.clone();
        let fallback_bundle_state = self.fallback_bundle_state.clone();
        let parent_hash = self.inner.parent.hash();

        trace!(
            target: "flashblocks::builder::block_validator",
            tx_count = transactions.clone().into_iter().count(),
            "Starting parallel block execution"
        );

        // Execute each transaction in parallel over a temporal view of the database
        // formed from the BAL provided. We reduce the results to aggregate the state transitions,
        // receipts, gas used, and access list. Then pre-load the aggregated results into the base
        // executor to finalize the block.
        let txs_execution_started = Instant::now();
        let mut results = transactions
            .clone()
            .into_par_iter()
            // StateProviderBox is Send but not Sync/Clone. map_init gives each Rayon worker
            // its own provider, so fallback reads do not go through a shared lock.
            .map_init(
                || client.state_by_block_hash(parent_hash),
                |state_provider, (index, tx)| {
                    let state_provider = state_provider.as_ref().map_err(|err| {
                        BalExecutorError::other(Error::other(format!(
                            "failed to create worker state provider: {err}"
                        )))
                    })?;

                    let state_provider_database =
                        StateProviderDatabase::new(state_provider.as_ref());
                    let bundle_database =
                        BundleDb::new(state_provider_database, fallback_bundle_state.clone());

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
                        &db_factory,
                        bundle_database,
                    )
                },
            )
            .collect::<Result<Vec<_>, BalExecutorError>>()?;
        // Sort results by transaction ascending index
        results.sort_unstable_by_key(|r| r.index);
        self.attempt_metrics.record_stage_duration(
            PayloadBuildStage::SequencerTxExecution,
            txs_execution_started.elapsed(),
        );

        let merged_result = merge_transaction_results(results, gas_used);
        let database = self.inner.executor.evm_mut().db_mut();

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

        Ok((self.finish(state_provider, None)?, merged_result.fees))
    }
}

/// Merge individual transaction results into an aggregated result.
fn merge_transaction_results(
    results: Vec<ParallelExecutionResult>,
    initial_gas_used: u64,
) -> ParallelExecutionResult {
    results.into_iter().fold(
        ParallelExecutionResult {
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
    db_factory: &TemporalDbFactory,
    db: DBRef,
) -> Result<ParallelExecutionResult, BalExecutorError>
where
    DBRef: DatabaseRef + std::fmt::Debug,
    R: OpReceiptBuilder<Receipt = OpReceipt, Transaction = OpTransactionSigned>
        + Send
        + Sync
        + Clone,
    OpTx: FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
{
    let temporal_db = db_factory.db(db, index as u64);
    let state = NoOpCommitDB::new(temporal_db);
    let mut database = BalBuilderDb::new(state);

    database.set_index(index);

    let evm = OpEvmFactory::<OpTx>::default().create_evm(database, evm_env);

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

    Ok(ParallelExecutionResult {
        receipts: result.receipts,
        gas_used: result.gas_used,
        fees,
        access_list,
        index,
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

            let signer = tx_envelope
                .recover_signer()
                .map_err(BalExecutorError::other)?;
            let recovered = Recovered::new_unchecked(tx_envelope, signer);

            Ok((start_index + i as BlockAccessIndex, recovered))
        })
        .collect()
}
