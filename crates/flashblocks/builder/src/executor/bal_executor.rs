use alloy_consensus::{BlockHeader, Header};
use alloy_eips::Decodable2718;
use alloy_op_evm::{block::receipt_builder::OpReceiptBuilder, OpBlockExecutionCtx, OpEvmFactory};
use alloy_primitives::{Address, Bytes, FixedBytes, U256};
use alloy_rpc_types_engine::PayloadId;
use eyre::eyre::{eyre, OptionExt};
use flashblocks_primitives::{
    access_list::FlashblockAccessListData, primitives::ExecutionPayloadFlashblockDeltaV1,
};
use op_alloy_consensus::OpTxEnvelope;
use rayon::prelude::*;
use reth::revm::{database::StateProviderDatabase, State};
use reth_chain_state::ExecutedBlock;
use reth_evm::{
    block::{BlockExecutionError, BlockExecutor},
    execute::{BlockAssembler, BlockAssemblerInput, BlockBuilderOutcome, ExecutorTx},
    op_revm::OpSpecId,
    ConfigureEvm, Database, Evm, EvmEnv, EvmFactory,
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
use revm::{
    database::{
        states::{bundle_state::BundleRetention, reverts::Reverts},
        BundleAccount, BundleState, TransitionState,
    },
};
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
    receipt_builder: R,
    execution_context: OpBlockExecutionCtx,
    config: OpEvmConfig,
}

