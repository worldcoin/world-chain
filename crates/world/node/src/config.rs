use flashblocks_builder::FlashblocksPayloadBuilderConfig;

use crate::args::WorldChainArgs;

#[derive(Debug, Clone)]
pub struct WorldChainNodeConfig {
    /// World Chain Specific CLI arguements
    pub args: WorldChainArgs,
    /// World Chain Payload Builder Configeration
    pub builder_config: FlashblocksPayloadBuilderConfig,
}
