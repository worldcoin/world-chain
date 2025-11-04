use alloy_consensus::{BlockHeader, Header, Transaction};
use alloy_op_evm::OpBlockExecutionCtx;
use alloy_primitives::{Address, FixedBytes, U256};
use eyre::eyre::eyre;
use flashblocks_primitives::access_list::FlashblockAccessList;
use reth::revm::database::StateProviderDatabase;
use reth::revm::State;
use reth_evm::block::{BlockExecutionError, BlockExecutor};
use reth_evm::op_revm::OpSpecId;
use reth_evm::{ConfigureEvm, Evm, EvmEnv};
use reth_node_api::PayloadBuilderError;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpNextBlockEnvAttributes;
use reth_optimism_node::{OpBuiltPayload, OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use reth_payload_primitives::BuiltPayload;
use reth_primitives::Recovered;
use reth_primitives::SealedHeader;
use reth_provider::{BlockExecutionResult, StateProvider};
use reth_trie_common::updates::TrieUpdates;
use reth_trie_common::{HashedPostState, KeccakKeyHasher};
use revm::database::states::bundle_state::BundleRetention;
use revm::database::states::reverts::Reverts;
use revm::database::{BundleAccount, BundleState};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::info;

use crate::executor::bal_builder::BalBuilderBlockExecutor;

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
            provided_bal.map(|bal| alloy_rlp::encode(bal))
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
