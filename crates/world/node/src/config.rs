use reth_optimism_payload_builder::config::OpBuilderConfig;

use crate::args::WorldChainArgs;

#[derive(Debug, Clone)]
pub struct WorldChainNodeConfig {
    pub args: WorldChainArgs,
    pub builder_config: OpBuilderConfig,
}
