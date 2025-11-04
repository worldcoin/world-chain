use alloy_consensus::{BlockHeader, Header, Transaction};
use alloy_eips::Decodable2718;
use alloy_op_evm::{
    block::receipt_builder::OpReceiptBuilder, OpBlockExecutionCtx, OpBlockExecutor, OpEvmFactory,
};
use alloy_primitives::{Address, FixedBytes, U256};
use eyre::eyre::eyre;
use flashblocks_primitives::{
    access_list::FlashblockAccessList, primitives::ExecutionPayloadFlashblockDeltaV1,
};
use op_alloy_consensus::OpTxEnvelope;
use rayon::prelude::*;
use reth::revm::{database::StateProviderDatabase, State};
use reth_evm::{
    block::{BlockExecutionError, BlockExecutor},
    execute::ExecutorTx,
    op_revm::{OpSpecId, OpTransaction},
    ConfigureEvm, Evm, EvmEnv, EvmFactory, FromRecoveredTx, FromTxWithEncoded,
};
use reth_node_api::PayloadBuilderError;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpEvmConfig, OpNextBlockEnvAttributes, OpRethReceiptBuilder};
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::OpBuiltPayload;
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use reth_payload_primitives::BuiltPayload;
use reth_primitives::{transaction::SignedTransaction, Recovered, SealedHeader};
use reth_provider::{BlockExecutionResult, StateProvider};
use reth_trie_common::{updates::TrieUpdates, HashedPostState, KeccakKeyHasher};
use revm::{
    context::TxEnv,
    database::{
        states::{bundle_state::BundleRetention, reverts::Reverts},
        BundleAccount, BundleState,
    },
    Database, DatabaseRef,
};
use revm_database_interface::WrapDatabaseRef;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tracing::info;

use crate::{
    access_list::{BlockAccessIndex, FlashblockAccessListConstruction},
    executor::{bal_builder::BalBuilderBlockExecutor, temporal_db::TemporalDbFactory},
};

/// A Block Executor for Optimism that can load pre state from previous flashblocks
///
/// A Block Access List is used to improve execution speed
///
/// 'BlockExecutor' trait is not flexible enough for our purposes.
/// TODO: WIP, currently unused
pub struct BalBlockExecutor<E, R, Spec> {
    spec: Spec,
    execution_context: OpBlockExecutionCtx,
    receipt_builder: R,
    config: OpEvmConfig,
    access_list: FlashblockAccessList,
    _marker: std::marker::PhantomData<E>,
}

pub struct ParallelTxExecutor<Evm, R, Spec>
where
    R: OpReceiptBuilder,
{
    inner: OpBlockExecutor<Evm, R, Spec>,
    flashblock_access_list: FlashblockAccessListConstruction,
}

