use alloy_op_evm::{
    OpBlockExecutionCtx, OpBlockExecutor, OpBlockExecutorFactory,
    block::{OpTxEnv, receipt_builder::OpReceiptBuilder},
};

use reth_evm::{
    Database, Evm, FromRecoveredTx, FromTxWithEncoded,
    block::{BlockExecutionError, BlockExecutor, CommitChanges, StateDB},
    execute::{
        BasicBlockBuilder, BlockAssemblerInput, BlockBuilder, BlockBuilderOutcome, ExecutorTx,
    },
    op_revm::{OpHaltReason, OpSpecId},
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpRethReceiptBuilder;
use reth_optimism_node::OpBlockAssembler;
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use reth_primitives::{NodePrimitives, Recovered, RecoveredBlock, SealedHeader};
use reth_provider::StateProvider;
use revm::{
    DatabaseCommit,
    context::{BlockEnv, result::ExecutionResult},
    database::states::bundle_state::BundleRetention,
};
use revm_database::BundleState;
use std::{borrow::Cow, sync::Arc};

use crate::BlockBuilderExt;
/// A wrapper around the [`BasicBlockBuilder`] for flashblocks.
pub struct FlashblocksBlockBuilder<'a, N: NodePrimitives, Evm, R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt>  + 'static = OpRethReceiptBuilder> {
    pub inner: BasicBlockBuilder<
        'a,
        OpBlockExecutorFactory<R>,
        OpBlockExecutor<Evm, R, OpChainSpec>,
        OpBlockAssembler<OpChainSpec>,
        N,
    >,
}

impl<'a, N: NodePrimitives, Evm> FlashblocksBlockBuilder<'a, N, Evm> {
    /// Creates a new [`FlashblocksBlockBuilder`] with the given executor factory and assembler.
    pub fn new(
        ctx: OpBlockExecutionCtx,
        parent: &'a SealedHeader<N::BlockHeader>,
        executor: OpBlockExecutor<Evm, OpRethReceiptBuilder, OpChainSpec>,
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

impl<'a, N, E, R> BlockBuilder for FlashblocksBlockBuilder<'a, N, E, R>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt> + 'static,
    N: NodePrimitives<
            Receipt = OpReceipt,
            SignedTx = OpTransactionSigned,
            Block = alloy_consensus::Block<OpTransactionSigned>,
            BlockHeader = alloy_consensus::Header,
        >,
    E: Evm<
            DB: StateDB + DatabaseCommit + Database + 'a,
            Tx: FromRecoveredTx<OpTransactionSigned>
                    + FromTxWithEncoded<OpTransactionSigned>
                    + OpTxEnv,
            Spec = OpSpecId,
            HaltReason = OpHaltReason,
            BlockEnv = BlockEnv,
        >,
{
    type Primitives = N;
    type Executor = OpBlockExecutor<E, R, OpChainSpec>;

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
        _state: impl StateProvider,
    ) -> Result<BlockBuilderOutcome<N>, BlockExecutionError> {
        unimplemented!(
            "finish is not supported on FlashblocksBlockBuilder; use finish_with_bundle instead"
        )
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

impl<'a, N, E, R> BlockBuilderExt for FlashblocksBlockBuilder<'a, N, E, R>
where
    R: OpReceiptBuilder<Transaction = OpTransactionSigned, Receipt = OpReceipt> + 'static,
    N: NodePrimitives<
            Receipt = OpReceipt,
            SignedTx = OpTransactionSigned,
            Block = alloy_consensus::Block<OpTransactionSigned>,
            BlockHeader = alloy_consensus::Header,
        >,
    E: Evm<
            DB: StateDB + DatabaseCommit + Database + 'a,
            Tx: FromRecoveredTx<OpTransactionSigned>
                    + FromTxWithEncoded<OpTransactionSigned>
                    + OpTxEnv,
            Spec = OpSpecId,
            HaltReason = OpHaltReason,
            BlockEnv = BlockEnv,
        >,
{
    fn finish_with_bundle(
        self,
        state: impl StateProvider,
    ) -> Result<(BlockBuilderOutcome<Self::Primitives>, BundleState), BlockExecutionError> {
        let (evm, result) = self.inner.executor.finish()?;
        let (mut db, evm_env) = evm.finish();

        // merge all transitions into bundle state
        db.merge_transitions(BundleRetention::Reverts);

        // Flatten reverts into a single transition:
        // - per account: keep earliest `previous_status`
        // - per account: keep earliest non-`DoNothing` account-info revert
        // - per account+slot: keep earliest revert-to value
        // - per account: OR `wipe_storage`
        //
        // This keeps `bundle_state.reverts.len() == 1`, which matches the expectation that this
        // bundle represents a single block worth of changes even if we built multiple payloads.
        let flattened = crate::utils::flatten_reverts(&db.bundle_state().reverts);
        db.bundle_state_mut().reverts = flattened;

        // calculate the state root
        let hashed_state = state.hashed_post_state(db.bundle_state());
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
            OpBlockExecutorFactory<R>,
        >::new(
            evm_env,
            self.inner.ctx,
            self.inner.parent,
            transactions,
            &result,
            Cow::Borrowed(db.bundle_state()),
            &state,
            state_root,
        ))?;

        let block = RecoveredBlock::new_unhashed(block, senders);

        Ok((
            BlockBuilderOutcome {
                execution_result: result,
                hashed_state,
                trie_updates,
                block,
            },
            db.take_bundle(),
        ))
    }
}
