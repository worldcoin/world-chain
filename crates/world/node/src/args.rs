use ::eyre::eyre::bail;
use alloy_primitives::{Address, B256};
use alloy_signer_local::PrivateKeySigner;
use clap::value_parser;
use ed25519_dalek::{SigningKey, VerifyingKey};
use flashblocks_cli::{FlashblocksArgs, FlashblocksPayloadBuilderConfig};
use hex::FromHex;
use kona_disc::LocalNode;
use kona_genesis::RollupConfig;
use kona_gossip::GaterConfig;
use kona_node_service::NetworkConfig;
use kona_peers::{BootNode, BootStoreFile, PeerMonitoring, PeerScoreLevel};
use kona_sources::{BlockSigner, ClientCert, RemoteSigner};
use libp2p::identity::Keypair;
use reth::chainspec::NamedChain;
use reth_network_peers::{PeerId, TrustedPeer};
use reth_node_builder::NodeConfig;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::args::RollupArgs;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::num::ParseIntError;
use std::path::PathBuf;
use std::str::FromStr;
use tokio::time::Duration;
use tracing::{debug, info, warn};
use url::Url;

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

    /// Kona consensus node args
    #[command(flatten)]
    pub kona: Option<KonaArgs>,

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

    /// Maximum cumulative uncompressed (EIP-2718 encoded) block size in bytes
    #[arg(
        long = "builder.block-uncompressed-size-limit",
        env = "BUILDER_BLOCK_UNCOMPRESSED_SIZE_LIMIT"
    )]
    pub block_uncompressed_size_limit: Option<u64>,
}

pub fn parse_sk(s: &str) -> eyre::Result<SigningKey> {
    let bytes = <[u8; 32]>::from_hex(s.trim())?;
    Ok(SigningKey::from_bytes(&bytes))
}

pub fn parse_vk(s: &str) -> eyre::Result<VerifyingKey> {
    let bytes = <[u8; 32]>::from_hex(s.trim())?;
    Ok(VerifyingKey::from_bytes(&bytes)?)
}

/// Resolves a hostname or IP address string to an [`IpAddr`].
fn resolve_host(host: &str) -> Result<IpAddr, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip);
    }
    let socket_addr = format!("{host}:0");
    match socket_addr.to_socket_addrs() {
        Ok(mut addrs) => addrs
            .next()
            .map(|addr| addr.ip())
            .ok_or_else(|| format!("DNS resolution for '{host}' returned no addresses")),
        Err(e) => Err(format!("Failed to resolve '{host}': {e}")),
    }
}

/// P2P network configuration for the in-process Kona consensus node.
///
/// These flags mirror kona's `P2PArgs` (see `bin/node/src/flags/p2p.rs`) and allow operators to
/// configure persistent P2P identity, bootnodes, peer scoring, gossip mesh parameters, discovery
/// settings, and sequencer signing — all via the world-chain binary's CLI.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(next_help_heading = "Kona P2P")]
pub struct KonaP2PArgs {
    /// Disable Discv5 (node discovery).
    #[arg(long = "p2p.no-discovery", default_value = "false", env = "KONA_NODE_P2P_NO_DISCOVERY")]
    pub no_discovery: bool,

    /// Read the hex-encoded 32-byte private key for the peer ID from this txt file.
    /// Created if not already exists. Important to persist to keep the same network identity
    /// after restarting.
    #[arg(long = "p2p.priv.path", env = "KONA_NODE_P2P_PRIV_PATH")]
    pub priv_path: Option<PathBuf>,

    /// The hex-encoded 32-byte private key for the peer ID.
    #[arg(long = "p2p.priv.raw", env = "KONA_NODE_P2P_PRIV_RAW")]
    pub private_key: Option<B256>,

    /// IP address or DNS hostname to advertise to external peers from Discv5.
    /// Uses `p2p.listen.ip` if not set. Setting this disables dynamic ENR updates.
    #[arg(long = "p2p.advertise.ip", env = "KONA_NODE_P2P_ADVERTISE_IP", value_parser = resolve_host)]
    pub advertise_ip: Option<IpAddr>,

