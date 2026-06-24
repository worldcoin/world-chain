use std::{io::Error, sync::Arc, time::Instant};

use alloy_consensus::{Header, Transaction};
use alloy_eip7928::BlockAccessIndex;
use alloy_op_evm::{
    OpBlockExecutionCtx, OpBlockExecutor, OpBlockExecutorFactory, OpEvmFactory, OpTx,
};
use alloy_primitives::{B256, U256};
use alloy_rpc_types_engine::PayloadId;
use rayon::prelude::*;
use reth_evm::{
    ConfigureEvm, Evm, EvmEnvFor, EvmFactory,
    block::{BlockExecutionError, CommitChanges},
    execute::{BlockAssemblerInput, BlockBuilder, BlockExecutor, BlockExecutorFactory},
};
use reth_node_api::BuiltPayloadExecutedBlock;
use reth_optimism_evm::{OpBlockAssembler, OpRethReceiptBuilder};
use reth_optimism_node::OpBuiltPayload;
use reth_optimism_primitives::{OpPrimitives, OpTransactionSigned};
use reth_primitives_traits::{Recovered, RecoveredBlock, SealedHeader};
use reth_provider::{BlockExecutionOutput, StateProviderFactory};
use reth_revm::database::StateProviderDatabase;
use reth_trie_common::{HashedPostState, KeccakKeyHasher, updates::TrieUpdates};
use revm::{database::states::bundle_state::BundleRetention, state::bal::Bal};
use revm_database::State;
use tracing::error;
use world_chain_primitives::{
    access_list::{FlashblockAccessList, FlashblockAccessListData, access_list_hash},
    primitives::ExecutionPayloadFlashblockDeltaV1,
};

use crate::{
    flashblock_validation_metrics::FlashblockValidationAttemptMetrics,
    state_root_strategy::{StateRootHandle, StateRootStrategy},
    validator::decode_transactions_with_indices,
};
use world_chain_chainspec::WorldChainSpec;
use world_chain_evm::{
    PayloadBuildStage, WorldChainEvmConfig,
    execution::{
        bal::{BalExecutorError, BalValidationError, CommittedState, record_op_l1_block_bal_reads},
        basic::FlashblocksBlockBuilder,
    },
    utils::{cache_prestate_from_bundle, extend_flashblock_bundle, flatten_reverts},
};

struct BalWorkerOutput<F: BlockExecutorFactory> {
    index: u64,
    transaction: Recovered<F::Transaction>,
    result: F::TxExecutionResult,
}

/// Result of computing the state root from a bundle state.
pub struct StateRootResult {
    pub state_root: B256,
    pub trie_updates: TrieUpdates,
    pub hashed_state: HashedPostState,
}

/// Context passed to an [`ExecutionStrategy`] for each flashblock.
pub struct ValidationCtx<'a, Evm: ConfigureEvm, S: StateRootStrategy> {
    pub parent: &'a SealedHeader<Header>,
    pub attempt_metrics: &'a mut FlashblockValidationAttemptMetrics,
    pub chain_spec: Arc<WorldChainSpec>,
    pub evm_env: EvmEnvFor<Evm>,
    pub execution_context: OpBlockExecutionCtx,
    /// State root strategy for this flashblock execution.
    pub state_root_strategy: S,
}

/// Strategy for executing flashblock diff transactions.
///
/// The `S` parameter is the [`StateRootStrategy`] paired with this execution
/// strategy via [`FlashblockTypes`].
pub trait ExecutionStrategy<Evm: ConfigureEvm, S: StateRootStrategy>: Send + Sync {
    fn execute(
        &self,
        ctx: ValidationCtx<'_, Evm, S>,
        client: impl StateProviderFactory + Clone + Sync + 'static,
        diff: ExecutionPayloadFlashblockDeltaV1,
        committed_state: CommittedState<OpRethReceiptBuilder>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError>;
}

/// BAL execution strategy: parallel transaction execution with an external state root handle.
pub struct FlashblocksBalExecutionStrategy;

impl<S: StateRootStrategy> ExecutionStrategy<WorldChainEvmConfig, S>
    for FlashblocksBalExecutionStrategy
{
    fn execute(
        &self,
        ctx: ValidationCtx<'_, WorldChainEvmConfig, S>,
        client: impl StateProviderFactory + Clone + Sync + 'static,
        diff: ExecutionPayloadFlashblockDeltaV1,
        committed_state: CommittedState<OpRethReceiptBuilder>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError> {
        BalBlockValidator::new(ctx).execute_block(client, diff, committed_state, payload_id)
    }
}

/// Validator for BAL-backed flashblock execution.
struct BalBlockValidator<'a, S: StateRootStrategy> {
    ctx: ValidationCtx<'a, WorldChainEvmConfig, S>,
}

impl<'a, S: StateRootStrategy> BalBlockValidator<'a, S> {
    fn new(ctx: ValidationCtx<'a, WorldChainEvmConfig, S>) -> Self {
        Self { ctx }
    }

