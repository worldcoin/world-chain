use alloy_primitives::Address;
use alloy_signer_local::PrivateKeySigner;
use clap::value_parser;
use ed25519_dalek::{SigningKey, VerifyingKey};
use eyre::eyre;
use flashblocks_cli::FlashblocksArgs;
use hex::FromHex;
use reth::chainspec::NamedChain;
use reth_network_peers::PeerId;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::args::RollupArgs;
use std::str::FromStr;
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

    /// Comma-separated list of peer IDs to which transactions should be propagated
    #[arg(long = "tx-peers", value_delimiter = ',', value_name = "PEER_ID")]
    pub tx_peers: Option<Vec<PeerId>>,
}

impl WorldChainArgs {
    pub fn into_config(mut self, spec: &OpChainSpec) -> eyre::Result<WorldChainNodeConfig> {
        // Perform arg validation here for things clap can't do.

        if let Some(peers) = &self.tx_peers {
            if self.rollup.disable_txpool_gossip {
                warn!(
                    target: "world_chain::network",
                    "--tx-peers is ignored when transaction pool gossip is disabled \
                     (--rollup.disable-tx-pool-gossip). The --tx-peers flag is shadowed and has no effect."
                );
                self.tx_peers = None;
            } else {
                tracing::info!(
                    target: "world_chain::network",
                    "Transaction propagation restricted to {} peer(s)",
                    peers.len()
                );
            }
        }

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

pub fn parse_sk(s: &str) -> eyre::Result<SigningKey> {
    let bytes = <[u8; 32]>::from_hex(s.trim())?;
    Ok(SigningKey::from_bytes(&bytes))
}

pub fn parse_vk(s: &str) -> eyre::Result<VerifyingKey> {
    let bytes = <[u8; 32]>::from_hex(s.trim())?;
    Ok(VerifyingKey::from_bytes(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_genesis::Genesis;
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
            flashblocks_interval: 200,
            recommit_interval: 200,
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
            flashblocks_interval: 200,
            recommit_interval: 200,
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

    #[test]
    fn test_tx_peers_basic() {
        let peer1 = "6f8a80d14311c39f35f516fa664deaaaa13e85b2f7493f37f6144d86991ec012937307647bd3b9a82abe2974e1407241d54947bbb39763a4cac9f77166ad92a0";
        let peer2 = "d860a01f9722d78051619d1e2351aba3f43f943f6f00718d1b9baa4101932a1f5011f16bb2b1bb35db20d6db18b2a4b46dcd226f73d917f6652a2b0a96b4f78a";

        let args =
            CommandParser::parse_from(["bin", "--tx-peers", &format!("{},{}", peer1, peer2)]).world;

        assert!(args.tx_peers.is_some());
        assert_eq!(args.tx_peers.as_ref().unwrap().len(), 2);

        let args = CommandParser::parse_from(["bin"]).world;
        assert!(args.tx_peers.is_none());
    }

    #[test]
    fn test_tx_peers_shadowing_by_disable_gossip() {
        let peer_id = "6f8a80d14311c39f35f516fa664deaaaa13e85b2f7493f37f6144d86991ec012937307647bd3b9a82abe2974e1407241d54947bbb39763a4cac9f77166ad92a0";

        let rollup_args = RollupArgs {
            disable_txpool_gossip: true,
            ..Default::default()
        };

        let args = WorldChainArgs {
            rollup: rollup_args,
            pbh: PbhArgs {
                verified_blockspace_capacity: 70,
                entrypoint: Default::default(),
                world_id: Default::default(),
                signature_aggregator: Default::default(),
            },
            builder: BuilderArgs {
                enabled: false,
                private_key: "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
                    .parse()
                    .unwrap(),
            },
            flashblocks: None,
            tx_peers: Some(vec![peer_id.parse().unwrap()]),
        };

        let spec = reth_optimism_chainspec::OpChainSpec::from_genesis(Genesis::default());
        let config = args.into_config(&spec).unwrap();

        // tx_peers should be set to None due to shadowing
        assert!(config.args.tx_peers.is_none());
    }

    #[test]
    fn test_clap_empty_string_behavior() {
        // Clap with value_delimiter and a type that requires parsing (like PeerId)
        // will ERROR on empty string because it can't parse "" as PeerId
        let result = CommandParser::try_parse_from(["bin", "--tx-peers="]);
        assert!(
            result.is_err(),
            "Clap should error on empty string for PeerId"
        );
    }
}