impl<R, Spec> BalBlockExecutor<R, Spec>
where
    R: OpReceiptBuilder<Transaction = OpTxEnvelope, Receipt = OpReceipt> + Clone + Send + Sync,
    Spec: OpHardforks + Clone + Send + Sync,
{
    /// Creates a new [`FlashblocksBlockExecutor`].
    pub fn new(
        spec: Arc<Spec>,
        receipt_builder: R,
        execution_context: OpBlockExecutionCtx,
        config: OpEvmConfig,
    ) -> Self {
        Self {
            spec,
            execution_context,
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

        // Decode and recover transactions
        let transactions = Self::decode_transactions(
            &diff.transactions,
            committed_payload
                .as_ref()
                .map_or(0, |p| p.block().transaction_count() as BlockAccessIndex + 1),
        )?;

        // Extract pre-loaded state from committed payload
        let (pre_loaded_bundle, pre_loaded_receipts) =
            Self::extract_committed_state(&committed_payload);

        // Execute transactions in parallel
        let execution_data = self.execute_transactions_parallel(
            &transactions,
            &temporal_db_factory,
            &pre_loaded_bundle,
            &pre_loaded_receipts,
            parent_header,
        )?;

        // Merge execution results
        let (merged_access_list, merged_transitions, merged_execution_result) =
            Self::merge_execution_data(execution_data);

        let execution_context = self.execution_context.clone();

        // Finalize execution with merged results
        let (bundle_state, env, execution_result, access_list_data) = self.finalize_execution(
            pre_loaded_bundle,
            pre_loaded_receipts,
            merged_access_list,
            merged_transitions,
            merged_execution_result,
            parent_header,
            db.clone(),
        )?;

        // Verify access list hash
        Self::verify_access_list_hash(
            &expected_access_list_data,
            &access_list_data,
            parent_header.number(),
        )?;

        Ok((
            bundle_state,
            execution_result,
            env,
            transactions.into_iter().map(|(tx, _)| tx).collect(),
            execution_context,
        ))
    }

    /// Decodes transactions from raw bytes and recovers signer addresses.
    fn decode_transactions(
        encoded_transactions: &[Bytes],
        start_index: BlockAccessIndex,
    ) -> Result<Vec<(Recovered<OpTransactionSigned>, BlockAccessIndex)>, eyre::Report> {
        encoded_transactions
            .iter()
            .enumerate()
            .map(|(i, tx)| {
                let tx_envelope = OpTransactionSigned::decode_2718(&mut tx.as_ref())
                    .map_err(|e| eyre!("failed to decode transaction: {e}"))?;

                let recovered = tx_envelope.try_clone_into_recovered().map_err(|e| {
                    eyre!("failed to recover transaction from signed envelope: {e}")
                })?;

                Ok((recovered, start_index + i as BlockAccessIndex))
            })
            .collect()
    }

    /// Extracts pre-loaded bundle and receipts from committed payload.
    fn extract_committed_state(
        committed_payload: &Option<OpBuiltPayload>,
    ) -> (Option<BundleState>, Option<Vec<OpReceipt>>) {
        let bundle = committed_payload
            .as_ref()
            .and_then(|p| p.executed_block())
            .map(|block| block.execution_output.bundle.clone());

        let receipts = committed_payload
            .as_ref()
            .and_then(|p| p.executed_block())
            .map(|block| {
                block
                    .execution_outcome()
                    .receipts()
                    .iter()
                    .flatten()
                    .cloned()
                    .collect()
            });

        (bundle, receipts)
    }

    /// Executes transactions in parallel and returns execution data.
    fn execute_transactions_parallel(
        &self,
        transactions: &[(Recovered<OpTransactionSigned>, BlockAccessIndex)],
        temporal_db_factory: &TemporalDbFactory<StateProviderDatabase<&dyn StateProvider>>,
        pre_loaded_bundle: &Option<BundleState>,
        pre_loaded_receipts: &Option<Vec<OpReceipt>>,
        parent_header: &SealedHeader<Header>,
    ) -> Result<
        Vec<(
            BlockAccessIndex,
            FlashblockAccessListConstruction,
            Option<TransitionState>,
            BlockExecutionResult<R::Receipt>,
        )>,
        eyre::Report,
    > {
        let evm_env = self
            .config
            .evm_env(parent_header.header())
            .map_err(|e| eyre!("failed to create EVM environment: {e}"))?;

        let mut execution_data: Vec<_> = transactions
            .par_iter()
            .map(|(tx, index)| {
                let temporal_db = temporal_db_factory.db(*index as u64);
                let mut state = State::builder()
                    .with_database_ref(temporal_db)
                    .with_bundle_prestate(pre_loaded_bundle.clone().unwrap_or_default())
                    .with_bundle_update()
                    .build();

                let evm = OpEvmFactory::default().create_evm(&mut state, evm_env.clone());

                let mut executor = BalBuilderBlockExecutor::new(
                    evm,
                    self.execution_context.clone(),
                    self.spec.clone(),
                    self.receipt_builder.clone(),
                );

                if let Some(receipts) = pre_loaded_receipts {
                    executor = executor
                        .with_block_access_index(*index)
                        .with_min_tx_index(receipts.len() as u64);
                }

                executor.execute_transaction(tx.as_executable())?;

                let access_list = executor.take_access_list();
                let (evm, result) = executor.finish()?;
                let (db, _) = evm.finish();

                Ok::<_, BlockExecutionError>((
                    *index,
                    access_list,
                    db.transition_state.take(),
                    result,
                ))
            })
            .collect::<Result<_, _>>()?;

        // Sort by ascending index
        execution_data.sort_unstable_by_key(|(index, ..)| *index);

        Ok(execution_data)
    }

    /// Merges execution data from parallel execution.
    fn merge_execution_data(
        execution_data: Vec<(
            BlockAccessIndex,
            FlashblockAccessListConstruction,
            Option<TransitionState>,
            BlockExecutionResult<R::Receipt>,
        )>,
    ) -> (
        FlashblockAccessListConstruction,
        TransitionState,
        BlockExecutionResult<R::Receipt>,
    ) {
        execution_data.into_iter().fold(
            (
                FlashblockAccessListConstruction::default(),
                TransitionState::default(),
                BlockExecutionResult {
                    receipts: vec![],
                    gas_used: 0,
                    requests: Default::default(),
                    blob_gas_used: 0,
                },
            ),
            |(mut access_list_acc, mut transition_state_acc, mut results_acc),
             (_, access_list, transition_state, result)| {
                access_list_acc.merge(access_list);
                if let Some(state) = transition_state {
                    transition_state_acc.add_transitions(state.transitions.into_iter().collect());
                }
                results_acc.gas_used += result.gas_used;
                results_acc.receipts.extend(result.receipts);

                (access_list_acc, transition_state_acc, results_acc)
            },
        )
    }

    /// Finalizes execution with merged results and returns final state.
    #[allow(clippy::too_many_arguments)]
    fn finalize_execution<'a>(
        self,
        pre_loaded_bundle: Option<BundleState>,
        pre_loaded_receipts: Option<Vec<OpReceipt>>,
        merged_access_list: FlashblockAccessListConstruction,
        merged_transitions: TransitionState,
        merged_execution_result: BlockExecutionResult<R::Receipt>,
        parent_header: &SealedHeader<Header>,
        db: StateProviderDatabase<&'a dyn StateProvider>,
    ) -> Result<
        (
            BundleState,
            EvmEnv<OpSpecId>,
            BlockExecutionResult<R::Receipt>,
            FlashblockAccessListData,
        ),
        eyre::Report,
    > {
        let evm_env = self
            .config
            .evm_env(parent_header.header())
            .map_err(|e| eyre!("failed to create EVM environment: {e}"))?;

        let mut state = State::builder()
            .with_database(db)
            .with_bundle_update()
            .build();

        let mut executor = BalBuilderBlockExecutor::new(
            OpEvmFactory::default().create_evm(&mut state, evm_env),
            self.execution_context.clone(),
            self.spec.clone(),
            self.receipt_builder.clone(),
        )
        .with_access_list(merged_access_list)
        .with_block_access_index(0);

        // if this is the first execution, apply pre-execution changes
        if pre_loaded_receipts.is_none() {
            executor.apply_pre_execution_changes()?;
        }

        if let Some(mut receipts) = pre_loaded_receipts {
            // first tx index after pre-loaded receipts
            let receipts_len = receipts.len();
            // append executed receipts to pre-loaded receipts
            receipts.extend(merged_execution_result.clone().receipts);
            executor = executor
                .with_receipts(receipts)
                .with_min_tx_index(receipts_len as u64);
        }

        // total ordering block index of post_execution_changes
        let next_index = merged_execution_result.receipts.len() as BlockAccessIndex + 1;

        executor = executor.with_block_access_index(next_index);

        if let Some(transitions) = executor.evm_mut().db_mut().transition_state.as_mut() {
            // merge ordered transitions from transaction execution on top of either
            // pre-loaded transitions or pre-execution transitions
            transitions.add_transitions(merged_transitions.transitions.into_iter().collect());
        }

        let (evm, execution_result, access_list_data, _, _) = executor.finish_with_access_list()?;
        let (mut db, env) = evm.finish();

        Self::merge_reverts(&mut db);

        Ok((db.take_bundle(), env, execution_result, access_list_data))
    }

    /// Merges and flattens reverts in the database state.
    fn merge_reverts(db: &mut State<impl Database>) {
        // Merge changes into the db
        db.merge_transitions(BundleRetention::Reverts);

        // Flatten reverts into a single vector. The bundle is reused across multiple payloads
        // which represent a single atomic state transition, so reverts should have length 1.
        // We only retain the first occurrence of a revert for any given account.
        let flattened = db
            .bundle_state
            .reverts
            .iter()
            .flatten()
            .scan(HashSet::new(), |visited, (acc, revert)| {
                visited.insert(acc).then(|| (*acc, revert.clone()))
            })
            .collect();

        db.bundle_state.reverts = Reverts::new(vec![flattened]);
    }

    /// Verifies that the computed access list hash matches the expected hash.
    fn verify_access_list_hash(
        expected: &FlashblockAccessListData,
        computed: &FlashblockAccessListData,
        block_number: u64,
    ) -> Result<(), eyre::Report> {
        if expected.access_list_hash != computed.access_list_hash {
            use std::io::Write;
            let _ =
                std::fs::File::create(format!("expected_{block_number}.json")).and_then(|mut f| {
                    f.write_all(serde_json::to_string_pretty(&expected.access_list)?.as_bytes())
                });

            let _ =
                std::fs::File::create(format!("computed_{block_number}.json")).and_then(|mut f| {
                    f.write_all(serde_json::to_string_pretty(&computed.access_list)?.as_bytes())
                });

            return Err(eyre!(
                "Access list hash mismatch - expected {:#?}, got {:#?}",
                expected.access_list_hash,
                computed.access_list_hash
            ));
        }

        Ok(())
    }

    /// Executes a [`ExecutionPayloadFlashblockDeltaV1`] on top of an optional [`OpBuiltPayload`].
    /// And computes the resulting [`OpBuiltPayload`].
    ///
    /// # Errors
    ///     If the provided BAL passed in the `diff` does not match the computed BAL from execution.
    pub fn validate_and_execute_diff_parallel(
        self,
        state_provider: Arc<dyn StateProvider>,
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
    state_provider: Arc<dyn StateProvider>,
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
