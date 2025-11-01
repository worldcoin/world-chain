use alloy_op_evm::OpBlockExecutionCtx;
use alloy_primitives::Address;
use flashblocks_primitives::access_list::FlashblockAccessList;
use reth::revm::State;
use reth_evm::execute::{
    BasicBlockBuilder, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome, ExecutorTx,
};
use reth_evm::op_revm::{OpHaltReason, OpSpecId, OpTransaction};
use reth_evm::Evm;
use reth_evm::{
    block::{BlockExecutionError, BlockExecutor, CommitChanges},
    Database, FromRecoveredTx, FromTxWithEncoded,
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::{OpBlockAssembler, OpRethReceiptBuilder};
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use reth_primitives::SealedHeader;
use reth_primitives::{NodePrimitives, Recovered, RecoveredBlock};
use reth_provider::StateProvider;
use revm::context::result::ExecutionResult;
use revm::context::TxEnv;
use revm::database::states::bundle_state::BundleRetention;
use revm::database::states::reverts::Reverts;
use revm::database::BundleAccount;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::trace;

use crate::executor::bal_builder::BalBuilderBlockExecutor;
use crate::executor::factory::FlashblocksBlockExecutorFactory;

/// A wrapper around the [`BasicBlockBuilder`] for flashblocks.
pub struct FlashblocksBlockBuilder<'a, N: NodePrimitives, Evm> {
    pub inner: BasicBlockBuilder<
        'a,
        FlashblocksBlockExecutorFactory,
        BalBuilderBlockExecutor<Evm, OpRethReceiptBuilder, OpChainSpec>,
        OpBlockAssembler<OpChainSpec>,
        N,
    >,
}

impl<'a, N: NodePrimitives, Evm> FlashblocksBlockBuilder<'a, N, Evm> {
    /// Creates a new [`FlashblocksBlockBuilder`] with the given executor factory and assembler.
    pub fn new(
        ctx: OpBlockExecutionCtx,
        parent: &'a SealedHeader<N::BlockHeader>,
        executor: BalBuilderBlockExecutor<Evm, OpRethReceiptBuilder, OpChainSpec>,
        transactions: Vec<Recovered<N::SignedTx>>,
        chain_spec: Arc<OpChainSpec>,
    ) -> Self {
        Self {
            inner: BasicBlockBuilder {
                executor,
                assembler: OpBlockAssembler::new(chain_spec),
                ctx,
                parent,
                transactions,
            },
        }
    }
}

impl<'a, DB, N, E> BlockBuilder for FlashblocksBlockBuilder<'a, N, E>
where
    DB: Database + 'a,
    N: NodePrimitives<
        Receipt = OpReceipt,
        SignedTx = OpTransactionSigned,
        Block = alloy_consensus::Block<OpTransactionSigned>,
        BlockHeader = alloy_consensus::Header,
    >,
    E: Evm<
        DB = &'a mut State<DB>,
        Tx = OpTransaction<TxEnv>,
        Spec = OpSpecId,
        HaltReason = OpHaltReason,
    >,
    OpTransaction<TxEnv>:
        FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
{
    type Primitives = N;
    type Executor = BalBuilderBlockExecutor<E, OpRethReceiptBuilder, OpChainSpec>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.inner.apply_pre_execution_changes()
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutorTx<Self::Executor>,
        f: impl FnOnce(
            &ExecutionResult<<<Self::Executor as BlockExecutor>::Evm as Evm>::HaltReason>,
        ) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        self.inner.execute_transaction_with_commit_condition(tx, f)
    }

    fn finish(
        self,
        state: impl StateProvider,
    ) -> Result<BlockBuilderOutcome<N>, BlockExecutionError> {
        let (evm, result) = self.inner.executor.finish()?;
        let (db, evm_env) = evm.finish();

        // merge all transitions into bundle state
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

        // calculate the state root
        let hashed_state = state.hashed_post_state(&db.bundle_state);
        let (state_root, trie_updates) = state
            .state_root_with_updates(hashed_state.clone())
            .map_err(BlockExecutionError::other)?;

        let (transactions, senders) = self
            .inner
            .transactions
            .into_iter()
            .map(|tx| tx.into_parts())
            .unzip();

        let block = self.inner.assembler.assemble_block(BlockAssemblerInput::<
            '_,
            '_,
            FlashblocksBlockExecutorFactory,
        >::new(
            evm_env,
            self.inner.ctx,
            self.inner.parent,
            transactions,
            &result,
            &db.bundle_state,
            &state,
            state_root,
        ))?;

        let block = RecoveredBlock::new_unhashed(block, senders);

        Ok(BlockBuilderOutcome {
            execution_result: result,
            hashed_state,
            trie_updates,
            block,
        })
    }

    fn executor_mut(&mut self) -> &mut Self::Executor {
        self.inner.executor_mut()
    }

    fn executor(&self) -> &Self::Executor {
        self.inner.executor()
    }

    fn into_executor(self) -> Self::Executor {
        self.inner.into_executor()
    }
}

