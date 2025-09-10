use std::str::FromStr;

use alloy_primitives::Address;
use alloy_signer_local::PrivateKeySigner;
use clap::{value_parser, ArgGroup};
use reth::chainspec::NamedChain;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::args::RollupArgs;
use rollup_boost::{
    ed25519_dalek::{SigningKey, VerifyingKey},
    parse_sk, parse_vk,
};
use tracing::warn;

use crate::config::WorldChainNodeConfig;

#[derive(Debug, Clone, clap::Args)]
pub struct WorldChainArgs {
    /// op rollup args
    #[command(flatten)]
    pub rollup: RollupArgs,

    /// Pbh args
    #[command(flatten)]
    pub pbh: PbhArgs,

    /// Builder args
    #[command(flatten)]
    pub builder: BuilderArgs,

    /// Flashblock args
    #[command(flatten)]
    pub flashblocks: Option<FlashblocksArgs>,
}

impl WorldChainArgs {
    pub fn into_config(mut self, spec: &OpChainSpec) -> eyre::Result<WorldChainNodeConfig> {
        // Perform arg validation here for things clap can't do.
        match spec.chain.named() {
            Some(NamedChain::World) => {
                if self.pbh.entrypoint == Address::default() {
                    self.pbh.entrypoint =
                        Address::from_str("0x0000000000A21818Ee9F93BB4f2AAad305b5397C")?;
                }
                if self.pbh.world_id == Address::default() {
                    self.pbh.world_id =
                        Address::from_str("0x047eE5313F98E26Cc8177fA38877cB36292D2364")?;
                }
                if self.pbh.signature_aggregator == Address::default() {
                    self.pbh.signature_aggregator =
                        Address::from_str("0xd21306C75C956142c73c0C3BAb282Be68595081E")?;
                }
            }
            Some(NamedChain::WorldSepolia) => {
                if self.pbh.entrypoint == Address::default() {
                    self.pbh.entrypoint =
                        Address::from_str("0x0000000000A21818Ee9F93BB4f2AAad305b5397C")?;
                }
                if self.pbh.world_id == Address::default() {
                    self.pbh.world_id =
                        Address::from_str("0xE177F37AF0A862A02edFEa4F59C02668E9d0aAA4")?;
                }
                if self.pbh.signature_aggregator == Address::default() {
                    self.pbh.signature_aggregator =
                        Address::from_str("0x8af27Ee9AF538C48C7D2a2c8BD6a40eF830e2489")?;
                }
            }
            _ => {
                if self.pbh.entrypoint == Address::default() {
                    warn!("missing `--builder.pbh_entrypoint`, using default")
                }
                if self.pbh.world_id == Address::default() {
                    warn!("missing `--builder.world_id`, using default")
                }
                if self.pbh.signature_aggregator == Address::default() {
                    warn!("missing `--builder.signature_aggregator`, using default")
                }
            }
        }

        Ok(WorldChainNodeConfig {
            args: self,
            da_config: Default::default(),
        })
    }
}

/// Parameters for pbh builder configuration
#[derive(Debug, Clone, PartialEq, clap::Args)]
#[command(next_help_heading = "Priority Blockspace for Humans")]
pub struct PbhArgs {
    /// Sets the max blockspace reserved for verified transactions. If there are not enough
    /// verified transactions to fill the capacity, the remaining blockspace will be filled with
    /// unverified transactions.
    /// This arg is a percentage of the total blockspace with the default set to 70 (ie 70%).
    #[arg(long = "pbh.verified_blockspace_capacity", default_value = "70", value_parser = value_parser!(u8).range(0..=100))]
    pub verified_blockspace_capacity: u8,

    /// Sets the ERC-4337 EntryPoint Proxy contract address
    /// This contract is used to validate 4337 PBH bundles
    #[arg(
        long = "pbh.entrypoint",
        default_value_t = Default::default(),
    )]
    pub entrypoint: Address,

    /// Sets the WorldID contract address.
    /// This contract is used to provide the latest merkle root on chain.
    #[arg(
        long = "pbh.world_id",
        default_value_t = Default::default(),
    )]
    pub world_id: Address,

    /// Sets the ERC0-7766 Signature Aggregator contract address
    /// This contract signifies that a given bundle should receive priority inclusion if it passes validation
    #[arg(
        long = "pbh.signature_aggregator",
        default_value_t = Default::default(),
    )]
    pub signature_aggregator: Address,
}

