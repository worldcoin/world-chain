use crate::config::FlashblocksPayloadBuilderConfig;
use ::eyre::eyre::bail;
use alloy_primitives::Address;
use reth::chainspec::NamedChain;
use reth_network_peers::PeerId;
use reth_node_builder::NodeConfig;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::args::RollupArgs;
use std::str::FromStr;
use tracing::{debug, info, warn};

pub mod builder;
pub mod p2p;
pub mod pbh;

pub use builder::*;
pub use p2p::*;
pub use pbh::*;

pub const DEFAULT_FLASHBLOCKS_BOOTNODES: &str = "enode://78ca7daeb63956cbc3985853d5699a6404d976a2612575563f46876968fdca2383a195ee7db40de348757b2256195996933708f351169ca3f3fe93ab2a774608@16.62.98.53:30303,enode://c96dcadf4cdea4c39ec3fd775637d9e67d455b856b1514cfcf55b72f873a34b96d69e47ccea9fc797a446d4e6948aa80f6b9d479a1727ca166758a900b08f422@16.63.14.166:30303,enode://15688a7b281c32a4da633252dcc5019d60f037ee9eb46d05093dd3023bdd688b9b207d10a39e054a5ed87db666b2cb75696f6537de74d1e1f8dcabc53dc8d2ab@16.63.123.160:30303";

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

    /// Disable the default World Chain bootnodes.
    #[arg(
        long = "worldchain.disable-bootnodes",
        value_name = "WORLDCHAIN_DISABLE_BOOTNODES",
        default_value_t = false
    )]
    pub disable_bootnodes: bool,
}

impl WorldChainArgs {
    pub fn into_config(
        mut self,
        config: &mut NodeConfig<OpChainSpec>,
    ) -> eyre::Result<WorldChainNodeConfig> {
        // Perform arg validation here for things clap can't do.
        let spec = &config.chain;

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
                if let Some(flashblocks) = &mut self.flashblocks
                    && flashblocks.authorizer_vk.is_none()
                    && flashblocks.override_authorizer_sk.is_none()
                {
                    flashblocks.authorizer_vk = Some(parse_vk(
                        "1361edebf7fd03a72aa23748e17eb5f6901b544cf80d3f410afa5e6e261d7281",
                    )?);
                }

                if self.flashblocks.is_some() && !self.disable_bootnodes {
                    let bootnodes = parse_trusted_peer(DEFAULT_FLASHBLOCKS_BOOTNODES)?;
                    debug!(target: "world_chain::network", ?bootnodes, "Setting default flashblocks bootnodes");
                    // dedup happens later
                    config.network.trusted_peers.extend(bootnodes);
                }

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
                if let Some(flashblocks) = &mut self.flashblocks
                    && flashblocks.authorizer_vk.is_none()
                    && flashblocks.override_authorizer_sk.is_none()
                {
                    flashblocks.authorizer_vk = Some(parse_vk(
                        "3b24dba9803930d6b31c85d9809e03f565b05eba0dd59cfd248e4cc95ebd3492",
                    )?);
                }

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
                if let Some(flashblocks) = &mut self.flashblocks
                    && flashblocks.authorizer_vk.is_none()
                    && flashblocks.override_authorizer_sk.is_none()
                {
                    bail!(
                        "--flashblocks.authorizer_vk or --flashblocks.override_authorizer_sk must be set for non world/sepolia chains"
                    );
                }
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

        let bal_enabled = self.flashblocks.as_ref().is_some_and(|fb| fb.access_list);

        info!(
            target: "reth::cli",
            "Flashblocks BAL validation is {}",
            if bal_enabled { "enabled" } else { "disabled" }
        );

        Ok(WorldChainNodeConfig {
            args: self,
            builder_config: FlashblocksPayloadBuilderConfig {
                inner: Default::default(),
                bal_enabled,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_genesis::Genesis;
    use clap::Parser;
    use reth_node_builder::NodeConfig;
    use std::sync::Arc;

    #[derive(Debug, Parser)]
    struct CommandParser {
        #[command(flatten)]
        world: WorldChainArgs,
    }

    #[test]
    fn flashblocks_both() {
        CommandParser::try_parse_from([
            "bin",
            "--flashblocks.enabled",
            "--flashblocks.override_authorizer",
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
    fn flashblocks_enabled_should_materialize_flashblocks_args() {
        let args = CommandParser::parse_from(["bin", "--flashblocks.enabled"]).world;
        assert!(
            args.flashblocks.is_some(),
            "expected --flashblocks.enabled to populate WorldChainArgs.flashblocks"
        );
        assert!(
            args.flashblocks.expect("just asserted").enabled,
            "expected parsed flashblocks args to have enabled=true"
        );
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
                block_uncompressed_size_limit: None,
            },
            flashblocks: None,
            tx_peers: Some(vec![peer_id.parse().unwrap()]),
            disable_bootnodes: true,
        };

        let spec = reth_optimism_chainspec::OpChainSpec::from_genesis(Genesis::default());
        let mut node_config = NodeConfig::new(Arc::new(spec));
        let config = args.into_config(&mut node_config).unwrap();

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