impl<'a, DB, N, E> FlashblocksBlockBuilder<'a, N, E>
where
    DB: Database + 'a,
    N: NodePrimitives<
        Receipt = OpReceipt,
        SignedTx = OpTransactionSigned,
        Block = alloy_consensus::Block<OpTransactionSigned>,
        BlockHeader = alloy_consensus::Header,
    >,
    E: Evm<
        DB = &'a mut State<DB>,
        Tx = OpTransaction<TxEnv>,
        Spec = OpSpecId,
        HaltReason = OpHaltReason,
    >,
    OpTransaction<TxEnv>:
        FromRecoveredTx<OpTransactionSigned> + FromTxWithEncoded<OpTransactionSigned>,
{
    // TODO: unify duplicate code
    pub fn finish_with_access_list(
        self,
        state: impl StateProvider,
    ) -> Result<(BlockBuilderOutcome<N>, FlashblockAccessList), BlockExecutionError> {
        let (evm, result, access_list, min_tx_index, max_tx_index) =
            self.inner.executor.finish_with_access_list()?;

        let (db, evm_env) = evm.finish();

        // merge all transitions into bundle state
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

        // Write the expected bundle state to a JSON
        let expected_json = serde_json::to_string_pretty(&db.bundle_state.state)
            .map_err(BlockExecutionError::other)?;
        // std::fs::write("expected_bundle_state.json", json).map_err(BlockExecutionError::other)?;

        // calculate the state root``
        let hashed_state = state.hashed_post_state(&db.bundle_state);
        let (state_root, trie_updates) = state
            .state_root_with_updates(hashed_state.clone())
            .map_err(BlockExecutionError::other)?;

        let (transactions, senders) = self
            .inner
            .transactions
            .into_iter()
            .map(|tx| tx.into_parts())
            .unzip();

        let block = self.inner.assembler.assemble_block(BlockAssemblerInput::<
            '_,
            '_,
            FlashblocksBlockExecutorFactory,
        >::new(
            evm_env,
            self.inner.ctx,
            self.inner.parent,
            transactions,
            &result,
            &db.bundle_state,
            &state,
            state_root,
        ))?;

        let block = RecoveredBlock::new_unhashed(block, senders);

        let access_list_before = access_list.clone();
        // TODO: Remove debug traces
        trace!(target: "test_target", "recorded transitions for tx index range: {} - {}, transactions length {:#?}", min_tx_index, max_tx_index, block.body().transactions().count());
        trace!(target: "test_target", "finished execution with access list length {:#?}", access_list_before.access_list.changes.len());

        let access_list_after = access_list.access_list;
        let access_list_bundle: HashMap<Address, BundleAccount> = access_list_after.clone().into();

        // // Write the access list to a JSON
        let got_json = serde_json::to_string_pretty(&access_list_bundle)
            .map_err(BlockExecutionError::other)?;
        // std::fs::write("flashblock_access_list_bundle.json", json)
        //     .map_err(BlockExecutionError::other)?;

        trace!(target: "test_target", "built final access list length {:#?}", access_list_after.changes.len());
        let block_number = block.header().number;

        std::fs::write(format!("expected_{}", block_number), expected_json).unwrap();

        std::fs::write(
            format!("flashblock_access_list_bundle_{}", block_number),
            got_json,
        )
        .unwrap();

        Ok((
            BlockBuilderOutcome {
                execution_result: result,
                hashed_state,
                trie_updates,
                block,
            },
            access_list_after,
        ))
    }
}
