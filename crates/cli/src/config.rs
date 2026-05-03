use world_chain_primitives::OpBuilderConfig;

use crate::cli::WorldChainArgs;

/// Configuration for the flashblocks payload builder.
#[derive(Default, Debug, Clone)]
pub struct FlashblocksPayloadBuilderConfig {
    /// Inner Optimism payload builder configuration.
    pub inner: OpBuilderConfig,

    /// Whether to enable Block Access List (BAL) generation.
    pub bal_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct WorldChainNodeConfig {
    /// World Chain Specific CLI arguments
    pub args: WorldChainArgs,
    /// World Chain Payload Builder Configuration
    pub builder_config: FlashblocksPayloadBuilderConfig,
}