    /// TCP port to advertise. Same as `p2p.listen.tcp` if not set.
    #[arg(long = "p2p.advertise.tcp", env = "KONA_NODE_P2P_ADVERTISE_TCP_PORT")]
    pub advertise_tcp_port: Option<u16>,

    /// UDP port to advertise. Same as `p2p.listen.udp` if not set.
    #[arg(long = "p2p.advertise.udp", env = "KONA_NODE_P2P_ADVERTISE_UDP_PORT")]
    pub advertise_udp_port: Option<u16>,

    /// IP address or DNS hostname to bind LibP2P/Discv5 to.
    #[arg(long = "p2p.listen.ip", default_value = "0.0.0.0", env = "KONA_NODE_P2P_LISTEN_IP", value_parser = resolve_host)]
    pub listen_ip: IpAddr,

    /// TCP port to bind LibP2P to. Any available system port if set to 0.
    #[arg(long = "p2p.listen.tcp", default_value = "9222", env = "KONA_NODE_P2P_LISTEN_TCP_PORT")]
    pub listen_tcp_port: u16,

    /// UDP port to bind Discv5 to. Same as TCP port if left 0.
    #[arg(long = "p2p.listen.udp", default_value = "9223", env = "KONA_NODE_P2P_LISTEN_UDP_PORT")]
    pub listen_udp_port: u16,

    /// Low-tide peer count. The node actively searches for new peer connections if below this.
    #[arg(long = "p2p.peers.lo", default_value = "20", env = "KONA_NODE_P2P_PEERS_LO")]
    pub peers_lo: u32,

    /// High-tide peer count. The node starts pruning peer connections after reaching this.
    #[arg(long = "p2p.peers.hi", default_value = "30", env = "KONA_NODE_P2P_PEERS_HI")]
    pub peers_hi: u32,

