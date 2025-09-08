use alloy_primitives::Address;
use clap::value_parser;
use reth_optimism_node::args::RollupArgs;
use rollup_boost::{
    ed25519_dalek::{SigningKey, VerifyingKey},
    parse_sk, parse_vk,
};

use crate::config::WorldChainNodeConfig;

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

impl TryFrom<WorldChainArgs> for WorldChainNodeConfig {
    type Error = eyre::Report;

    fn try_from(args: WorldChainArgs) -> Result<Self, Self::Error> {
        // Perform arg validation here for things clap can't do.

        Ok(WorldChainNodeConfig {
            args,
            da_config: Default::default(),
        })
    }
}

/// Parameters for pbh builder configuration
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "PBH Builder")]
#[group(requires = "builder.enabled")]
pub struct BuilderArgs {
    /// Whether block building is enabled
    #[arg(
        long = "builder.enabled",
        id = "builder.enabled",
        env,
        help = "Enable block building",
        required = false
    )]
    pub enabled: bool,
    /// Sets the max blockspace reserved for verified transactions. If there are not enough
    /// verified transactions to fill the capacity, the remaining blockspace will be filled with
    /// unverified transactions.
    /// This arg is a percentage of the total blockspace with the default set to 70 (ie 70%).
    #[arg(long = "builder.verified_blockspace_capacity", default_value = "70", value_parser = value_parser!(u8).range(0..=100))]
    pub verified_blockspace_capacity: u8,

    /// Sets the ERC-4337 EntryPoint Proxy contract address
    /// This contract is used to validate 4337 PBH bundles
    #[arg(
        long = "builder.pbh_entrypoint",
        default_value = "0x0000000000000000000000000000000000000000"
    )]
    pub pbh_entrypoint: Address,

    /// Sets the WorldID contract address.
    /// This contract is used to provide the latest merkle root on chain.
    #[arg(
        long = "builder.world_id",
        default_value = "0x0000000000000000000000000000000000000000"
    )]
    pub world_id: Address,

    /// Sets the ERC0-7766 Signature Aggregator contract address
    /// This contract signifies that a given bundle should receive priority inclusion if it passes validation
    #[arg(
        long = "builder.signature_aggregator",
        default_value = "0x0000000000000000000000000000000000000000"
    )]
    pub signature_aggregator: Address,

    /// Sets the private key of the builder
    #[arg(
        long = "builder.private_key",
        env = "BUILDER_PRIVATE_KEY",
        default_value = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
    )]
    pub private_key: String,
}

/// Flashblocks configuration
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Flashblocks")]
#[group(requires = "flashblocks.enabled")]
pub struct FlashblocksArgs {
    /// Whether flashblocks are enabled for this builder
    #[arg(
        long = "flashblocks.enabled",
        id = "flashblocks.enabled",
        env,
        help = "Enable flashblocks for this builder",
        required = false
    )]
    pub enabled: bool,

    #[arg(
        long = "flashblocks.authorizor_vk",
        env = "FLASHBLOCKS_AUTHORIZOR_VK", 
        value_parser = parse_vk,
        required = false,
    )]
    pub authorizor_vk: Option<VerifyingKey>,

    // TODO: Make this optional
    #[arg(
        long = "flashblocks.builder_sk", 
        env = "FLASHBLOCKS_BUILDER_SK", 
        value_parser = parse_sk,
        required = false,
    )]
    pub builder_sk: Option<SigningKey>,
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
    use clap::Parser;

    /// A helper type to parse Args more easily
    #[derive(Debug, Parser)]
    struct CommandParser {
        #[command(flatten)]
        pub builder: Option<BuilderArgs>,

        #[command(flatten)]
        pub flashblocks: Option<FlashblocksArgs>,
    }

    #[test]
    fn flashblocks_only() {
        let expected_args = FlashblocksArgs {
            enabled: true,
            authorizor_vk: None,
            builder_sk: Some(SigningKey::from_bytes(&[0; 32])),
        };

        let args = CommandParser::parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.builder_sk",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ]);

        assert_eq!(args.flashblocks.unwrap(), expected_args);
        assert!(args.builder.is_none())
    }

    #[test]
    fn builder_only() {
        let args = CommandParser::parse_from(["bin", "--builder.enabled"]);

        assert!(args.flashblocks.is_none());
        assert!(args.builder.is_some())
    }

    #[test]
    fn neither() {
        let args = CommandParser::parse_from(["bin"]);

        assert!(args.flashblocks.is_none());
        assert!(args.builder.is_none())
    }

    #[test]
    fn both() {
        let args = CommandParser::parse_from([
            "bin",
            "--builder.enabled",
            "--flashblocks.enabled",
            "--flashblocks.builder_sk",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ]);

        assert!(args.flashblocks.is_some());
        assert!(args.builder.is_some())
    }
}
