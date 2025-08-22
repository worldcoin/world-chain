use alloy_primitives::Address;
use clap::value_parser;
use reth_optimism_node::args::RollupArgs;
use rollup_boost::{
    ed25519_dalek::{SigningKey, VerifyingKey},
    parse_sk, parse_vk,
};

use crate::node::WorldChainNodeConfig;

#[derive(Debug, Clone, Default, clap::Args)]
pub struct WorldChainArgs {
    /// op rollup args
    #[command(flatten)]
    pub rollup: RollupArgs,

    /// Flashblock args
    // TODO: Make this optional
    #[command(flatten)]
    pub builder: BuilderArgs,

    /// Flashblock args
    #[command(flatten)]
    pub flashblocks: Option<FlashblocksArgs>,
}

/// Parameters for pbh builder configuration
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "PBH Builder")]
#[group(requires = "enabled")]
pub struct BuilderArgs {
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
    pub private_key: String,
}

/// Flashblocks configuration
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Flashblocks")]
#[group(requires = "enabled")]
pub struct FlashblocksArgs {
    /// Whether flashblocks are enabled for this builder
    #[arg(
        long = "flashblocks.enabled",
        env,
        help = "Enable flashblocks for this builder"
    )]
    pub enabled: bool,

    #[arg(
        long = "flashblocks.authorizor_vk",
        env = "FLASHBLOCKS_AUTHORIZOR_VK", 
        value_parser = parse_vk,
        required = false,
    )]
    pub authorizor_vk: Option<VerifyingKey>,

    #[arg(long = "flashblocks.builder_sk", 
        env = "FLASHBLOCKS_BUILDER_SK", 
        value_parser = parse_sk,
        required = false,
    )]
    pub builder_sk: SigningKey,
}

pub enum NodeContextType {
    Basic,
    Flashblocks,
}

impl From<WorldChainNodeConfig> for NodeContextType {
    fn from(config: WorldChainNodeConfig) -> Self {
        match config.args.flashblocks.is_some() {
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
            authorizor_vk: None,
            builder_sk: SigningKey::from_bytes(&[0; 32]),
            enabled: true,
        };

        let args = CommandParser::<FlashblocksArgs>::parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.builder_sk",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ])
        .args;

        assert_eq!(args, expected_args);
    }
}
