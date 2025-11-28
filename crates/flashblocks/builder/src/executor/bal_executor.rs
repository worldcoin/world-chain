use alloy_consensus::{BlockHeader, Header, Transaction};
use alloy_eips::Decodable2718;
use alloy_op_evm::{OpBlockExecutionCtx, block::receipt_builder::OpReceiptBuilder};
use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_rpc_types_engine::PayloadId;
use eyre::eyre::{OptionExt, eyre};
use flashblocks_primitives::primitives::ExecutionPayloadFlashblockDeltaV1;
use op_alloy_consensus::OpTxEnvelope;
use rayon::iter::{IndexedParallelIterator, ParallelIterator};
use reth::revm::{State, database::StateProviderDatabase};
use reth_chain_state::ExecutedBlock;
use reth_evm::{
    ConfigureEvm, Database, Evm as EvmTrait, EvmEnv, EvmEnvFor, EvmFactory, EvmFactoryFor, EvmFor,
    block::BlockExecutionError,
    execute::{BlockAssembler, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome},
    op_revm::{OpSpecId, OpTransaction},
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_node::OpBuiltPayload;
use reth_optimism_primitives::{OpPrimitives, OpReceipt, OpTransactionSigned};
use reth_payload_primitives::BuiltPayload;
use reth_primitives::{Recovered, RecoveredBlock, SealedHeader, transaction::SignedTransaction};
use reth_provider::{BlockExecutionResult, ExecutionOutcome, StateProvider};
use reth_trie_common::{HashedPostState, KeccakKeyHasher, updates::TrieUpdates};
use revm::{
    DatabaseRef,
    context::{BlockEnv, TxEnv},
    database::{BundleAccount, BundleState},
    inspector::NoOpInspector,
};
use revm_database_interface::WrapDatabaseRef;
use std::sync::Arc;
use tracing::error;

use crate::{
    access_list::{BlockAccessIndex, FlashblockAccessListConstruction},
    assembler::FlashblocksBlockAssembler,
    block_builder::FlashblocksBlockBuilder,
    executor::{
        BalExecutorError, BalValidationError, bal_builder::BalBuilderBlockExecutor,
        factory::FlashblocksBlockExecutorFactory,
    },
};

/// Result of computing the state root from a bundle state.
pub struct StateRootResult {
    pub state_root: B256,
    pub trie_updates: TrieUpdates,
    pub hashed_state: HashedPostState,
}

/// Result of verifying a flashblock delta against the current state.
pub struct VerifyBlockResult<R: OpReceiptBuilder> {
    pub bundle_state: BundleState,
    pub execution_result: BlockExecutionResult<R::Receipt>,
    pub evm_env: EvmEnv<OpSpecId>,
    pub transactions: Vec<Recovered<OpTransactionSigned>>,
    pub execution_context: OpBlockExecutionCtx,
    pub fees: u128,
}

pub struct CommittedState<R: OpReceiptBuilder = OpRethReceiptBuilder> {
    pub gas_used: u64,
    pub fees: U256,
    pub bundle: BundleState,
    pub receipts: Vec<(BlockAccessIndex, R::Receipt)>,
    pub transactions: Vec<(BlockAccessIndex, Recovered<R::Transaction>)>,
}

impl<R: OpReceiptBuilder> CommittedState<R> {
    pub fn take_bundle(&mut self) -> BundleState {
        core::mem::take(&mut self.bundle)
    }

    pub fn start_tx_index(&self) -> BlockAccessIndex {
        self.transactions.len() as BlockAccessIndex + 1
    }

    pub fn min_access_index(&self) -> BlockAccessIndex {
        if self.start_tx_index() == 1 {
            0
        } else {
            self.start_tx_index()
        }
    }
}

impl<R: OpReceiptBuilder + Default> Default for CommittedState<R> {
    fn default() -> Self {
        Self {
            gas_used: 0,
            fees: U256::ZERO,
            bundle: BundleState::default(),
            receipts: vec![],
            transactions: vec![],
        }
    }
}

pub struct BalExecutionState<R: OpReceiptBuilder> {
    pub committed_state: CommittedState<R>,
    pub transactions: Vec<(BlockAccessIndex, Recovered<OpTransactionSigned>)>,
    pub evm_env: EvmEnvFor<OpEvmConfig>,
    pub evm_config: OpEvmConfig,
    pub execution_context: OpBlockExecutionCtx,
    pub executor_transactions: Vec<(BlockAccessIndex, Recovered<OpTransactionSigned>)>,
}

impl<R> BalExecutionState<R>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt> + Default,
{
    pub fn new_bal_execution_state(
        committed_state: CommittedState<R>,
        evm_env: EvmEnvFor<OpEvmConfig>,
        evm_config: OpEvmConfig,
        execution_context: OpBlockExecutionCtx,
        diff: ExecutionPayloadFlashblockDeltaV1,
    ) -> Result<BalExecutionState<R>, BalExecutorError> {
        let transactions: Vec<(BlockAccessIndex, Recovered<OpTransactionSigned>)> =
            BalBlockValidator::<R>::decode_transactions(
                &diff.transactions,
                committed_state.start_tx_index(),
            )?;

        Ok(BalExecutionState {
            committed_state,
            transactions: Vec::new(),
            evm_env,
            evm_config,
            execution_context,
            executor_transactions: transactions,
        })
    }

    pub fn all_transactions_iter(&self) -> impl Iterator<Item = Recovered<OpTransactionSigned>> {
        let mut all_transactions = self.committed_state.transactions.clone();
        all_transactions.extend(self.executor_transactions.clone());
        all_transactions.into_iter().map(|(_, tx)| tx)
    }

    pub fn executor_transactions_iter(
        &self,
    ) -> impl Iterator<Item = &'_ Recovered<OpTransactionSigned>> {
        self.executor_transactions.iter().map(|(_, tx)| tx)
    }

    pub fn committed_transactions_iter(
        &self,
    ) -> impl Iterator<Item = &'_ Recovered<OpTransactionSigned>> {
        self.committed_state.transactions.iter().map(|(_, tx)| tx)
    }
}

