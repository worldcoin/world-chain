use alloy_consensus::{Header, Transaction};
use alloy_op_evm::OpBlockExecutionCtx;
use alloy_primitives::{keccak256, Address, FixedBytes, U256};
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
use reth_optimism_node::{OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
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

use crate::executor::bal_builder::BalBuilderBlockExecutor;

#[expect(clippy::too_many_arguments, clippy::type_complexity)]
pub fn execute_transactions(
    transactions: Vec<Recovered<OpTransactionSigned>>,
    provided_bal_hash: Option<FixedBytes<32>>,
    evm_config: &OpEvmConfig,
    sealed_header: Arc<SealedHeader<Header>>,
    state_provider: Arc<dyn StateProvider>,
    attributes: &OpNextBlockEnvAttributes,
    latest_bundle: Option<BundleState>,
    execution_context: OpBlockExecutionCtx,
    chain_spec: &OpChainSpec,
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

    let genesis_alloc = &chain_spec.genesis.alloc;

    let mut executor = BalBuilderBlockExecutor::new(
        evm,
        execution_context.clone(),
        chain_spec,
        OpRethReceiptBuilder::default(),
        0, // TODO: Need to pre-load receipts from the latest payload if available min_tx_index = receipts.len() as u64
    );

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
    let (evm, result, access_list, min_tx_index, max_tx_index) = executor
        .finish_with_access_list()
        .map_err(|e| eyre!(format!("failed to finish execution: {e}")))?;

    let access_list = access_list.access_list;

    // Validate the BAL matches the provided Flashblock BAL
    let expected_bal_hash = keccak256(alloy_rlp::encode(&access_list));

    // TODO: re-enable this check once we have fixed execution logic.
    // if provided_bal_hash.is_some() && expected_bal_hash != provided_bal_hash.unwrap() {
    //     return Err(eyre!(format!(
    //         "Access List Hash does not match computed hash - expected {:#?} got {:#?}",
    //         expected_bal_hash,
    //         provided_bal_hash.unwrap()
    //     )));
    // }

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
        access_list,
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