    /// Grace period (seconds) to keep a newly connected peer around.
    #[arg(
        long = "p2p.peers.grace",
        default_value = "30",
        env = "KONA_NODE_P2P_PEERS_GRACE",
        value_parser = |arg: &str| -> Result<Duration, ParseIntError> {Ok(Duration::from_secs(arg.parse()?))}
    )]
    pub peers_grace: Duration,

    /// GossipSub topic stable mesh target count (desired outbound degree).
    #[arg(long = "p2p.gossip.mesh.d", default_value = "8", env = "KONA_NODE_P2P_GOSSIP_MESH_D")]
    pub gossip_mesh_d: usize,

    /// GossipSub topic stable mesh low watermark.
    #[arg(long = "p2p.gossip.mesh.lo", default_value = "6", env = "KONA_NODE_P2P_GOSSIP_MESH_DLO")]
    pub gossip_mesh_dlo: usize,

    /// GossipSub topic stable mesh high watermark.
    #[arg(long = "p2p.gossip.mesh.dhi", default_value = "12", env = "KONA_NODE_P2P_GOSSIP_MESH_DHI")]
    pub gossip_mesh_dhi: usize,

    /// GossipSub gossip target (announcements of IHAVE).
    #[arg(long = "p2p.gossip.mesh.dlazy", default_value = "6", env = "KONA_NODE_P2P_GOSSIP_MESH_DLAZY")]
    pub gossip_mesh_dlazy: usize,

    /// Publish messages to all known peers on the topic, outside of the mesh.
    #[arg(long = "p2p.gossip.mesh.floodpublish", default_value = "false", env = "KONA_NODE_P2P_GOSSIP_FLOOD_PUBLISH")]
    pub gossip_flood_publish: bool,

    /// Peer scoring strategy: none or light.
    #[arg(long = "p2p.scoring", default_value = "light", env = "KONA_NODE_P2P_SCORING")]
    pub scoring: PeerScoreLevel,

    /// Ban peers based on their score.
    #[arg(long = "p2p.ban.peers", default_value = "false", env = "KONA_NODE_P2P_BAN_PEERS")]
    pub ban_enabled: bool,

    /// Score threshold below which peers are banned.
    #[arg(long = "p2p.ban.threshold", default_value = "-100", env = "KONA_NODE_P2P_BAN_THRESHOLD")]
    pub ban_threshold: i64,

    /// Duration in minutes to ban a peer for.
    #[arg(long = "p2p.ban.duration", default_value = "60", env = "KONA_NODE_P2P_BAN_DURATION")]
    pub ban_duration: u64,

    /// Interval in seconds to find peers using the discovery service.
    #[arg(long = "p2p.discovery.interval", default_value = "5", env = "KONA_NODE_P2P_DISCOVERY_INTERVAL")]
    pub discovery_interval: u64,

    /// Seconds to wait before removing a random peer from discovery to rotate the peer set.
    #[arg(long = "p2p.discovery.randomize", env = "KONA_NODE_P2P_DISCOVERY_RANDOMIZE")]
    pub discovery_randomize: Option<u64>,

    /// Directory to store the bootstore.
    #[arg(long = "p2p.bootstore", env = "KONA_NODE_P2P_BOOTSTORE")]
    pub bootstore: Option<PathBuf>,

    /// Disable the bootstore.
    #[arg(long = "p2p.no-bootstore", env = "KONA_NODE_P2P_NO_BOOTSTORE")]
    pub disable_bootstore: bool,

    /// Max redial attempts for a disconnected peer. 0 = unlimited.
    #[arg(long = "p2p.redial", env = "KONA_NODE_P2P_REDIAL", default_value = "500")]
    pub peer_redial: Option<u64>,

    /// Duration in minutes of the peer dial period.
    #[arg(long = "p2p.redial.period", env = "KONA_NODE_P2P_REDIAL_PERIOD", default_value = "60")]
    pub redial_period: u64,

    /// Comma-separated list of bootnode ENRs or enode URLs.
    #[arg(long = "p2p.bootnodes", value_delimiter = ',', env = "KONA_NODE_P2P_BOOTNODES")]
    pub bootnodes: Vec<String>,

    /// Enable topic scoring (being phased out, for backwards-compat/debugging only).
    #[arg(long = "p2p.topic-scoring", default_value = "false", env = "KONA_NODE_P2P_TOPIC_SCORING")]
    pub topic_scoring: bool,

    /// Override the unsafe block signer address.
    /// By default fetched from rollup config's system config on L1.
    #[arg(long = "p2p.unsafe.block.signer", env = "KONA_NODE_P2P_UNSAFE_BLOCK_SIGNER")]
    pub unsafe_block_signer: Option<Address>,

    /// Signer configuration for gossip payloads.
    #[command(flatten)]
    pub signer: KonaSignerArgs,
}

impl Default for KonaP2PArgs {
    fn default() -> Self {
        Self {
            no_discovery: false,
            priv_path: None,
            private_key: None,
            advertise_ip: None,
            advertise_tcp_port: None,
            advertise_udp_port: None,
            listen_ip: IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            listen_tcp_port: 9222,
            listen_udp_port: 9223,
            peers_lo: 20,
            peers_hi: 30,
            peers_grace: Duration::from_secs(30),
            gossip_mesh_d: 8,
            gossip_mesh_dlo: 6,
            gossip_mesh_dhi: 12,
            gossip_mesh_dlazy: 6,
            gossip_flood_publish: false,
            scoring: PeerScoreLevel::default(),
            ban_enabled: false,
            ban_threshold: -100,
            ban_duration: 60,
            discovery_interval: 5,
            discovery_randomize: None,
            bootstore: None,
            disable_bootstore: false,
            peer_redial: Some(500),
            redial_period: 60,
            bootnodes: Vec::new(),
            topic_scoring: false,
            unsafe_block_signer: None,
            signer: KonaSignerArgs::default(),
        }
    }
}

