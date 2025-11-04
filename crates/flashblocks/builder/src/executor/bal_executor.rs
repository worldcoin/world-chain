use alloy_consensus::{BlockHeader, Header, Transaction, TxReceipt};
use alloy_eips::Encodable2718;
use alloy_op_evm::block::receipt_builder::OpReceiptBuilder;
use alloy_op_evm::{OpBlockExecutionCtx, OpBlockExecutor, OpEvmFactory};
use alloy_primitives::{Address, FixedBytes, U256};
use eyre::eyre::eyre;
use flashblocks_primitives::access_list::FlashblockAccessList;
use rayon::prelude::*;
use reth::revm::database::StateProviderDatabase;
use reth::revm::State;
use reth_evm::block::{BlockExecutionError, BlockExecutor};
use reth_evm::op_revm::{OpSpecId, OpTransaction};
use reth_evm::{ConfigureEvm, Evm, EvmEnv, EvmFactory};
use reth_evm::{Database, FromRecoveredTx, FromTxWithEncoded};
use reth_node_api::PayloadBuilderError;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpNextBlockEnvAttributes;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{OpBuiltPayload, OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use reth_payload_primitives::BuiltPayload;
use reth_primitives::Recovered;
use reth_primitives::SealedHeader;
use reth_provider::{BlockExecutionResult, StateProvider};
use reth_trie_common::updates::TrieUpdates;
use reth_trie_common::{HashedPostState, KeccakKeyHasher};
use revm::context::TxEnv;
use revm::database::states::bundle_state::BundleRetention;
use revm::database::states::reverts::Reverts;
use revm::database::{BundleAccount, BundleState, CacheDB};
use revm::DatabaseRef;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::info;

use crate::access_list::FlashblockAccessListConstruction;
use crate::executor::bal_builder::BalBuilderBlockExecutor;
use crate::executor::temporal_db::TemporalDbFactory;

/// A Block Executor for Optimism that can load pre state from previous flashblocks
///
/// A Block Access List is used to improve execution speed
///
/// 'BlockExecutor' trait is not flexible enough for our purposes.
/// TODO: WIP, currently unused
pub struct BalBlockExecutor<Evm, R, Spec>
where
    R: OpReceiptBuilder,
{
    inner: OpBlockExecutor<Evm, R, Spec>,
    flashblock_access_list: FlashblockAccessList,
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
    DB: Database + DatabaseRef<Error: Send + Sync + 'static> + Send + Sync + 'db,
    E: Evm<
            DB = &'db mut State<DB>,
            Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
            Spec = OpSpecId,
        > + Send
        + Sync,
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt> + Send + Sync,
    Spec: OpHardforks + Clone + Send + Sync,
{
    /// Creates a new [`FlashblocksBlockExecutor`].
    pub fn new(
        evm: E,
        ctx: OpBlockExecutionCtx,
        spec: Spec,
        receipt_builder: R,
        flashblock_access_list: FlashblockAccessList,
    ) -> Self {
        let executor = OpBlockExecutor::new(evm, ctx, spec, receipt_builder);

        Self {
            inner: executor,
            flashblock_access_list,
        }
    }

    /// Extends the [`BundleState`] of the executor with a specified pre-image.
    ///
    /// This should be used _only_ when initializing the executor
    pub fn with_bundle_prestate(mut self, pre_state: BundleState) -> Self {
        self.inner.evm_mut().db_mut().bundle_state.extend(pre_state);
        self
    }

    /// Extends the receipts to reflect the aggregated execution result
    pub fn with_receipts(mut self, receipts: Vec<R::Receipt>) -> Self {
        self.inner.receipts.extend_from_slice(&receipts);
        self
    }

    fn execute_block(
        mut self,
        transactions: impl IntoParallelIterator<Item = (OpTransaction<TxEnv>, u64)>,
    ) -> Result<
        BlockExecutionResult<<OpBlockExecutor<E, R, Spec> as BlockExecutor>::Receipt>,
        BlockExecutionError,
    >
    where
        Self: Sized,
    {
        self.inner.apply_pre_execution_changes()?;

        let (state, env) = self.inner.evm.finish();
        // TODO: may not need this cache
        let cache_db = CacheDB::new(&*state);
        let temporal_factory = TemporalDbFactory::new(&cache_db, self.flashblock_access_list);

        let res = transactions.into_par_iter().for_each(|(tx, index)| {
            let db = temporal_factory.db(index);
            // TODO: we probably can get rid of this cache as well
            let db = CacheDB::new(db);
            let mut evm = OpEvmFactory::default().create_evm(db, env.clone());
            evm.transact_raw(tx).unwrap();
        });

        // self.inner.apply_post_execution_changes()
        todo!()
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
        use_preloaded_bundle: state.use_preloaded_bundle.clone(),
        block_hashes: state.block_hashes.clone(),
    }
}
