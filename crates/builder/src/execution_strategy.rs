use std::{sync::Arc, time::Instant};

use alloy_consensus::{BlockHeader, Header, Transaction};
use alloy_op_evm::{OpBlockExecutionCtx, OpBlockExecutor, OpEvmFactory, OpTx};
use alloy_primitives::{Address, B256, U256};
use alloy_rpc_types_engine::PayloadId;
use reth_evm::{
    ConfigureEvm, EvmEnvFor, EvmFactory,
    block::{BlockExecutionError, CommitChanges},
    execute::{BlockBuilder, BlockBuilderOutcome},
};
use reth_node_api::BuiltPayloadExecutedBlock;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_node::OpBuiltPayload;
use reth_optimism_primitives::OpPrimitives;
use reth_primitives_traits::SealedHeader;
use reth_provider::{BlockExecutionOutput, StateProvider, StateProviderFactory};
use reth_revm::database::StateProviderDatabase;
use reth_trie_common::{HashedPostState, KeccakKeyHasher, updates::TrieUpdates};
use revm::database::BundleAccount;
use revm_database::State;
use tracing::error;
use world_chain_primitives::{
    access_list::FlashblockAccessListData, primitives::ExecutionPayloadFlashblockDeltaV1,
};

use crate::{
    BlockBuilderExt,
    bal_executor::{BalExecutorError, BalValidationError, CommittedState},
    database::{
        bal_builder_db::{BalBuilderDb, NoOpCommitDB},
        bundle_db::BundleDb,
        temporal_db::TemporalDbFactory,
    },
    executor::FlashblocksBlockBuilder,
    flashblock_validation_metrics::FlashblockValidationAttemptMetrics,
    metrics::PayloadBuildStage,
    state_root_strategy::StateRootStrategy,
    validator::{BalBlockValidator, decode_transactions_with_indices},
};

/// Result of computing the state root from a bundle state.
pub struct StateRootResult {
    pub state_root: B256,
    pub trie_updates: TrieUpdates,
    pub hashed_state: HashedPostState,
}

pub fn compute_state_root<'a>(
    state_provider: Arc<dyn StateProvider + Send>,
    bundle_state: impl IntoIterator<Item = (&'a Address, &'a BundleAccount)>,
) -> Result<StateRootResult, BlockExecutionError> {
    let hashed_state = HashedPostState::from_bundle_state::<KeccakKeyHasher>(bundle_state);
    let (state_root, trie_updates) = state_provider
        .state_root_with_updates(hashed_state.clone())
        .map_err(BlockExecutionError::other)?;
    Ok(StateRootResult {
        state_root,
        trie_updates,
        hashed_state,
    })
}

/// Context passed to an [`ExecutionStrategy`] for each flashblock.
pub struct ValidationCtx<'a, Evm: ConfigureEvm> {
    pub parent: &'a SealedHeader<Header>,
    pub attempt_metrics: &'a mut FlashblockValidationAttemptMetrics,
    pub chain_spec: Arc<OpChainSpec>,
    pub evm_env: EvmEnvFor<Evm>,
    pub execution_context: OpBlockExecutionCtx,
}