impl KonaP2PArgs {
    /// Load or generate the libp2p keypair from CLI inputs.
    ///
    /// If a raw private key is provided, it is used directly. If a file path is provided, the key
    /// is loaded from the file (or generated and written to it if it doesn't exist). If neither is
    /// provided, returns an error.
    fn keypair(&self) -> eyre::Result<Keypair> {
        if let Some(mut private_key) = self.private_key {
            let keypair = kona_cli::SecretKeyLoader::parse(&mut private_key.0)
                .map_err(|e| eyre::Report::msg(format!("{e}")))?;
            info!(
                target: "world_chain::p2p",
                peer_id = %keypair.public().to_peer_id(),
                "Loaded P2P keypair from raw private key"
            );
            return Ok(keypair);
        }

        let Some(ref key_path) = self.priv_path else {
            eyre::bail!("Neither a raw private key nor a private key file path was provided.");
        };

        kona_cli::SecretKeyLoader::load(key_path).map_err(|e| eyre::Report::msg(format!("{e}")))
    }

    /// Construct a [`NetworkConfig`] from these CLI arguments.
    ///
    /// Adapted from kona's `P2PArgs::config()`. The `rollup_config` is the parsed OP Stack rollup
    /// config. `l2_chain_id` is used for bootstore default path.
    pub fn build_network_config(
        self,
        rollup_config: &RollupConfig,
        l2_chain_id: u64,
    ) -> eyre::Result<NetworkConfig> {
        let advertise_ip = self.advertise_ip.unwrap_or(self.listen_ip);
        let static_ip = self.advertise_ip.is_some();
        let advertise_tcp_port = self.advertise_tcp_port.unwrap_or(self.listen_tcp_port);
        let advertise_udp_port = self.advertise_udp_port.unwrap_or(self.listen_udp_port);

        let keypair = self.keypair().unwrap_or_else(|e| {
            let generated = Keypair::generate_secp256k1();
            warn!(
                target: "world_chain::p2p",
                error = %e,
                peer_id = %generated.public().to_peer_id(),
                "Failed to load P2P keypair, generated ephemeral keypair. \
                 Set --p2p.priv.path or --p2p.priv.raw for a persistent peer ID."
            );
            generated
        });

        let secp256k1_key = keypair
            .clone()
            .try_into_secp256k1()
            .map_err(|e| eyre::Report::msg(format!("Failed to convert keypair to secp256k1: {e}")))?
            .secret()
            .to_bytes();
        let local_node_key = discv5::enr::k256::ecdsa::SigningKey::from_bytes(&secp256k1_key.into())
            .map_err(|e| eyre::Report::msg(format!("Failed to convert to k256 signing key: {e}")))?;

        let discovery_address =
            LocalNode::new(local_node_key, advertise_ip, advertise_tcp_port, advertise_udp_port);

        let gossip_config = kona_gossip::default_config_builder()
            .mesh_n(self.gossip_mesh_d)
            .mesh_n_low(self.gossip_mesh_dlo)
            .mesh_n_high(self.gossip_mesh_dhi)
            .gossip_lazy(self.gossip_mesh_dlazy)
            .flood_publish(self.gossip_flood_publish)
            .build()
            .map_err(|e| eyre::Report::msg(format!("Failed to build gossip config: {e}")))?;

        let monitor_peers = self.ban_enabled.then_some(PeerMonitoring {
            ban_duration: Duration::from_secs(60 * self.ban_duration),
            ban_threshold: self.ban_threshold as f64,
        });

        let discovery_listening_address =
            SocketAddr::new(self.listen_ip, self.listen_udp_port);
        let discovery_config =
            NetworkConfig::discv5_config(discovery_listening_address.into(), static_ip);

        let mut gossip_address = libp2p::Multiaddr::from(self.listen_ip);
        gossip_address.push(libp2p::multiaddr::Protocol::Tcp(self.listen_tcp_port));

        let unsafe_block_signer = self.unsafe_block_signer.unwrap_or(Address::ZERO);

        let bootstore = if self.disable_bootstore {
            None
        } else {
            Some(self.bootstore.map_or_else(
                || BootStoreFile::Default { chain_id: l2_chain_id },
                BootStoreFile::Custom,
            ))
        };

        let bootnodes = self
            .bootnodes
            .iter()
            .map(|bootnode| BootNode::parse_bootnode(bootnode))
            .collect::<Vec<BootNode>>()
            .into();

        let gossip_signer = self.signer.into_block_signer()?;

        Ok(NetworkConfig {
            discovery_config,
            discovery_interval: Duration::from_secs(self.discovery_interval),
            discovery_address,
            discovery_randomize: self.discovery_randomize.map(Duration::from_secs),
            enr_update: !static_ip,
            gossip_address,
            keypair,
            unsafe_block_signer,
            gossip_config,
            scoring: self.scoring,
            monitor_peers,
            bootstore,
            topic_scoring: self.topic_scoring,
            gater_config: GaterConfig {
                peer_redialing: self.peer_redial,
                dial_period: Duration::from_secs(60 * self.redial_period),
            },
            bootnodes,
            rollup_config: rollup_config.clone(),
            gossip_signer,
        })
    }
}

