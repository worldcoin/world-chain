use reth_optimism_node::OpDAConfig;

use crate::args::WorldChainArgs;

#[derive(Debug, Clone)]
pub struct WorldChainNodeConfig {
    /// World Chain Specific CLI arguements
    pub args: WorldChainArgs,
    /// Data availability configuration for the OP builder.
    ///
    /// Used to throttle the size of the data availability payloads (configured by the batcher via
    /// the `miner_` api).
    ///
    /// By default no throttling is applied.
    pub da_config: OpDAConfig,
}
