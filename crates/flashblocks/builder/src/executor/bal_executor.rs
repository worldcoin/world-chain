use alloy_consensus::{BlockHeader, Header};
use alloy_eips::Decodable2718;
use alloy_op_evm::{block::receipt_builder::OpReceiptBuilder, OpBlockExecutionCtx, OpEvmFactory};
use alloy_primitives::{Address, FixedBytes, U256};
use alloy_rpc_types_engine::PayloadId;
use eyre::eyre::{eyre, OptionExt};
use flashblocks_primitives::primitives::ExecutionPayloadFlashblockDeltaV1;
use op_alloy_consensus::OpTxEnvelope;
use rayon::prelude::*;
use reth::revm::{database::StateProviderDatabase, State};
use reth_chain_state::ExecutedBlock;
use reth_evm::{
    block::{BlockExecutionError, BlockExecutor},
    execute::{BlockAssembler, BlockAssemblerInput, BlockBuilderOutcome, ExecutorTx},
    op_revm::OpSpecId,
    ConfigureEvm, Evm, EvmEnv, EvmFactory,
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::OpBuiltPayload;
use reth_optimism_primitives::{OpPrimitives, OpReceipt, OpTransactionSigned};
use reth_payload_primitives::BuiltPayload;
use reth_primitives::{transaction::SignedTransaction, Recovered, RecoveredBlock, SealedHeader};
use reth_provider::{BlockExecutionResult, ExecutionOutcome, StateProvider};
use reth_trie_common::{updates::TrieUpdates, HashedPostState, KeccakKeyHasher};
use revm::database::{
    states::{bundle_state::BundleRetention, reverts::Reverts},
    BundleAccount, BundleState,
};
use revm_database_interface::WrapDatabaseRef;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::{
    access_list::{BlockAccessIndex, FlashblockAccessListConstruction},
    assembler::FlashblocksBlockAssembler,
    executor::{
        bal_builder::BalBuilderBlockExecutor, factory::FlashblocksBlockExecutorFactory,
        temporal_db::TemporalDbFactory,
    },
};

/// A Block Executor for Optimism that can load pre state from previous flashblocks
///
/// A Block Access List is used to improve execution speed
///
/// 'BlockExecutor' trait is not flexible enough for our purposes.
/// TODO: WIP, currently unused
pub struct BalBlockExecutor<R, Spec> {
    spec: Arc<Spec>,
    execution_context: OpBlockExecutionCtx,
    receipt_builder: R,
    config: OpEvmConfig,
}

impl<R, Spec> BalBlockExecutor<R, Spec>
where
    R: OpReceiptBuilder<Transaction = OpTxEnvelope, Receipt = OpReceipt> + Clone + Send + Sync,
    Spec: OpHardforks + Clone + Send + Sync,
{
    /// Creates a new [`FlashblocksBlockExecutor`].
    pub fn new(
        ctx: OpBlockExecutionCtx,
        spec: Arc<Spec>,
        receipt_builder: R,
        config: OpEvmConfig,
    ) -> Self {
        Self {
            spec,
            execution_context: ctx,
            receipt_builder,
            config,
        }
    }
    /// Verifies and executes a given [`ExecutionPayloadFlashblockDeltaV1`] on top of an option [`OpBuiltPayload`].
    pub(crate) fn verify_block(
        self,
        state_provider: &dyn StateProvider,
        committed_payload: Option<OpBuiltPayload>,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent_header: &SealedHeader<Header>,
    ) -> Result<
        (
            BundleState,
            BlockExecutionResult<R::Receipt>,
            EvmEnv<OpSpecId>,
            Vec<Recovered<OpTransactionSigned>>,
            OpBlockExecutionCtx,
        ),
        eyre::Report,
    >
    where
        Self: Sized,
    {
        let db = StateProviderDatabase::new(state_provider);

        let expected_access_list_data = diff
            .access_list_data
            .ok_or_eyre("Access list data must be provided on the diff")?;

        let temporal_db_factory =
            TemporalDbFactory::new(&db, expected_access_list_data.access_list.clone());

        let merge_reverts = |db: &mut State<WrapDatabaseRef<_>>| {
            // merge changes into the db
            db.merge_transitions(BundleRetention::Reverts);

            // flatten reverts into a single reverts as the bundle is re-used across multiple payloads
            // which represent a single atomic state transition. therefore reverts should have length 1
            // we only retain the first occurance of a revert for any given account.
            let flattened = db
                .bundle_state
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

            db.bundle_state.reverts = Reverts::new(vec![flattened]);
        };

        // Accumulate the transactions to execute
        let transactions = diff
            .transactions
            .clone()
            .into_iter()
            .enumerate()
            .map(|(i, tx)| {
                let tx_envelope = OpTransactionSigned::decode_2718(&mut tx.as_ref())
                    .map_err(|e| eyre!(format!("failed to decode transaction: {e}")))?;

                let recovered = tx_envelope.try_clone_into_recovered().map_err(|e| {
                    eyre!(format!(
                        "failed to recover transaction from signed envelope: {e}"
                    ))
                })?;

                Ok::<(Recovered<OpTransactionSigned>, BlockAccessIndex), eyre::Report>((
                    recovered,
                    i as BlockAccessIndex,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let pre_loaded_bundle = committed_payload.as_ref().map(|committed| {
            committed
                .executed_block()
                .unwrap()
                .execution_output
                .bundle
                .clone()
        });

        let pre_loaded_receipts: Option<Vec<OpReceipt>> =
            committed_payload.as_ref().map(|committed| {
                committed
                    .executed_block()
                    .unwrap()
                    .execution_outcome()
                    .receipts()
                    .iter()
                    .flatten()
                    .cloned()
                    .collect()
            });

        let temporal_db = temporal_db_factory.db(0);

        let mut state = if let Some(ref bundle) = pre_loaded_bundle {
            State::builder()
                .with_database_ref(temporal_db)
                .with_bundle_update()
                .with_bundle_prestate(bundle.clone())
                .build()
        } else {
            State::builder()
                .with_database_ref(temporal_db)
                .with_bundle_update()
                .build()
        };

        let evm = OpEvmFactory::default().create_evm(
            &mut state,
            self.config
                .evm_env(&parent_header.header().clone())
                .unwrap(),
        );

        let mut executor = BalBuilderBlockExecutor::new(
            evm,
            self.execution_context.clone(),
            self.spec.clone(),
            self.receipt_builder.clone(),
        );

        if let Some(pre_loaded_receipts) = &pre_loaded_receipts {
            executor = executor
                .with_receipts(pre_loaded_receipts.clone())
                .with_min_tx_index(pre_loaded_receipts.len() as u64);
        }

        executor.apply_pre_execution_changes()?;

        let mut execution_data = transactions
            .clone()
            .into_par_iter()
            .map(|(tx, index)| {
                let temporal_db = temporal_db_factory.db(index as u64);
                let mut state = if let Some(pre_loaded_bundle) = &pre_loaded_bundle {
                    State::builder()
                        .with_database_ref(temporal_db)
                        .with_bundle_update()
                        .with_bundle_prestate(pre_loaded_bundle.clone())
                        .build()
                } else {
                    State::builder()
                        .with_database_ref(temporal_db)
                        .with_bundle_update()
                        .build()
                };

                let evm = OpEvmFactory::default().create_evm(
                    &mut state,
                    self.config
                        .evm_env(&parent_header.header().clone())
                        .unwrap(),
                );

                let mut executor = BalBuilderBlockExecutor::new(
                    evm,
                    self.execution_context.clone(),
                    self.spec.clone(),
                    self.receipt_builder.clone(),
                );

                if let Some(pre_loaded_receipts) = &pre_loaded_receipts {
                    executor = executor
                        .with_block_access_index(index as BlockAccessIndex)
                        .with_min_tx_index(pre_loaded_receipts.len() as u64);
                }

                let _ = executor.execute_transaction(tx.as_executable())?;

                let access_list = executor.take_access_list();
                let (evm, result) = executor.finish()?;
                let (db, _) = evm.finish();
                merge_reverts(db);

                Ok::<
                    (
                        BlockAccessIndex,
                        FlashblockAccessListConstruction,
                        BundleState,
                        BlockExecutionResult<<R as OpReceiptBuilder>::Receipt>,
                    ),
                    BlockExecutionError,
                >((index, access_list, db.bundle_state.clone(), result))
            })
            .collect::<Result<Vec<_>, _>>()?;

        execution_data.sort_unstable_by_key(|e| e.0);

        // Merge access lists together
        let (merged_access_list, merged_bundle_state, mut merged_execution_result) =
            execution_data.into_iter().fold(
                (
                    FlashblockAccessListConstruction::default(),
                    BundleState::default(),
                    BlockExecutionResult::<R::Receipt> {
                        receipts: vec![],
                        gas_used: 0,
                        requests: Default::default(),
                        blob_gas_used: 0,
                    },
                ),
                |(mut access_list_acc, mut bundle_acc, mut results_acc),
                 (_, access_list, bundle_state, result)| {
                    access_list_acc.merge(access_list);
                    bundle_acc.extend(bundle_state);
                    results_acc.gas_used += result.gas_used;
                    results_acc.receipts.extend_from_slice(&result.receipts);

                    (access_list_acc, bundle_acc, results_acc)
                },
            );

        let mut curr_access_list = executor.take_access_list();
        curr_access_list.merge(merged_access_list.clone());

        let pre_execution_bundle = executor.evm_mut().db_mut();

        merge_reverts(pre_execution_bundle);

        // Pre-pend the merged bundle with the bundle created from the pre-execution changes
        pre_execution_bundle
            .bundle_state
            .extend(merged_bundle_state.clone());

        let mut executor = executor
            .with_access_list(curr_access_list)
            .with_block_access_index(transactions.len() as BlockAccessIndex + 1);

        let temporal_db = temporal_db_factory.db(transactions.len() as u64 + 1);
        executor.evm_mut().db_mut().database.0 = temporal_db;

        if let Some(pre_loaded_receipts) = &pre_loaded_receipts {
            executor = executor
                .with_receipts(merged_execution_result.receipts.to_vec())
                .with_min_tx_index(pre_loaded_receipts.len() as u64);
        }

        let (evm, execution_result, access_list_data, _, _) = executor.finish_with_access_list()?;

        merged_execution_result
            .receipts
            .extend_from_slice(&execution_result.receipts);

        let (db, env) = evm.finish();
        merge_reverts(db);

        // Verify the hash matches
        let expected_hash = expected_access_list_data.access_list_hash;
        let computed_hash = access_list_data.access_list_hash;

        if expected_hash != computed_hash {
            std::fs::write(
                format!("expected_{}", parent_header.number()),
                serde_json::to_string_pretty(&expected_access_list_data.access_list).unwrap(),
            )?;

            std::fs::write(
                format!("computed_{}", parent_header.number()),
                serde_json::to_string_pretty(&access_list_data.access_list).unwrap(),
            )?;

            return Err(eyre!(format!(
                "Access List Hash does not match computed hash - expected {:#?} got {:#?}",
                expected_hash, computed_hash
            )));
        }

        Ok((
            db.take_bundle(),
            merged_execution_result,
            env,
            transactions.into_iter().map(|(tx, _)| tx).collect(),
            self.execution_context,
        ))
    }

    /// Executes a [`ExecutionPayloadFlashblockDeltaV1`] on top of an optional [`OpBuiltPayload`].
    /// And computes the resulting [`OpBuiltPayload`].
    ///
    /// # Errors
    ///     If the provided BAL passed in the `diff` does not match the computed BAL from execution.
    pub fn validate_and_execute_diff_parallel(
        self,
        state_provider: Arc<impl StateProvider>,
        committed_payload: Option<OpBuiltPayload>, // TODO: Pre-Load the bundle with this bundle.
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent_header: &SealedHeader<Header>,
        chain_spec: Arc<OpChainSpec>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, eyre::Report> {
        let bundle: HashMap<Address, BundleAccount> = diff
            .clone()
            .access_list_data
            .ok_or_eyre("Access list data must be provided on the diff")?
            .access_list
            .into();

        let (r_0, r_1) = rayon::join(
            || {
                self.verify_block(
                    state_provider.as_ref(),
                    committed_payload,
                    diff,
                    parent_header,
                )
            },
            || compute_state_root(state_provider.clone(), &bundle),
        );

        let (bundle_state, execution_result, evm_env, transactions, execution_context) = r_0?;
        let (state_root, trie_updates, hashed_state) = r_1?;

        let (transactions, senders) = transactions.into_iter().map(|tx| tx.into_parts()).unzip();

        let assembler = FlashblocksBlockAssembler::new(chain_spec);

        let block = assembler.assemble_block(BlockAssemblerInput::<
            FlashblocksBlockExecutorFactory,
        >::new(
            evm_env,
            execution_context,
            parent_header,
            transactions,
            &execution_result,
            &bundle_state,
            &state_provider.clone(),
            state_root,
        ))?;

        let block = RecoveredBlock::new_unhashed(block, senders);

        // Construct the built payload
        let outcome = BlockBuilderOutcome::<OpPrimitives> {
            execution_result,
            hashed_state,
            trie_updates,
            block,
        };

        let sealed_block = Arc::new(outcome.block.sealed_block().clone());

        let execution_outcome = ExecutionOutcome::new(
            bundle_state,
            vec![outcome.execution_result.receipts.clone()],
            outcome.block.number(),
            Vec::new(),
        );

        // create the executed block data
        let executed_block = ExecutedBlock {
            recovered_block: Arc::new(outcome.block),
            execution_output: Arc::new(execution_outcome),
            hashed_state: Arc::new(outcome.hashed_state),
            trie_updates: Arc::new(outcome.trie_updates),
        };

        Ok(OpBuiltPayload::new(
            payload_id,
            sealed_block,
            U256::ZERO, // TODO: FIXME:
            Some(executed_block),
        ))
    }

    pub fn validate_and_execute_diff_linear(
        self,
        _state_provider: Arc<impl StateProvider>,
        _committed_payload: Option<OpBuiltPayload>, // TODO: Pre-Load the bundle with this bundle.
        _diff: ExecutionPayloadFlashblockDeltaV1,
        _parent_header: &SealedHeader<Header>,
        _chain_spec: Arc<OpChainSpec>,
        _payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, eyre::Report> {
        todo!()
    }
}

pub fn compute_state_root(
    state_provider: Arc<impl StateProvider>,
    bundle: &HashMap<Address, BundleAccount>,
) -> Result<(FixedBytes<32>, TrieUpdates, HashedPostState), eyre::Report> {
    let bundle_state: HashMap<&Address, &BundleAccount> = bundle.iter().collect();

    // compute hashed post state
    let hashed_state = HashedPostState::from_bundle_state::<KeccakKeyHasher>(bundle_state);

    // compute state root & trie updates
    let (state_root, trie_updates) = state_provider
        .state_root_with_updates(hashed_state.clone())
        .map_err(BlockExecutionError::other)?;

    Ok((state_root, trie_updates, hashed_state))
}

pub fn clone_state<DB>(state: &State<Arc<DB>>) -> State<Arc<DB>> {
    State {
        cache: state.cache.clone(),
        database: state.database.clone(),
        transition_state: state.transition_state.clone(),
        bundle_state: state.bundle_state.clone(),
        use_preloaded_bundle: state.use_preloaded_bundle,
        block_hashes: state.block_hashes.clone(),
    }
}