/// Signer configuration for Kona's gossip payloads.
///
/// Mirrors kona's `SignerArgs`. Supports local key signing or remote signer.
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct KonaSignerArgs {
    /// Local private key for the sequencer to sign unsafe blocks.
    #[arg(
        long = "p2p.sequencer.key",
        env = "KONA_NODE_P2P_SEQUENCER_KEY",
        conflicts_with = "p2p_signer_endpoint"
    )]
    pub sequencer_key: Option<B256>,

    /// Path to a file containing the sequencer private key.
    #[arg(
        long = "p2p.sequencer.key.path",
        env = "KONA_NODE_P2P_SEQUENCER_KEY_PATH",
        conflicts_with = "sequencer_key"
    )]
    pub sequencer_key_path: Option<PathBuf>,

    /// URL of the remote signer endpoint.
    #[arg(
        long = "p2p.signer.endpoint",
        id = "p2p_signer_endpoint",
        env = "KONA_NODE_P2P_SIGNER_ENDPOINT",
        requires = "p2p_signer_address"
    )]
    pub endpoint: Option<Url>,

    /// Address to sign transactions for (required with remote signer).
    #[arg(
        long = "p2p.signer.address",
        id = "p2p_signer_address",
        env = "KONA_NODE_P2P_SIGNER_ADDRESS",
        requires = "p2p_signer_endpoint"
    )]
    pub address: Option<Address>,

    /// Headers for the remote signer. Format: `key=value`.
    #[arg(long = "p2p.signer.header", env = "KONA_NODE_P2P_SIGNER_HEADER", requires = "p2p_signer_endpoint")]
    pub header: Vec<String>,

    /// Path to CA certificates for the remote signer.
    #[arg(long = "p2p.signer.tls.ca", env = "KONA_NODE_P2P_SIGNER_TLS_CA", requires = "p2p_signer_endpoint")]
    pub ca_cert: Option<PathBuf>,

    /// Path to the client certificate for the remote signer.
    #[arg(
        long = "p2p.signer.tls.cert",
        env = "KONA_NODE_P2P_SIGNER_TLS_CERT",
        requires = "p2p_signer_tls_key",
        requires = "p2p_signer_endpoint"
    )]
    pub cert: Option<PathBuf>,

    /// Path to the client key for the remote signer.
    #[arg(
        long = "p2p.signer.tls.key",
        id = "p2p_signer_tls_key",
        env = "KONA_NODE_P2P_SIGNER_TLS_KEY",
        requires = "cert",
        requires = "p2p_signer_endpoint"
    )]
    pub key: Option<PathBuf>,
}

impl KonaSignerArgs {
    /// Convert into an optional [`BlockSigner`].
    fn into_block_signer(self) -> eyre::Result<Option<BlockSigner>> {
        let sequencer_key = match (self.sequencer_key, &self.sequencer_key_path) {
            (Some(key), None) => Some(key),
            (None, Some(path)) => {
                let keypair = kona_cli::SecretKeyLoader::load(path)
                    .map_err(|e| eyre::Report::msg(format!("Failed to load sequencer key: {e}")))?;
                let secp = keypair
                    .try_into_secp256k1()
                    .map_err(|_| eyre::Report::msg("Sequencer key is not secp256k1"))?;
                Some(B256::from_slice(&secp.secret().to_bytes()))
            }
            (Some(_), Some(_)) => {
                eyre::bail!(
                    "Both --p2p.sequencer.key and --p2p.sequencer.key.path cannot be specified"
                );
            }
            (None, None) => None,
        };

        let remote = self.into_remote_signer()?;

        match (sequencer_key, remote) {
            (Some(_), Some(_)) => {
                eyre::bail!("Cannot specify both local sequencer key and remote signer")
            }
            (Some(key), None) => {
                let signer: BlockSigner = PrivateKeySigner::from_bytes(&key)?.into();
                Ok(Some(signer))
            }
            (None, Some(remote)) => Ok(Some(remote.into())),
            (None, None) => Ok(None),
        }
    }