/// Strategy for executing flashblock diff transactions.
pub trait ExecutionStrategy<Evm: ConfigureEvm>: Send + Sync {
    fn execute(
        &self,
        ctx: ValidationCtx<'_, Evm>,
        client: impl StateProviderFactory + Clone + Sync + 'static,
        diff: ExecutionPayloadFlashblockDeltaV1,
        committed_state: CommittedState<OpRethReceiptBuilder>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError>;
}

pub struct FlashblocksBalExecutionStrategy<S: StateRootStrategy> {
    pub state_root_strategy: S,
}

impl<S: StateRootStrategy> ExecutionStrategy<OpEvmConfig> for FlashblocksBalExecutionStrategy<S> {
    fn execute(
        &self,
        ctx: ValidationCtx<'_, OpEvmConfig>,
        client: impl StateProviderFactory + Clone + Sync + 'static,
        diff: ExecutionPayloadFlashblockDeltaV1,
        committed_state: CommittedState<OpRethReceiptBuilder>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError> {
        let FlashblockAccessListData {
            access_list,
            access_list_hash,
        } = diff
            .access_list_data
            .ok_or(BalValidationError::MissingAccessListData.boxed())
            .map_err(BalExecutorError::from)?;

        let state_provider_ref = client
            .state_by_block_hash(ctx.parent.hash())
            .map_err(BalExecutorError::other)?;
        let state_provider_database = StateProviderDatabase::new(state_provider_ref.as_ref());
        let block_access_index = access_list.min_tx_index;

        let mut bundle_state = committed_state.bundle.clone();

        let bundle_database =
            BundleDb::new(state_provider_database.clone(), bundle_state.clone().into());
        let temporal_db_factory = TemporalDbFactory::new(&bundle_database, &access_list);
        let temporal_db = temporal_db_factory.db(bundle_database, block_access_index as u64);

        access_list
            .extend_bundle(&mut bundle_state, &state_provider_database)
            .map_err(BalExecutorError::other)?;

        let mut noop_state = NoOpCommitDB::new(temporal_db);
        let mut database = BalBuilderDb::new(&mut noop_state);
        database.set_index(block_access_index);

        let state_root_handle = self
            .state_root_strategy
            .prepare(client.clone(), ctx.parent.hash(), bundle_state.clone())
            .map_err(BalExecutorError::other)?;

        let evm = OpEvmFactory::default().create_evm(database, ctx.evm_env.clone());

        let mut executor: OpBlockExecutor<_, OpRethReceiptBuilder, Arc<OpChainSpec>> =
            OpBlockExecutor::new(
                evm,
                ctx.execution_context.clone(),
                ctx.chain_spec.clone(),
                OpRethReceiptBuilder::default(),
            );

        executor.gas_used = committed_state.gas_used;
        executor.receipts = committed_state.receipts_iter().cloned().collect();

        let (validator, access_list_receiver) = BalBlockValidator::new(
            ctx.execution_context.clone(),
            ctx.parent,
            executor,
            bundle_state.clone().into(),
            committed_state.bundle.clone().into(),
            committed_state.transactions_iter().cloned().collect(),
            ctx.chain_spec.clone(),
            &temporal_db_factory,
            state_root_handle,
            ctx.evm_env.clone(),
            (access_list.min_tx_index, access_list.max_tx_index),
            ctx.attempt_metrics,
        );

        let transactions_offset = committed_state.transactions.len() as u16 + 1;
        let executor_transactions =
            decode_transactions_with_indices(&diff.transactions, transactions_offset)?;

        let finish_state_provider = client
            .state_by_block_hash(ctx.parent.hash())
            .map_err(BalExecutorError::other)?;

        let (outcome, fees): (BlockBuilderOutcome<OpPrimitives>, u128) =
            validator.execute_block(client.clone(), finish_state_provider.as_ref(), executor_transactions)?;

        let computed_access_list = access_list_receiver
            .recv()
            .map_err(BalExecutorError::other)?;

        let computed_access_list_hash =
            world_chain_primitives::access_list::access_list_hash(&computed_access_list);

        if computed_access_list_hash != access_list_hash {
            error!(
                target: "flashblocks::state_executor",
                ?access_list_hash,
                expected = ?access_list_hash,
                access_list = ?computed_access_list,
                expected_access_list = ?access_list.clone(),
                block_number = outcome.block.number(),
                block_hash = ?outcome.block.hash(),
                execution_context = ?ctx.execution_context,
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

        Ok(OpBuiltPayload::new(
            payload_id,
            sealed_block,
            U256::from(fees),
            Some(executed_block),
        ))
    }
}

pub struct FlashblocksLegacyExecutionStrategy;

impl ExecutionStrategy<OpEvmConfig> for FlashblocksLegacyExecutionStrategy {
    fn execute(
        &self,
        ctx: ValidationCtx<'_, OpEvmConfig>,
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
            .with_bundle_prestate(bundle_state)
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
        executor.receipts = committed_state.receipts_iter().cloned().collect();

        let mut builder = FlashblocksBlockBuilder::<OpPrimitives, _>::new(
            ctx.execution_context.clone(),
            ctx.parent,
            executor,
            committed_state.transactions_iter().cloned().collect(),
            ctx.chain_spec.clone(),
        );

        // Apply pre-execution changes on the first flashblock (no prior committed state).
        if committed_state.transactions.is_empty() {
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
            committed_state.transactions_iter().count() as u16,
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
                fees += U256::from(gas_used) * U256::from(miner_fee);
            }
        }
        ctx.attempt_metrics.record_stage_duration(
            PayloadBuildStage::SequencerTxExecution,
            txs_execution_started.elapsed(),
        );

        let finish_state_provider = client
            .state_by_block_hash(ctx.parent.hash())
            .map_err(BalExecutorError::other)?;

        let finalize_started = Instant::now();
        let (outcome, bundle) = builder
            .finish_with_bundle(finish_state_provider.as_ref(), &mut *ctx.attempt_metrics)?;
        ctx.attempt_metrics
            .record_stage_duration(PayloadBuildStage::Finalize, finalize_started.elapsed());

        let BlockBuilderOutcome {
            execution_result,
            block,
            hashed_state,
            trie_updates,
        } = outcome;

        let sealed_block = Arc::new(block.sealed_block().clone());
        let execution_output = BlockExecutionOutput {
            state: bundle,
            result: execution_result,
        };
        let executed_block: BuiltPayloadExecutedBlock<OpPrimitives> = BuiltPayloadExecutedBlock {
            recovered_block: Arc::new(block),
            execution_output: Arc::new(execution_output),
            hashed_state: either::Left(Arc::new(hashed_state)),
            trie_updates: either::Left(Arc::new(trie_updates)),
        };

        Ok(OpBuiltPayload::new(
            payload_id,
            sealed_block,
            committed_state.fees + fees,
            Some(executed_block),
        ))
    }
}