impl<'a, R> BalExecutionState<R>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt> + Default,
    EvmFactoryFor<OpEvmConfig>:
        EvmFactory<Spec = OpSpecId, Tx = OpTransaction<TxEnv>, BlockEnv = BlockEnv>,
{
    pub fn basic_executor<DB: Database + Send + Sync>(
        &mut self,
        spec: Arc<OpChainSpec>,
        receipt_builder: R,
        state: &'a mut State<DB>,
    ) -> BalBuilderBlockExecutor<EvmFor<OpEvmConfig, &'a mut State<DB>, NoOpInspector>, R> {
        let starting_tx_index = self.committed_state.transactions.len() as BlockAccessIndex + 1;

        let min_access_index = if starting_tx_index == 1 {
            0
        } else {
            starting_tx_index
        };

        let evm = self
            .evm_config
            .evm_factory()
            .create_evm(state, self.evm_env.clone());

        let receipts: Vec<_> = self
            .committed_state
            .receipts
            .iter()
            .cloned()
            .map(|(_, r)| r)
            .collect();

        self.executor_at_index(
            spec,
            receipt_builder,
            evm,
            starting_tx_index,
            FlashblockAccessListConstruction::default(),
        )
        .with_min_tx_index(min_access_index)
        .with_receipts(receipts)
        .with_gas_used(self.committed_state.gas_used)
    }

    pub fn executor_at_index<DB: Database + Send + Sync + 'a>(
        &self,
        spec: Arc<OpChainSpec>,
        receipt_builder: R,
        evm: EvmFor<OpEvmConfig<Arc<OpChainSpec>>, &'a mut State<DB>, NoOpInspector>,
        index: BlockAccessIndex,
        access_list: FlashblockAccessListConstruction,
    ) -> BalBuilderBlockExecutor<
        EvmFor<OpEvmConfig<Arc<OpChainSpec>>, &'a mut State<DB>, NoOpInspector>,
        R,
    > {
        BalBuilderBlockExecutor::new(evm, self.execution_context.clone(), spec, receipt_builder)
            .with_block_access_index(index)
            .with_access_list(access_list)
    }

    pub fn state_for_db<DB: Database + Send + Sync + 'a>(&self, db: DB) -> State<DB> {
        State::builder()
            .with_database(db)
            .with_bundle_prestate(self.committed_state.bundle.clone())
            .with_bundle_update()
            .build()
    }

    pub fn state_for_ref_db<DB: DatabaseRef + Send + Sync + 'a>(
        &self,
        db: DB,
    ) -> State<WrapDatabaseRef<DB>> {
        State::builder()
            .with_database_ref(db)
            .with_bundle_prestate(self.committed_state.bundle.clone())
            .with_bundle_update()
            .build()
    }
}

