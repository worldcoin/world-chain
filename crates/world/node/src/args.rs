use std::net::IpAddr;

use alloy_primitives::Address;
use clap::value_parser;
use reth_optimism_node::args::RollupArgs;
use rollup_boost::{
    ed25519_dalek::{SigningKey, VerifyingKey},
    parse_sk, parse_vk,
};

use crate::node::WorldChainNodeConfig;

#[derive(Debug, Clone, Default, clap::Args)]
#[command(next_help_heading = "PBH Builder")]
pub struct WorldChainArgs {
    /// op rollup args
    #[command(flatten)]
    pub rollup_args: RollupArgs,

    /// Sets the max blockspace reserved for verified transactions. If there are not enough
    /// verified transactions to fill the capacity, the remaining blockspace will be filled with
    /// unverified transactions.
    /// This arg is a percentage of the total blockspace with the default set to 70 (ie 70%).
    #[arg(long = "builder.verified_blockspace_capacity", default_value = "70", value_parser = value_parser!(u8).range(0..=100))]
    pub verified_blockspace_capacity: u8,

    /// Sets the ERC-4337 EntryPoint Proxy contract address
    /// This contract is used to validate 4337 PBH bundles
    #[arg(long = "builder.pbh_entrypoint")]
    pub pbh_entrypoint: Address,

    /// Sets the WorldID contract address.
    /// This contract is used to provide the latest merkle root on chain.
    #[arg(long = "builder.world_id")]
    pub world_id: Address,

    /// Sets the ERC0-7766 Signature Aggregator contract address
    /// This contract signifies that a given bundle should receive priority inclusion if it passes validation
    #[arg(long = "builder.signature_aggregator")]
    pub signature_aggregator: Address,

    /// Sets the private key of the builder
    #[arg(long = "builder.private_key", env = "BUILDER_PRIVATE_KEY")]
    pub builder_private_key: String,

    /// Flashblock args
    #[command(flatten)]
    pub flashblocks_args: FlashblocksArgs,
}

/// Parameters for Flashblocks configuration
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Flashblocks")]
pub struct FlashblocksArgs {
    /// Whether flashblocks are enabled for this builder
    #[arg(
        long = "flashblock.enabled",
        env,
        default_value_t = false,
        help = "Enable flashblocks for this builder"
    )]
    pub flashblocks_enabled: bool,
    /// The payload building interval in milliseconds.
    #[arg(long = "flashblock.block_time", env, default_value = "1000")]
    pub flashblock_block_time: u64,

    /// Interval in milliseconds to wait before computing the next pending block.
    #[arg(long = "flashblock.interval", env, default_value = "200")]
    pub flashblock_interval: u64,

    /// The host to bind the Flashblocks WebSocket server to.
    #[arg(long = "flashblock.host", env, default_value = "127.0.0.1")]
    pub flashblock_host: IpAddr,

    /// The port to bind the Flashblocks WebSocket server to.
    #[arg(long = "flashblock.port", env, default_value = "9002")]
    pub flashblock_port: u16,

    #[arg(
        long = "flashblock.authorizor_sk",
        env = "FLASHBLOCKS_AUTHORIZOR_VK", 
        value_parser = parse_vk,
        required = false,
    )]
    pub flashblocks_authorizor_vk: Option<VerifyingKey>,

    #[arg(long = "flashblock.builder_sk", 
        env = "FLASHBLOCKS_BUILDER_SK", 
        value_parser = parse_sk,
        required = false,
    )]
    pub flashblocks_builder_sk: SigningKey,
}

impl Default for FlashblocksArgs {
    fn default() -> Self {
        Self {
            flashblocks_enabled: false,
            flashblock_block_time: 1000,
            flashblock_interval: 200,
            flashblock_host: "127.0.0.1".parse().unwrap(),
            flashblock_port: 9002,
            flashblocks_authorizor_vk: None,
            flashblocks_builder_sk: SigningKey::from_bytes(&[0; 32]),
        }
    }
}

pub enum NodeContextType {
    Basic,
    Flashblocks,
}

impl From<WorldChainNodeConfig> for NodeContextType {
    fn from(config: WorldChainNodeConfig) -> Self {
        match config.args.flashblocks_args.flashblocks_enabled {
            true => Self::Flashblocks,
            false => Self::Basic,
        }
    }
}
    

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Args, Parser};
    use rollup_boost::ed25519_dalek::SigningKey;

    /// A helper type to parse Args more easily
    #[derive(Parser)]
    struct CommandParser<T: Args> {
        #[command(flatten)]
        args: T,
    }

    #[test]
    fn parse_args() {
        let expected_args = FlashblocksArgs {
            flashblock_block_time: 1,
            flashblock_interval: 200,
            flashblock_port: 9002,
            flashblock_host: "127.0.0.1".parse().unwrap(),
            flashblocks_authorizor_vk: None,
            flashblocks_builder_sk: SigningKey::from_bytes(&[0; 32]),
            flashblocks_enabled: false,
        };

        let args = CommandParser::<FlashblocksArgs>::parse_from([
            "bin",
            "--flashblock.block_time",
            "1",
            "--flashblock.interval",
            "200",
            "--flashblock.builder_sk",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ])
        .args;

        assert_eq!(args, expected_args);
    }
}