    fn executor_factory(
        &self,
    ) -> OpBlockExecutorFactory<OpRethReceiptBuilder, Arc<WorldChainSpec>> {
        OpBlockExecutorFactory::new(
            OpRethReceiptBuilder::default(),
            self.ctx.chain_spec.clone(),
            OpEvmFactory::<OpTx>::default(),
        )
    }

    fn validate_min_tx_index(
        &self,
        min_tx_index: u64,
        transactions_offset: u64,
        committed_state: &CommittedState<OpRethReceiptBuilder>,
    ) -> Result<(), BalExecutorError> {
        let expected_min_tx_index = if committed_state.is_first {
            0
        } else {
            transactions_offset
        };

        if min_tx_index != expected_min_tx_index {
            return Err(BalValidationError::BalIndexMismatch {
                index: "min",
                expected: expected_min_tx_index,
                got: min_tx_index,
            }
            .boxed()
            .into());
        }

        Ok(())
    }

    fn validate_max_tx_index(
        &self,
        max_tx_index: u64,
        transactions_offset: u64,
        transactions: &[(u64, Recovered<OpTransactionSigned>)],
    ) -> Result<(), BalExecutorError> {
        let expected_max_tx_index = transactions
            .last()
            .map(|(index, _)| index + 1)
            .unwrap_or(transactions_offset);

        if max_tx_index != expected_max_tx_index {
            return Err(BalValidationError::BalIndexMismatch {
                index: "max",
                expected: expected_max_tx_index,
                got: max_tx_index,
            }
            .boxed()
            .into());
        }

        Ok(())
    }

    fn execute_workers<C>(
        &mut self,
        client: &C,
        parent_hash: B256,
        transactions: &[(u64, Recovered<OpTransactionSigned>)],
        received_bal: Arc<Bal>,
        committed_bundle: &revm_database::BundleState,
    ) -> Result<
        Vec<BalWorkerOutput<OpBlockExecutorFactory<OpRethReceiptBuilder, Arc<WorldChainSpec>>>>,
        BalExecutorError,
    >
    where
        C: StateProviderFactory + Clone + Sync + 'static,
    {
        let spec = self.ctx.chain_spec.clone();
        let execution_context = self.ctx.execution_context.clone();
        let evm_env = self.ctx.evm_env.clone();
        let fallback_bundle_state = Arc::new(committed_bundle.clone());

        let txs_execution_started = Instant::now();
        let mut worker_outputs = transactions
            .to_vec()
            .into_par_iter()
            .map_init(
                || client.state_by_block_hash(parent_hash),
                |state_provider, (index, tx)| {
                    let state_provider = state_provider.as_ref().map_err(|err| {
                        BalExecutorError::other(Error::other(format!(
                            "failed to create worker state provider: {err}"
                        )))
                    })?;

                    let mut worker_state = State::builder()
                        .with_database(StateProviderDatabase::new(state_provider.as_ref()))
                        .with_cached_prestate(cache_prestate_from_bundle(&fallback_bundle_state))
                        .with_bundle_update()
                        .with_bal(received_bal.clone())
                        .build();
                    worker_state.set_bal_index(BlockAccessIndex::new(index));

                    let executor_factory = OpBlockExecutorFactory::new(
                        OpRethReceiptBuilder::default(),
                        spec.clone(),
                        OpEvmFactory::<OpTx>::default(),
                    );
                    let evm = executor_factory
                        .evm_factory()
                        .create_evm(&mut worker_state, evm_env.clone());
                    let mut worker_executor =
                        executor_factory.create_executor(evm, execution_context.clone());

                    let result = worker_executor
                        .execute_transaction_without_commit(tx.clone())
                        .map_err(BalExecutorError::BlockExecutionError)?;

                    Ok(BalWorkerOutput {
                        index,
                        transaction: tx,
                        result,
                    })
                },
            )
            .collect::<Result<Vec<_>, BalExecutorError>>()?;

        worker_outputs.sort_unstable_by_key(|output| output.index);

        self.ctx.attempt_metrics.record_stage_duration(
            PayloadBuildStage::SequencerTxExecution,
            txs_execution_started.elapsed(),
        );

        Ok(worker_outputs)
    }

