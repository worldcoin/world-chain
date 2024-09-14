use reth_chainspec::ChainSpec;
use reth_evm::{
    execute::{
        BatchExecutor, BlockExecutionError, BlockExecutionInput, BlockExecutionOutput,
        BlockExecutorProvider, Executor, ProviderError,
    },
    ConfigureEvm,
};
use reth_evm_optimism::OptimismEvmConfig;
use reth_evm_optimism::{OpBatchExecutor, OpBlockExecutor, OpExecutorProvider};
use reth_execution_types::ExecutionOutcome;
use reth_node_builder::components::ExecutorBuilder;
use reth_node_builder::{BuilderContext, FullNodeTypes, NodeTypes};
use reth_primitives::{BlockNumber, BlockWithSenders, Receipt};
use reth_prune_types::PruneModes;
use revm_primitives::db::Database;
use std::sync::Arc;

/// A regular optimism evm and executor builder.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct WcExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for WcExecutorBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = ChainSpec>>,
{
    type EVM = OptimismEvmConfig;
    type Executor = WcExecutorProvider<Self::EVM>;

    async fn build_evm(
        self,
        ctx: &BuilderContext<Node>,
    ) -> eyre::Result<(Self::EVM, Self::Executor)> {
        let chain_spec = ctx.chain_spec();
        let evm_config = OptimismEvmConfig::default();
        let executor = WcExecutorProvider::new(chain_spec, evm_config);

        Ok((evm_config, executor))
    }
}

/// Provides executors to execute regular ethereum blocks
#[derive(Debug, Clone)]
pub struct WcExecutorProvider<EvmConfig = OptimismEvmConfig> {
    inner: OpExecutorProvider<EvmConfig>,
}

impl<EvmConfig> WcExecutorProvider<EvmConfig>
where
    EvmConfig: ConfigureEvm,
{
    /// Create a new [`WcExecutorProvider`].
    pub fn new(chain_spec: Arc<ChainSpec>, evm_config: EvmConfig) -> Self {
        let inner = OpExecutorProvider::new(chain_spec, evm_config);
        Self { inner }
    }
}

impl<EvmConfig> BlockExecutorProvider for WcExecutorProvider<EvmConfig>
where
    EvmConfig: ConfigureEvm,
{
    // TODO: I'm kind of confused by trait bound on Executor.
    // It essentially doesn't let you change the type of BlockWithSenders.
    // Maybe I'm misunderstanding something...
    type Executor<DB: Database<Error: Into<ProviderError> + std::fmt::Display>> =
        WcBlockExecutor<EvmConfig, DB>;

    type BatchExecutor<DB: Database<Error: Into<ProviderError> + std::fmt::Display>> =
        WcBatchExecutor<EvmConfig, DB>;

    fn executor<DB>(&self, db: DB) -> Self::Executor<DB>
    where
        DB: Database<Error: Into<ProviderError> + std::fmt::Display>,
    {
        let inner = self.inner.executor(db);
        WcBlockExecutor { inner }
    }

    fn batch_executor<DB>(&self, db: DB) -> Self::BatchExecutor<DB>
    where
        DB: Database<Error: Into<ProviderError> + std::fmt::Display>,
    {
        let inner = self.inner.batch_executor(db);
        WcBatchExecutor { inner }
    }
}

// /// Helper container type for EVM with chain spec.
// #[derive(Debug, Clone)]
// struct WcEvmExecutor<EvmConfig> {
//     inner: OpEvmExecutor<EvmConfig>,
// }

#[derive(Debug)]
pub struct WcBlockExecutor<EvmConfig, DB> {
    inner: OpBlockExecutor<EvmConfig, DB>,
}

impl<EvmConfig, DB> Executor<DB> for WcBlockExecutor<EvmConfig, DB>
where
    EvmConfig: ConfigureEvm,
    DB: Database<Error: Into<ProviderError> + std::fmt::Display>,
{
    // TODO: Why can't we change this BlockWithSenders to WcBlockWithSenders?
    type Input<'a> = BlockExecutionInput<'a, BlockWithSenders>;
    // TODO: maybe we want some receipts for PBH here.
    type Output = BlockExecutionOutput<Receipt>;
    type Error = BlockExecutionError;

    /// Executes the block and commits the state changes.
    ///
    /// Returns the receipts of the transactions in the block.
    ///
    /// Returns an error if the block could not be executed or failed verification.
    ///
    /// State changes are committed to the database.
    fn execute(self, input: Self::Input<'_>) -> Result<Self::Output, Self::Error> {
        // TODO: This clone is probably bad.
        // let block: BlockWithSenders = input.block.clone().into();
        // let input = BlockExecutionInput {
        //     block: &block,
        //     total_difficulty: input.total_difficulty,
        // };

        self.inner.execute(input)

        // let BlockExecutionInput {
        //     block,
        //     total_difficulty,
        // } = input;
        // let (receipts, gas_used) = self
        //     .inner
        //     .execute_without_verification(block, total_difficulty)?;
        //
        // // NOTE: we need to merge keep the reverts for the bundle retention
        // self.inner.state.merge_transitions(BundleRetention::Reverts);
        //
        // Ok(BlockExecutionOutput {
        //     state: self.state.take_bundle(),
        //     receipts,
        //     requests: vec![],
        //     gas_used,
        // })
    }
}

/// An executor for a batch of blocks.
///
/// State changes are tracked until the executor is finalized.
#[derive(Debug)]
pub struct WcBatchExecutor<EvmConfig, DB> {
    inner: OpBatchExecutor<EvmConfig, DB>,
}

impl<EvmConfig, DB> BatchExecutor<DB> for WcBatchExecutor<EvmConfig, DB>
where
    EvmConfig: ConfigureEvm,
    DB: Database<Error: Into<ProviderError> + std::fmt::Display>,
{
    // type Input<'a> = BlockExecutionInput<'a, WcBlockWithSenders>;
    type Input<'a> = BlockExecutionInput<'a, BlockWithSenders>;
    type Output = ExecutionOutcome;
    type Error = BlockExecutionError;

    fn execute_and_verify_one(&mut self, input: Self::Input<'_>) -> Result<(), Self::Error> {
        // // TODO: This clone is probably bad.
        // let block: BlockWithSenders = input.block.clone().into();
        // let input = BlockExecutionInput {
        //     block: &block,
        //     total_difficulty: input.total_difficulty,
        // };
        self.inner.execute_and_verify_one(input)
    }

    fn finalize(self) -> Self::Output {
        self.inner.finalize()
    }

    fn set_tip(&mut self, tip: BlockNumber) {
        self.inner.set_tip(tip);
    }

    fn set_prune_modes(&mut self, prune_modes: PruneModes) {
        self.inner.set_prune_modes(prune_modes);
    }

    fn size_hint(&self) -> Option<usize> {
        self.inner.size_hint()
    }
}
