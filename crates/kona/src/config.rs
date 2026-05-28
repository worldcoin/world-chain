//! Configuration for the in-process Kona integration.
//!
//! Bridges World Chain's node configuration to the [`KonaService`](crate::KonaService) inputs.
//! Unlike the previous HTTP transport, there is no engine RPC URL or JWT here: the engine is driven
//! in-process via an [`InProcessEngineClient`](crate::InProcessEngineClient) supplied by the node
//! add-ons.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};

use kona_genesis::RollupConfig;
use kona_node_service::SequencerConfig;
use kona_rpc::RpcBuilder;
use url::Url;
use world_chain_cli::KonaP2PArgs;

/// Static configuration for the in-process Kona node.
///
/// The dynamic, post-launch pieces (the reth engine handle, payload store, and L2 provider) are
/// injected by the add-ons when assembling the [`KonaService`](crate::KonaService).
#[derive(Debug, Clone)]
pub struct KonaConfig {
    /// The OP Stack rollup configuration, shared with reth.
    pub rollup_config: Arc<RollupConfig>,

    /// L1 RPC endpoint URL for fetching L1 block data.
    pub l1_rpc_url: Url,

    /// L1 beacon API endpoint URL for fetching blob data.
    pub l1_beacon_url: Url,

    /// Whether to trust the L1 RPC without additional receipt verification.
    pub l1_trust_rpc: bool,

    /// Whether to trust the L2 RPC used by the derivation pipeline.
    pub l2_trust_rpc: bool,

    /// Whether to run in sequencer mode.
    pub sequencer_mode: bool,

    /// Whether the sequencer should start in the stopped state.
    pub sequencer_stopped: bool,

    /// Whether the sequencer runs in recovery mode.
    pub sequencer_recovery_mode: bool,

    /// Optional op-conductor RPC endpoint. When [`Some`], the conductor service is enabled.
    pub conductor_rpc_url: Option<Url>,

    /// Number of L1 confirmations the sequencer waits on before building from an L1 origin.
    pub l1_confs: u64,

    /// P2P network configuration arguments.
    pub p2p: KonaP2PArgs,

    /// IP address the Kona node RPC server binds to.
    pub rpc_addr: IpAddr,

    /// Port the Kona node RPC server binds to.
    pub rpc_port: u16,

    /// Whether the admin namespace is enabled on the Kona node RPC server.
    pub rpc_enable_admin: bool,

    /// Whether the Kona node RPC server is enabled.
    pub rpc_enabled: bool,

    /// Optional override for L1 slot duration in seconds.
    pub l1_slot_duration_override: Option<u64>,
}

impl KonaConfig {
    /// Creates a basic configuration for a validator node.
    pub fn validator(
        rollup_config: Arc<RollupConfig>,
        l1_rpc_url: Url,
        l1_beacon_url: Url,
    ) -> Self {
        Self {
            rollup_config,
            l1_rpc_url,
            l1_beacon_url,
            l1_trust_rpc: false,
            l2_trust_rpc: false,
            sequencer_mode: false,
            sequencer_stopped: false,
            sequencer_recovery_mode: false,
            conductor_rpc_url: None,
            l1_confs: 4,
            p2p: KonaP2PArgs::default(),
            rpc_addr: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            rpc_port: 8547,
            rpc_enable_admin: false,
            rpc_enabled: true,
            l1_slot_duration_override: None,
        }
    }

    /// Builds the [`SequencerConfig`] driving kona's sequencer actor.
    pub fn make_sequencer_config(&self) -> SequencerConfig {
        SequencerConfig {
            sequencer_stopped: self.sequencer_stopped,
            sequencer_recovery_mode: self.sequencer_recovery_mode,
            conductor_rpc_url: self.conductor_rpc_url.clone(),
            l1_conf_delay: self.l1_confs,
        }
    }

    /// Builds the [`RpcBuilder`] for kona's node RPC server, if RPC is enabled.
    ///
    /// Returns [`None`] when the RPC server is disabled. The server is exposed over HTTP only;
    /// websocket and dev endpoints are disabled, and admin state is not persisted.
    pub fn make_rpc_builder(&self) -> Option<RpcBuilder> {
        self.rpc_enabled.then(|| RpcBuilder {
            no_restart: false,
            socket: SocketAddr::new(self.rpc_addr, self.rpc_port),
            enable_admin: self.rpc_enable_admin,
            admin_persistence: None,
            ws_enabled: false,
            dev_enabled: false,
        })
    }
}