    fn execute_block<C>(
        mut self,
        client: C,
        diff: ExecutionPayloadFlashblockDeltaV1,
        committed_state: CommittedState<OpRethReceiptBuilder>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError>
    where
        C: StateProviderFactory + Clone + Sync + 'static,
    {
        let FlashblockAccessListData {
            access_list,
            access_list_hash: expected_access_list_hash,
        } = diff
            .access_list_data
            .ok_or(BalValidationError::MissingAccessListData.boxed())
            .map_err(BalExecutorError::from)?;

        let min_tx_index = access_list.min_tx_index;
        let max_tx_index = access_list.max_tx_index;
        let transactions_offset = committed_state.transactions.len() as u64 + 1;
        self.validate_min_tx_index(min_tx_index, transactions_offset, &committed_state)?;

        let transactions =
            decode_transactions_with_indices(&diff.transactions, transactions_offset)?;
        self.validate_max_tx_index(max_tx_index, transactions_offset, &transactions)?;

        let received_bal = Arc::new(
            Bal::try_from(access_list.as_block_access_list()).map_err(BalExecutorError::other)?,
        );

        let parent_hash = self.ctx.parent.hash();
        let finish_state_provider = client
            .state_by_block_hash(parent_hash)
            .map_err(BalExecutorError::other)?;

        // Compute the state root from the *received* access list, overlapping the trie walk with
        // parallel transaction execution below. The access list carries the claimed post-state, so
        // deriving the cumulative hashed post-state from it (the committed prior-flashblock state
        // extended with this flashblock's changes) lets the state root computation start before
        // execution finishes. The trie walk is dispatched to a background thread; we block on the
        // handle only after execution and BAL hash validation succeed. A wrong access list yields a
        // mismatching state root, so this stays consensus-safe.
        let state_root_started = Instant::now();
        let committed_hashed_state = HashedPostState::from_bundle_state::<KeccakKeyHasher>(
            committed_state.bundle.state.iter(),
        );
        let predicted_hashed_state = access_list
            .clone()
            .into_hashed_post_state(finish_state_provider.as_ref(), committed_hashed_state)
            .map_err(BalExecutorError::other)?;
        let state_root_handle = self.ctx.state_root_strategy.prepare(
            client.clone(),
            parent_hash,
            predicted_hashed_state,
        )?;

        let mut db = State::builder()
            .with_database(StateProviderDatabase::new(finish_state_provider.as_ref()))
            .with_cached_prestate(cache_prestate_from_bundle(&committed_state.bundle))
            .with_bundle_update()
            .with_bal_builder()
            .build();
        db.set_bal_index(BlockAccessIndex::new(min_tx_index));

        let executor_factory = self.executor_factory();
        let evm = executor_factory
            .evm_factory()
            .create_evm(&mut db, self.ctx.evm_env.clone());
        let mut canonical_executor =
            executor_factory.create_executor(evm, self.ctx.execution_context.clone());

        canonical_executor.gas_used = committed_state.gas_used;
        canonical_executor.da_footprint_used = committed_state.blob_gas_used;
        canonical_executor.receipts = committed_state.receipts_iter().cloned().collect();

        let mut canonical_transactions: Vec<_> =
            committed_state.transactions_iter().cloned().collect();

        if committed_state.is_first {
            let pre_execution_changes_started = Instant::now();
            canonical_executor
                .evm_mut()
                .db_mut()
                .set_bal_index(BlockAccessIndex::new(0));
            canonical_executor.apply_pre_execution_changes()?;
            self.ctx.attempt_metrics.record_stage_duration(
                PayloadBuildStage::PreExecutionChanges,
                pre_execution_changes_started.elapsed(),
            );
        }

        let worker_outputs = self.execute_workers(
            &client,
            parent_hash,
            &transactions,
            received_bal,
            &committed_state.bundle,
        )?;

        let basefee = self.ctx.evm_env.block_env.basefee;
        let mut fees = U256::ZERO;
        for output in worker_outputs {
            canonical_executor
                .evm_mut()
                .db_mut()
                .set_bal_index(BlockAccessIndex::new(output.index));
            if !output.transaction.is_deposit() {
                record_op_l1_block_bal_reads(canonical_executor.evm_mut().db_mut())?;
            }
            let gas_output = canonical_executor.commit_transaction(output.result);

            if !output.transaction.is_deposit() {
                let miner_fee = output
                    .transaction
                    .effective_tip_per_gas(basefee)
                    .expect("fee is always valid; execution succeeded");
                fees += U256::from(gas_output.tx_gas_used()) * U256::from(miner_fee);
            }

            canonical_transactions.push(output.transaction);
        }

        canonical_executor
            .evm_mut()
            .db_mut()
            .set_bal_index(BlockAccessIndex::new(max_tx_index));

        let finalize_started = Instant::now();
        let (evm, execution_result) = canonical_executor.finish()?;
        let (db, evm_env) = evm.finish();

        let built_block_access_list = db
            .take_built_alloy_bal()
            .ok_or_else(|| BlockExecutionError::msg("missing BAL builder state"))?;
        let computed_access_list = FlashblockAccessList::from_block_access_list(
            built_block_access_list,
            (min_tx_index, max_tx_index),
        );
        let computed_access_list_hash = access_list_hash(&computed_access_list);

        if computed_access_list_hash != expected_access_list_hash {
            error!(
                target: "flashblocks::state_executor",
                ?expected_access_list_hash,
                expected = ?expected_access_list_hash,
                got = ?computed_access_list_hash,
                access_list = ?computed_access_list,
                expected_access_list = ?access_list.clone(),
                execution_context = ?self.ctx.execution_context,
                "Access list hash mismatch"
            );
            return Err(BalValidationError::BalHashMismatch {
                expected: expected_access_list_hash,
                got: computed_access_list_hash,
                expected_bal: access_list,
                got_bal: computed_access_list,
            }
            .boxed()
            .into());
        }

        let merge_started = Instant::now();
        db.merge_transitions(BundleRetention::Reverts);
        self.ctx
            .attempt_metrics
            .record_stage_duration(PayloadBuildStage::MergeTransitions, merge_started.elapsed());

        let flattened = flatten_reverts(&db.bundle_state.reverts);
        db.bundle_state.reverts = flattened;
        let bundle = extend_flashblock_bundle(&committed_state.bundle, db.take_bundle());

        // Block on the background trie walk kicked off before execution. The result is derived from
        // the received access list and validated against `diff.state_root` during block assembly.
        let StateRootResult {
            state_root,
            trie_updates,
            hashed_state,
        } = state_root_handle.finish()?;
        self.ctx
            .attempt_metrics
            .record_stage_duration(PayloadBuildStage::StateRoot, state_root_started.elapsed());

        let (transactions, senders) = canonical_transactions
            .into_iter()
            .map(|tx| tx.into_parts())
            .unzip();

        let block_assembly_started = Instant::now();
        let assembler = OpBlockAssembler::new(self.ctx.chain_spec.clone());
        let block = assembler.assemble_block(BlockAssemblerInput::<
            '_,
            '_,
            OpBlockExecutorFactory<OpRethReceiptBuilder, Arc<WorldChainSpec>>,
        >::new(
            evm_env,
            self.ctx.execution_context,
            self.ctx.parent,
            transactions,
            &execution_result,
            &bundle,
            finish_state_provider.as_ref(),
            state_root,
            None,
        ))?;
        self.ctx.attempt_metrics.record_stage_duration(
            PayloadBuildStage::BlockAssembly,
            block_assembly_started.elapsed(),
        );

        let block = RecoveredBlock::new_unhashed(block, senders);

        self.ctx
            .attempt_metrics
            .record_stage_duration(PayloadBuildStage::Finalize, finalize_started.elapsed());

        if block.receipts_root != diff.receipts_root {
            error!(
                target: "flashblocks::state_executor",
                got = ?block.receipts_root,
                expected = ?diff.receipts_root,
                "Receipts root mismatch"
            );
            return Err(BalValidationError::ReceiptsRootMismatch {
                expected: diff.receipts_root,
                got: block.receipts_root,
            }
            .boxed()
            .into());
        }

        if block.state_root != diff.state_root {
            error!(
                target: "flashblocks::state_executor",
                got = ?block.state_root,
                expected = ?diff.state_root,
                expected_bundle = ?bundle,
                "State root mismatch"
            );
            return Err(BalValidationError::StateRootMismatch {
                expected: diff.state_root,
                got: block.state_root,
                bundle_state: bundle.clone(),
            }
            .boxed()
            .into());
        }

        if block.hash() != diff.block_hash {
            error!(
                target: "flashblocks::state_executor",
                got = ?block.hash(),
                expected = ?diff.block_hash,
                "Block hash mismatch"
            );
            return Err(BalValidationError::BalHashMismatch {
                expected: diff.block_hash,
                got: block.hash(),
                expected_bal: access_list,
                got_bal: computed_access_list,
            }
            .boxed()
            .into());
        }

        let sealed_block = Arc::new(block.sealed_block().clone());
        let execution_output = BlockExecutionOutput {
            state: bundle,
            result: execution_result,
        };
        let executed_block: BuiltPayloadExecutedBlock<OpPrimitives> = BuiltPayloadExecutedBlock {
            recovered_block: Arc::new(block),
            execution_output: Arc::new(execution_output),
            hashed_state: Arc::new(hashed_state),
            trie_updates: Arc::new(trie_updates),
        };

        Ok(OpBuiltPayload::new(
            payload_id,
            sealed_block,
            committed_state.fees + fees,
            Some(executed_block),
        ))
    }
}

pub struct FlashblocksLegacyExecutionStrategy;

impl<S: StateRootStrategy> ExecutionStrategy<WorldChainEvmConfig, S>
    for FlashblocksLegacyExecutionStrategy
{
    fn execute(
        &self,
        ctx: ValidationCtx<'_, WorldChainEvmConfig, S>,
        client: impl StateProviderFactory + Clone + Sync + 'static,
        diff: ExecutionPayloadFlashblockDeltaV1,
        committed_state: CommittedState<OpRethReceiptBuilder>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError> {
        let bundle_state = committed_state.bundle.clone();

        let state_provider = client
            .state_by_block_hash(ctx.parent.hash())
            .map_err(BalExecutorError::other)?;

        let mut db = State::builder()
            .with_database(StateProviderDatabase::new(state_provider.as_ref()))
            .with_cached_prestate(cache_prestate_from_bundle(&bundle_state))
            .with_bundle_update()
            .build();

        let evm = OpEvmFactory::<OpTx>::default().create_evm(&mut db, ctx.evm_env.clone());

        let mut executor = OpBlockExecutor::new(
            evm,
            ctx.execution_context.clone(),
            (*ctx.chain_spec).clone(),
            OpRethReceiptBuilder::default(),
        );

        executor.gas_used = committed_state.gas_used;
        executor.da_footprint_used = committed_state.blob_gas_used;
        executor.receipts = committed_state.receipts_iter().cloned().collect();

        let mut builder = FlashblocksBlockBuilder::<OpPrimitives, _>::new(
            ctx.execution_context.clone(),
            ctx.parent,
            executor,
            committed_state.transactions_iter().cloned().collect(),
            ctx.chain_spec.clone(),
            bundle_state,
        );

        // Apply pre-execution changes on the first flashblock (no prior committed state).
        if committed_state.is_first {
            let pre_execution_changes_started = Instant::now();
            builder.apply_pre_execution_changes()?;
            ctx.attempt_metrics.record_stage_duration(
                PayloadBuildStage::PreExecutionChanges,
                pre_execution_changes_started.elapsed(),
            );
        }

        let basefee = ctx.evm_env.block_env.basefee;
        let transactions = decode_transactions_with_indices(
            &diff.transactions,
            committed_state.transactions_iter().count() as u64,
        )?;

        let mut fees = U256::ZERO;
        let txs_execution_started = Instant::now();
        for (_, tx) in &transactions {
            let gas_used = builder
                .execute_transaction_with_commit_condition(tx.clone(), |_| CommitChanges::Yes)?;

            if !tx.is_deposit()
                && let Some(gas_used) = gas_used
            {
                let miner_fee = tx
                    .effective_tip_per_gas(basefee)
                    .expect("fee is always valid; execution succeeded");
                fees += U256::from(gas_used.tx_gas_used()) * U256::from(miner_fee);
            }
        }
        ctx.attempt_metrics.record_stage_duration(
            PayloadBuildStage::SequencerTxExecution,
            txs_execution_started.elapsed(),
        );

        // Extract state root strategy from ctx; bundle state is only available after
        // merging transitions, so prepare() cannot be called until then.
        let state_root_strategy = ctx.state_root_strategy;
        let parent_hash = ctx.parent.hash();

        let finish_state_provider = client
            .state_by_block_hash(parent_hash)
            .map_err(BalExecutorError::other)?;

        let finalize_started = Instant::now();

        let (evm, execution_result) = builder.inner.executor.finish()?;
        let (db, evm_env) = evm.finish();

        let merge_started = Instant::now();
        db.merge_transitions(BundleRetention::Reverts);
        ctx.attempt_metrics
            .record_stage_duration(PayloadBuildStage::MergeTransitions, merge_started.elapsed());

        let bundle = extend_flashblock_bundle(&committed_state.bundle, db.take_bundle());

        let state_root_started = Instant::now();
        let hashed_state =
            HashedPostState::from_bundle_state::<KeccakKeyHasher>(bundle.state.iter());
        let state_root_handle = state_root_strategy.prepare(client, parent_hash, hashed_state)?;
        let StateRootResult {
            state_root,
            trie_updates,
            hashed_state,
        } = state_root_handle.finish()?;
        ctx.attempt_metrics
            .record_stage_duration(PayloadBuildStage::StateRoot, state_root_started.elapsed());

        let (transactions, senders) = builder
            .inner
            .transactions
            .into_iter()
            .map(|tx| tx.into_parts())
            .unzip();

        let block_assembly_started = Instant::now();
        let block = builder
            .inner
            .assembler
            .assemble_block(BlockAssemblerInput::<
                '_,
                '_,
                OpBlockExecutorFactory<OpRethReceiptBuilder>,
            >::new(
                evm_env,
                builder.inner.ctx,
                builder.inner.parent,
                transactions,
                &execution_result,
                &bundle,
                finish_state_provider.as_ref(),
                state_root,
                None,
            ))?;
        ctx.attempt_metrics.record_stage_duration(
            PayloadBuildStage::BlockAssembly,
            block_assembly_started.elapsed(),
        );

        let block = RecoveredBlock::new_unhashed(block, senders);

        ctx.attempt_metrics
            .record_stage_duration(PayloadBuildStage::Finalize, finalize_started.elapsed());

        let sealed_block = Arc::new(block.sealed_block().clone());
        let execution_output = BlockExecutionOutput {
            state: bundle,
            result: execution_result,
        };
        let executed_block: BuiltPayloadExecutedBlock<OpPrimitives> = BuiltPayloadExecutedBlock {
            recovered_block: Arc::new(block),
            execution_output: Arc::new(execution_output),
            hashed_state: Arc::new(hashed_state),
            trie_updates: Arc::new(trie_updates),
        };

        Ok(OpBuiltPayload::new(
            payload_id,
            sealed_block,
            committed_state.fees + fees,
            Some(executed_block),
        ))
    }
}
