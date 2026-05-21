#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod builder;
mod hardfork;
mod spec;
pub mod wip1001;

pub use builder::WorldChainSpecBuilder;
pub use hardfork::{WorldChainHardfork, WorldChainHardforks};
pub use spec::{
    JOVIAN_UPGRADE_TIMESTAMP_MAINNET, JOVIAN_UPGRADE_TIMESTAMP_SEPOLIA,
    STRATO_UPGRADE_TIMESTAMP_MAINNET, STRATO_UPGRADE_TIMESTAMP_SEPOLIA, WorldChainSpec,
};
pub use wip1001::{
    STRATO_WIP1001_PLACEHOLDER_CONFIG, STRATO_WIP1001_WORLD_MAINNET_CONFIG,
    STRATO_WIP1001_WORLD_SEPOLIA_CONFIG, Wip1001ActivationConfig,
    strato_wip1001_parameters_for_chain,
};
