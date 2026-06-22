#![cfg_attr(not(test), warn(unused_crate_dependencies))]

//! World Chain EVM configuration.

use reth_evm::{
    block::BlockExecutionError,
    execute::{BlockBuilder, BlockBuilderOutcome},
};
use reth_node_builder::{BuilderContext, components::ExecutorBuilder};
pub use reth_optimism_evm::{
    L1BlockInfoError, OpBlockAssembler, OpBlockExecutionCtx, OpBlockExecutionError, OpEvm,
    OpEvmConfig, OpEvmFactory, OpNextBlockEnvAttributes, OpRethReceiptBuilder, OpTx, revm_spec,
    revm_spec_by_timestamp_after_bedrock,
};
use reth_optimism_primitives::OpPrimitives;
use reth_provider::StateProvider;
use revm_database::BundleState;
use world_chain_chainspec::WorldChainSpec;

pub mod execution;
pub mod metrics;
pub mod utils;

pub use metrics::{FlashblockExecutionMetrics, PayloadBuildStage};

pub trait BlockBuilderExt: BlockBuilder {
    /// Completes block building and returns the [`BlockBuilderOutcome`] plus the accumulated
    /// [`BundleState`].
    fn finish_with_bundle(
        self,
        state_provider: impl StateProvider,
        metrics: impl FlashblockExecutionMetrics,
    ) -> Result<(BlockBuilderOutcome<Self::Primitives>, BundleState), BlockExecutionError>;
}

/// World Chain EVM configuration.
pub type WorldChainEvmConfig<
    N = OpPrimitives,
    R = OpRethReceiptBuilder,
    EvmFactory = OpEvmFactory<OpTx>,
> = OpEvmConfig<WorldChainSpec, N, R, EvmFactory>;

/// Executor builder that constructs [`WorldChainEvmConfig`].
#[derive(Debug, Copy, Clone, Default)]
pub struct WorldChainExecutorBuilder;

impl<Node> ExecutorBuilder<Node> for WorldChainExecutorBuilder
where
    Node: reth_node_api::FullNodeTypes<
            Types: reth_node_api::NodeTypes<ChainSpec = WorldChainSpec, Primitives = OpPrimitives>,
        >,
{
    type EVM = WorldChainEvmConfig;

    async fn build_evm(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::EVM> {
        Ok(WorldChainEvmConfig::new(
            ctx.chain_spec(),
            OpRethReceiptBuilder::default(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_chain_config_defaults_to_world_chain_spec() {
        let chain_spec = WorldChainSpec::dev();
        let evm_config = WorldChainEvmConfig::optimism(chain_spec.clone());

        assert!(std::sync::Arc::ptr_eq(evm_config.chain_spec(), &chain_spec));
    }
}