impl<'db, DB, E, R, Spec> BalBlockExecutor<E, R, Spec>
where
    E: Evm<DB = &'db mut State<DB>, Tx = OpTransaction<TxEnv>, Spec = OpSpecId> + Send + Sync,
    DB: Database + DatabaseRef<Error: Send + Sync + 'static> + Clone + Send + Sync + 'db,
    OpTransaction<TxEnv>: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
    R: OpReceiptBuilder<Transaction = OpTxEnvelope, Receipt = OpReceipt> + Clone + Send + Sync,
    Spec: OpHardforks + Clone + Send + Sync,
{
    /// Creates a new [`FlashblocksBlockExecutor`].
    pub fn new(
        ctx: OpBlockExecutionCtx,
        spec: Spec,
        receipt_builder: R,
        config: OpEvmConfig,
        access_list: FlashblockAccessList,
    ) -> Self {
        Self {
            spec,
            execution_context: ctx,
            receipt_builder,
            config,
            access_list,
            _marker: std::marker::PhantomData,
        }
    }
    /// Verifies and executes a given [`ExecutionPayloadFlashblockDeltaV1`] on top of an option [`OpBuiltPayload`].
    ///
    ///
    pub fn verify_block(
        self,
        state_provider: &dyn StateProvider,
        committed_payload: Option<OpBuiltPayload>, // TODO: Pre-Load the bundle with this bundle.
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent_header: &SealedHeader<Header>,
    ) -> Result<
        (
            BundleState,
            BlockExecutionResult<R::Receipt>,
            EvmEnv<OpSpecId>,
            Vec<OpTxEnvelope>,
            OpBlockExecutionCtx,
        ),
        eyre::Report,
    >
    where
        Self: Sized,
    {
        let db = StateProviderDatabase::new(state_provider);
        let temporal_db_factory = TemporalDbFactory::new(&db, FlashblockAccessList::default());

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
                .block
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
                        .with_receipts(pre_loaded_receipts.clone())
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
        let (merged_access_list, mut merged_bundle_state, mut merged_execution_result) =
            execution_data.into_iter().fold(
                (
                    FlashblockAccessListConstruction::default(),
                    BundleState::default(),
                    BlockExecutionResult::<R::Receipt> {
                        receipts: vec![],
                        gas_used: 0,
                        requests: Default::default(),
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

        let temporal_db = temporal_db_factory.db(diff.transactions.len() as u64);

        let mut state = State::builder()
            .with_database_ref(temporal_db)
            .with_bundle_update()
            .build();

        let evm = OpEvmFactory::default().create_evm(
            &mut state,
            self.config
                .evm_env(&parent_header.header().clone())
                .unwrap(),
        );

        let executor = BalBuilderBlockExecutor::new(
            evm,
            self.execution_context.clone(),
            self.spec.clone(),
            self.receipt_builder.clone(),
        )
        .with_access_list(merged_access_list); // TODO: Need to aggregate receipts

        let (evm, execution_result, access_list_data, _, _) = executor.finish_with_access_list()?;

        merged_execution_result
            .receipts
            .extend_from_slice(&execution_result.receipts);

        let (db, env) = evm.finish();
        merge_reverts(db);

        merged_bundle_state.extend(db.bundle_state.clone());

        // Verify the hash matches
        if let Some(expected_data) = diff.access_list_data {
            let expected_hash = expected_data.access_list_hash;
            let computed_hash = access_list_data.access_list_hash;

            if expected_hash != computed_hash {
                return Err(eyre!(format!(
                    "Access List Hash does not match computed hash - expected {:#?} got {:#?}",
                    expected_hash, computed_hash
                )));
            }
        }

        Ok((
            merged_bundle_state,
            merged_execution_result,
            env,
            transactions
                .iter()
                .map(|(tx, _)| tx.inner().clone())
                .collect(),
            self.execution_context,
        ))

        // Compute the [`BlockExecutionOutcome`]
    }
}

#[expect(clippy::too_many_arguments, clippy::type_complexity)]
pub fn execute_transactions(
    transactions: Vec<Recovered<OpTransactionSigned>>,
    evm_config: &OpEvmConfig,
    sealed_header: Arc<SealedHeader<Header>>,
    state_provider: Arc<dyn StateProvider>,
    attributes: &OpNextBlockEnvAttributes,
    latest_bundle: Option<BundleState>,
    execution_context: OpBlockExecutionCtx,
    chain_spec: &OpChainSpec,
    committed_payload: Option<OpBuiltPayload>,
    provided_bal: Option<&FlashblockAccessList>,
) -> Result<
    (
        BundleState,
        BlockExecutionResult<OpReceipt>,
        FlashblockAccessList,
        EvmEnv<OpSpecId>,
        U256,
    ),
    eyre::Report,
> {
    // Prepare EVM environment.
    let evm_env = evm_config
        .next_evm_env(sealed_header.clone().header(), attributes)
        .map_err(PayloadBuilderError::other)?;

    let state = StateProviderDatabase::new(&state_provider);

    let mut state = if let Some(ref bundle) = latest_bundle {
        State::builder()
            .with_database(state)
            .with_bundle_prestate(bundle.clone())
            .with_bundle_update()
            .build()
    } else {
        State::builder()
            .with_database(state)
            .with_bundle_update()
            .build()
    };

    let evm = evm_config.evm_with_env(&mut state, evm_env);
    let base_fee = evm.block().basefee;

    let mut executor = if let Some(committed) = committed_payload {
        let receipts: Vec<OpReceipt> = committed
            .executed_block()
            .unwrap()
            .execution_outcome()
            .receipts()
            .iter()
            .flatten()
            .cloned()
            .collect();

        let min_tx_index = receipts.len() as u64;

        BalBuilderBlockExecutor::new(
            evm,
            execution_context.clone(),
            chain_spec,
            OpRethReceiptBuilder::default(),
        )
        .with_receipts(receipts)
        .with_min_tx_index(min_tx_index)
        .with_gas_used(
            committed
                .executed_block()
                .unwrap()
                .recovered_block()
                .gas_used(),
        )
    } else {
        BalBuilderBlockExecutor::new(
            evm,
            execution_context.clone(),
            chain_spec,
            OpRethReceiptBuilder::default(),
        )
    };

    let mut total_fees = U256::ZERO;

    if latest_bundle.is_none() {
        executor
            .apply_pre_execution_changes()
            .map_err(|e| eyre!(format!("failed to apply pre-execution changes: {e}")))?;
    }

    for transaction in transactions.iter() {
        let gas_used = executor
            .execute_transaction(transaction)
            .map_err(|e| eyre!(format!("failed to execute transaction: {e}")))?;

        if !transaction.is_deposit() {
            let miner_fee = transaction
                .effective_tip_per_gas(base_fee)
                .expect("fee is always valid; execution succeeded");
            total_fees += U256::from(miner_fee) * U256::from(gas_used);
        }
    }

    // Apply post execution changes
    let (evm, result, access_list_data, _, _) = executor
        .finish_with_access_list()
        .map_err(|e| eyre!(format!("failed to finish execution: {e}")))?;

    if provided_bal.is_some_and(|bal| alloy_rlp::encode(bal) != *access_list_data.access_list_hash)
    {
        info!(
            "Provided BAL: min_tx_index: {}, max_tx_index: {}",
            provided_bal.unwrap().min_tx_index,
            provided_bal.unwrap().max_tx_index
        );
        info!(
            "Computed BAL: min_tx_index: {}, max_tx_index: {}",
            access_list_data.access_list.min_tx_index, access_list_data.access_list.max_tx_index
        );
        return Err(eyre!(format!(
            "Access List Hash does not match computed hash - expected {:#?} got {:#?}",
            access_list_data.access_list_hash,
            provided_bal.map(alloy_rlp::encode)
        )));
    }

    let (db, env) = evm.finish();

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

    Ok((
        db.bundle_state.clone(),
        result,
        access_list_data.access_list,
        env,
        total_fees,
    ))
}

pub fn compute_state_root(
    state_provider: Arc<Box<dyn StateProvider>>,
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