    fn into_remote_signer(self) -> eyre::Result<Option<RemoteSigner>> {
        let Some(endpoint) = self.endpoint else {
            return Ok(None);
        };
        let Some(address) = self.address else {
            eyre::bail!("--p2p.signer.address is required with --p2p.signer.endpoint");
        };

        let headers = self
            .header
            .iter()
            .map(|h| {
                let (key, value) = h
                    .split_once('=')
                    .ok_or_else(|| eyre::Report::msg("Invalid header format, expected key=value"))?;
                Ok((HeaderName::from_str(key)?, HeaderValue::from_str(value)?))
            })
            .collect::<eyre::Result<HeaderMap>>()?;

        let client_cert = self
            .cert
            .map(|cert| {
                Ok::<_, eyre::Report>(ClientCert {
                    cert,
                    key: self
                        .key
                        .ok_or_else(|| eyre::Report::msg("--p2p.signer.tls.key required with --p2p.signer.tls.cert"))?,
                })
            })
            .transpose()?;

        Ok(Some(RemoteSigner {
            address,
            endpoint,
            ca_cert: self.ca_cert,
            client_cert,
            headers,
        }))
    }
}

/// Arguments for the in-process Kona consensus node.
///
/// When `--kona.enabled` is set, the Kona OP Stack consensus node runs in-process alongside reth,
/// eliminating the need for a separate op-node binary. Engine API calls are dispatched directly
/// via Rust function calls instead of HTTP/IPC.
#[derive(Debug, Clone, PartialEq, clap::Args)]
#[command(next_help_heading = "Kona Consensus Node")]
pub struct KonaArgs {
    /// Enable the in-process Kona consensus node.
    ///
    /// When enabled, the world-chain binary acts as both the execution and consensus client.
    #[arg(long = "kona.enabled", id = "kona.enabled", default_value_t = false)]
    pub enabled: bool,

    /// L1 execution RPC URL for fetching deposits, batches, and finalization signals.
    #[arg(
        long = "kona.l1-rpc-url",
        env = "KONA_L1_RPC_URL",
        requires = "kona.enabled",
        default_value = "http://localhost:8545"
    )]
    pub l1_rpc_url: String,

    /// L1 beacon API URL for fetching blob data (required post-Dencun).
    #[arg(
        long = "kona.l1-beacon-url",
        env = "KONA_L1_BEACON_URL",
        requires = "kona.enabled",
        default_value = "http://localhost:5052"
    )]
    pub l1_beacon_url: String,

    /// Trust the L1 RPC without additional receipt verification.
    #[arg(
        long = "kona.l1-trust-rpc",
        requires = "kona.enabled",
        default_value_t = false
    )]
    pub l1_trust_rpc: bool,

    /// P2P network configuration.
    #[command(flatten)]
    pub p2p: KonaP2PArgs,

    /// Path to the OP Stack rollup configuration JSON file.
    ///
    /// This file defines the rollup parameters (chain ID, block time, hardfork activation
    /// timestamps, genesis hashes, etc.) used by the Kona consensus node. It follows the same
    /// format as op-node's `--rollup.config` flag.
    #[arg(
        long = "kona.rollup-config",
        env = "KONA_ROLLUP_CONFIG",
        requires = "kona.enabled",
    )]
    pub rollup_config_path: Option<std::path::PathBuf>,
}

fn parse_trusted_peer(s: &str) -> eyre::Result<Vec<TrustedPeer>> {
    s.split(',')
        .map(|enode| {
            enode.parse().map_err(|err| {
                eyre::Report::msg(format!("invalid flashblocks bootnode '{}': {}", enode, err))
            })
        })
        .collect()
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
            kona: None,
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
