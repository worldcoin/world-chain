#![warn(unused_crate_dependencies)]

mod builder;
mod hardfork;
pub mod manifest;
mod spec;

pub use alloy_hardforks::Hardfork;
pub use builder::WorldChainSpecBuilder;
pub use hardfork::{WorldChainHardfork, WorldChainHardforks};
pub use manifest::{
    ChainConfig, Feature, ManifestCommitment, NetworkManifest, WORLD_CHAIN_HARDFORKS, hardfork_key,
};
pub use spec::{
    JOVIAN_UPGRADE_TIMESTAMP_MAINNET, JOVIAN_UPGRADE_TIMESTAMP_SEPOLIA, WorldChainSpec,
    parse_hardfork,
};