/// Parameters for pbh builder configuration
#[derive(Debug, Clone, PartialEq, clap::Args)]
#[command(next_help_heading = "Block Builder")]
pub struct BuilderArgs {
    #[arg(
        long = "builder.enabled",
        id = "builder.enabled",
        requires = "private_key",
        required = false
    )]
    pub enabled: bool,

    /// Private key for the builder
    /// used to update PBH nullifiers.
    #[arg(
        long = "builder.private_key",
        env = "BUILDER_PRIVATE_KEY",
        requires = "builder.enabled",
        default_value = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
    )]
    pub private_key: PrivateKeySigner,
}

/// Flashblocks configuration
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Flashblocks",
    group = ArgGroup::new("authorizer")
        .multiple(false)
)]
#[group(requires = "flashblocks.enabled")]
pub struct FlashblocksArgs {
    #[arg(
        long = "flashblocks.enabled",
        id = "flashblocks.enabled",
        requires = "authorizer",
        required = false
    )]
    pub enabled: bool,

    /// Authorizer verifying key
    /// used to verify flashblock authenticity.
    #[arg(
        long = "flashblocks.authorizer_vk",
        env = "FLASHBLOCKS_AUTHORIZER_VK", 
        group = "authorizer",
        value_parser = parse_vk,
        required = false,
    )]
    pub authorizer_vk: Option<VerifyingKey>,

    /// Flashblocks signing key
    /// used to sign authorized flashblocks payloads.
    #[arg(
        long = "flashblocks.builder_sk", 
        env = "FLASHBLOCKS_BUILDER_SK", 
        requires = "builder.enabled",
        required = false,
        value_parser = parse_sk,
    )]
    pub builder_sk: Option<SigningKey>,

    /// Uses the builder_sk to create spoofed
    /// flashblocks authorizations.
    ///
    /// Should only be used for testing
    #[arg(
        long = "flashblocks.spoof_authorizer",
        env = "FLASHBLOCKS_SPOOF_AUTHORIZER",
        group = "authorizer",
        requires = "builder_sk",
        required = false
    )]
    pub spoof_authorizer: bool,
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

    #[derive(Debug, Parser)]
    struct CommandParser {
        #[command(flatten)]
        world: WorldChainArgs,
    }

    #[test]
    fn flashblocks_spoof_authorizer() {
        let flashblocks = FlashblocksArgs {
            enabled: true,
            spoof_authorizer: true,
            authorizer_vk: None,
            builder_sk: Some(SigningKey::from_bytes(&[0; 32])),
        };

        let args = CommandParser::parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.spoof_authorizer",
            "--flashblocks.builder_sk",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "--builder.enabled",
            "--builder.private_key",
            "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        ])
        .world;

        assert_eq!(args.flashblocks.unwrap(), flashblocks);
    }

    #[test]
    fn flashblocks_authorizer() {
        let flashblocks = FlashblocksArgs {
            enabled: true,
            spoof_authorizer: false,
            authorizer_vk: Some(VerifyingKey::from_bytes(&[0; 32]).unwrap()),
            builder_sk: None,
        };

        let args = CommandParser::parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.authorizer_vk",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ])
        .world;

        assert_eq!(args.flashblocks.unwrap(), flashblocks);
    }

    #[test]
    fn flashblocks_neither() {
        CommandParser::try_parse_from(["bin", "--flashblocks.enabled"]).unwrap_err();
    }

    #[test]
    fn flashblocks_both() {
        CommandParser::try_parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.spoof_authorizer",
            "--flashblocks.authorizer_vk",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ])
        .unwrap_err();
    }

    #[test]
    fn flashblocks_sk_without_builder() {
        CommandParser::try_parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.spoof_authorizer",
            "--flashblocks.builder_sk",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ])
        .unwrap_err();
    }

    #[test]
    fn builder() {
        let args = CommandParser::parse_from([
            "bin",
            "--builder.enabled",
            "--builder.private_key",
            "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        ])
        .world;

        assert!(args.flashblocks.is_none());
    }

    #[test]
    fn builder_missing_pk() {
        CommandParser::try_parse_from(["bin", "--builder.enabled"]).unwrap_err();
    }

    #[test]
    fn missing_builder_enabled() {
        CommandParser::try_parse_from([
            "bin",
            "--builder.private_key",
            "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        ])
        .unwrap_err();
    }

    #[test]
    fn none() {
        let args = CommandParser::parse_from(["bin"]).world;
        assert!(args.flashblocks.is_none());
    }
}