/// A Block Executor for Optimism that can load pre state from previous flashblocks
///
/// A Block Access List is used to improve execution speed
///
/// 'BlockExecutor' trait is not flexible enough for our purposes.
pub struct BalBlockValidator<R: OpReceiptBuilder + Default = OpRethReceiptBuilder> {
    execution_state: BalExecutionState<R>,
    chain_spec: Arc<OpChainSpec>,
    receipt_builder: R,
}

impl<R> BalBlockValidator<R>
where
    R: OpReceiptBuilder<Transaction = OpTxEnvelope, Receipt = OpReceipt> + Default,
{
    /// Creates a new [`FlashblocksBlockExecutor`].
    pub fn new(
        spec: Arc<OpChainSpec>,
        execution_context: OpBlockExecutionCtx,
        committed_payload: Option<OpBuiltPayload>,
        evm_env: EvmEnv<OpSpecId>,
        evm_config: OpEvmConfig,
        receipt_builder: R,
        diff: ExecutionPayloadFlashblockDeltaV1,
    ) -> Result<Self, BalExecutorError> {
        let state = if let Some(payload) = committed_payload {
            CommittedState::<R>::try_from(payload)?
        } else {
            CommittedState::<R>::default()
        };

        let bal_execution_state = BalExecutionState::new_bal_execution_state(
            state,
            evm_env,
            evm_config,
            execution_context,
            diff,
        )?;

        Ok(Self {
            execution_state: bal_execution_state,
            chain_spec: spec,
            receipt_builder,
        })
    }

    /// Verifies and executes a given [`ExecutionPayloadFlashblockDeltaV1`] on top of an option [`OpBuiltPayload`].
    pub(crate) fn verify_block(
        &mut self,
        state_provider: impl StateProvider + Clone + 'static,
        diff: ExecutionPayloadFlashblockDeltaV1,
    ) -> Result<VerifyBlockResult<R>, BalExecutorError>
    where
        Self: Sized,
        R: Clone + Send + Sync + 'static,
    {
        let db = StateProviderDatabase::new(state_provider.clone());

        let expected_access_list_data = diff
            .access_list_data
            .ok_or_eyre("Access list data must be provided on the diff")?;

        let mut state = self.execution_state.state_for_db(db);

        let executor = self.execution_state.basic_executor(
            self.chain_spec.clone(),
            self.receipt_builder.clone(),
            &mut state,
        );

        let executor_state = Arc::new(&self.execution_state);

        let parallel_output = executor.execute_block_parallel(
            executor_state,
            expected_access_list_data,
            state_provider,
        )?;

        Ok(VerifyBlockResult {
            bundle_state: parallel_output.bundle_state,
            execution_result: parallel_output.execution_result,
            evm_env: parallel_output.evm_env,
            transactions: self.execution_state.all_transactions_iter().collect(),
            execution_context: parallel_output.execution_context,
            fees: parallel_output.fees,
        })
    }

    /// Decodes transactions from raw bytes and recovers signer addresses.
    pub fn decode_transactions(
        encoded_transactions: &[Bytes],
        start_index: BlockAccessIndex,
    ) -> Result<Vec<(BlockAccessIndex, Recovered<OpTransactionSigned>)>, BalExecutorError> {
        encoded_transactions
            .iter()
            .enumerate()
            .map(|(i, tx)| {
                let tx_envelope = OpTransactionSigned::decode_2718(&mut tx.as_ref())
                    .map_err(|e| eyre!("failed to decode transaction: {e}"))?;

                let recovered = tx_envelope.try_clone_into_recovered().map_err(|e| {
                    eyre!("failed to recover transaction from signed envelope: {e}")
                })?;

                Ok((start_index + i as BlockAccessIndex, recovered))
            })
            .collect()
    }

    /// Executes a [`ExecutionPayloadFlashblockDeltaV1`] on top of an optional [`OpBuiltPayload`].
    /// And computes the resulting [`OpBuiltPayload`].
    ///
    /// # Errors
    /// Returns an error if the provided BAL in `diff` does not match the computed BAL from execution.
    pub fn validate_and_execute_diff_parallel(
        mut self,
        state_provider: Arc<dyn StateProvider>,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent_header: &SealedHeader<Header>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError>
    where
        R: Clone + Send + Sync + 'static,
    {
        let bal_state: alloy_primitives::map::HashMap<Address, BundleAccount> = diff
            .clone()
            .access_list_data
            .ok_or(BalExecutorError::MissingBlockAccessList)?
            .access_list
            .into();

        let mut bundle_state = self.execution_state.committed_state.bundle.clone();
        bundle_state.extend_state(bal_state.into_iter().collect());

        // Clone bundle_state for state root computation since into_iter() moves the state
        let bundle_state_for_root = bundle_state.clone();

        let (verify_result, state_root_result) = rayon::join(
            || self.verify_block(state_provider.clone(), diff.clone()),
            || {
                compute_state_root(
                    state_provider.clone(),
                    &bundle_state_for_root.state.into_iter().collect(),
                )
            },
        );

        let verify_result = verify_result?;
        let state_root_result = state_root_result?;

        if state_root_result.state_root != diff.state_root {
            error!(
                target: "flashblocks::bal_executor",
                expected = %diff.state_root,
                got = %state_root_result.state_root,
                "State root mismatch after executing flashblock delta"
            );

            // Create error with explicit state data for reliable diagnostic capture
            return Err(BalValidationError::StateRootMismatch {
                expected: diff.state_root,
                got: state_root_result.state_root,
            }
            .into());
        }

        let (transactions, senders) = verify_result
            .transactions
            .into_iter()
            .map(|tx| tx.into_parts())
            .unzip();
        let assembler = FlashblocksBlockAssembler::new(self.chain_spec.clone());

        let block = assembler.assemble_block(BlockAssemblerInput::<
            FlashblocksBlockExecutorFactory,
        >::new(
            verify_result.evm_env,
            verify_result.execution_context,
            parent_header,
            transactions,
            &verify_result.execution_result,
            &verify_result.bundle_state,
            &state_provider.clone(),
            state_root_result.state_root,
        ))?;

        let block = RecoveredBlock::new_unhashed(block, senders);

        assert_eq!(
            block.sealed_block().receipts_root(),
            diff.receipts_root,
            "Receipts root mismatch after assembling block from execution result"
        );
        assert_eq!(
            block.sealed_block().hash(),
            diff.block_hash,
            "Block hash mismatch after assembling block from execution result"
        );

        // Construct the built payload
        let outcome = BlockBuilderOutcome::<OpPrimitives> {
            execution_result: verify_result.execution_result,
            hashed_state: state_root_result.hashed_state,
            trie_updates: state_root_result.trie_updates,
            block,
        };

        let sealed_block = Arc::new(outcome.block.sealed_block().clone());

        // Use the bundle_state computed from the access list (which was used for state root)
        // instead of verify_result.bundle_state which may be incomplete from parallel execution
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
            self.execution_state.committed_state.fees + U256::from(verify_result.fees),
            Some(executed_block),
        ))
    }

    pub fn validate_and_execute_diff_linear(
        mut self,
        state_provider: Arc<impl StateProvider + 'static>,
        parent_header: &SealedHeader<Header>,
        payload_id: PayloadId,
    ) -> Result<OpBuiltPayload, BalExecutorError>
    where
        R: Clone,
    {
        let db = StateProviderDatabase::new(state_provider.clone());
        let executor_transactions = self.execution_state.executor_transactions.clone();
        let mut state = self.execution_state.state_for_db(db);

        let chain_spec = self.chain_spec.clone();
        let receipt_builder = self.receipt_builder.clone();

        let executor =
            self.execution_state
                .basic_executor(chain_spec.clone(), receipt_builder, &mut state);

        let mut block_builder = FlashblocksBlockBuilder::new(
            self.execution_state.execution_context.clone(),
            parent_header,
            executor,
            self.execution_state
                .committed_transactions_iter()
                .cloned()
                .collect(),
            chain_spec.clone(),
        );

        if self.execution_state.committed_state.min_access_index() == 0 {
            block_builder.apply_pre_execution_changes()?;
        }

        let mut cumulative_fees = self.execution_state.committed_state.fees;

        for transaction in executor_transactions {
            let (_, tx) = transaction;
            let is_deposit = tx.is_deposit();
            let gas_used = block_builder.execute_transaction(tx.clone())?;

            if !is_deposit {
                let miner_fee = &tx
                    .effective_tip_per_gas(self.execution_state.evm_env.block_env().basefee)
                    .expect("fee is always valid; execution succeeded");
                cumulative_fees += U256::from(*miner_fee) * U256::from(gas_used);
            }
        }

        let build_outcome: BlockBuilderOutcome<OpPrimitives> =
            block_builder.finish(state_provider.clone())?;

        // 7. Seal the block
        let BlockBuilderOutcome {
            execution_result,
            block,
            hashed_state,
            trie_updates,
        } = build_outcome;

        let sealed_block = Arc::new(block.sealed_block().clone());

        let execution_outcome = ExecutionOutcome::new(
            state.take_bundle(),
            vec![execution_result.receipts.clone()],
            block.number(),
            Vec::new(),
        );

        // create the executed block data
        let executed_block = ExecutedBlock {
            recovered_block: Arc::new(block),
            execution_output: Arc::new(execution_outcome),
            hashed_state: Arc::new(hashed_state),
            trie_updates: Arc::new(trie_updates),
        };

        let payload = OpBuiltPayload::new(
            payload_id,
            sealed_block,
            cumulative_fees,
            Some(executed_block),
        );

        Ok(payload)
    }
}

pub fn compute_state_root(
    state_provider: Arc<dyn StateProvider>,
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

impl<R> TryFrom<OpBuiltPayload> for CommittedState<R>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt> + Default,
{
    type Error = BalExecutorError;

    fn try_from(value: OpBuiltPayload) -> Result<Self, Self::Error> {
        let executed_block = value
            .executed_block()
            .ok_or(BalExecutorError::MissingExecutedBlock)?;

        let gas_used = executed_block.recovered_block().gas_used();
        let bundle = executed_block.execution_outcome().bundle.clone();
        let fees = value.fees();

        let transactions: Vec<_> = executed_block
            .recovered_block()
            .clone_transactions_recovered()
            .enumerate()
            .map(|(index, tx)| (index as BlockAccessIndex, tx))
            .collect();

        let receipts: Vec<_> = executed_block
            .execution_outcome()
            .receipts()
            .iter()
            .flatten()
            .cloned()
            .enumerate()
            .map(|(index, r)| (index as BlockAccessIndex, r))
            .collect();

        Ok(Self {
            transactions,
            receipts,
            gas_used,
            fees,
            bundle,
        })
    }
}
