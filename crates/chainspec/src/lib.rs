#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod builder;
mod hardfork;
mod spec;
pub mod wip1001;

pub use builder::WorldChainSpecBuilder;
pub use hardfork::{WorldChainHardfork, WorldChainHardforks};
pub use spec::{
    JOVIAN_UPGRADE_TIMESTAMP_MAINNET, JOVIAN_UPGRADE_TIMESTAMP_SEPOLIA,
    TROPO_UPGRADE_TIMESTAMP_MAINNET, TROPO_UPGRADE_TIMESTAMP_SEPOLIA, WorldChainSpec,
};
pub use wip1001::{
    TROPO_WIP1001_PLACEHOLDER_CONFIG, TROPO_WIP1001_WORLD_MAINNET_CONFIG,
    TROPO_WIP1001_WORLD_SEPOLIA_CONFIG, Wip1001ActivationConfig,
    tropo_wip1001_parameters_for_chain,
};
