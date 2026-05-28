//! Configuration for the Kona integration.
//!
//! Bridges between World Chain's node configuration and Kona's node builder requirements.

use alloy_rpc_types_engine::JwtSecret;
use kona_genesis::RollupConfig;
use kona_node_service::{
    EngineConfig, L1ConfigBuilder, NetworkConfig, NodeMode, RollupNode, RollupNodeBuilder,
};
use std::sync::Arc;
use url::Url;
use world_chain_cli::KonaP2PArgs;

/// Configuration for the in-process Kona node.
///
/// This bridges the gap between World Chain's existing configuration and what Kona's
/// [`RollupNodeBuilder`] expects.
#[derive(Debug, Clone)]
pub struct KonaConfig {
    /// The OP Stack rollup configuration.
    pub rollup_config: Arc<RollupConfig>,

    /// L1 RPC endpoint URL for fetching L1 block data.
    pub l1_rpc_url: Url,

    /// L1 beacon API endpoint URL for fetching blob data.
    pub l1_beacon_url: Url,

    /// Whether to trust the L1 RPC without additional receipt verification.
    pub l1_trust_rpc: bool,

    /// Whether to trust the L2 RPC without additional verification.
    pub l2_trust_rpc: bool,

    /// Whether to run in sequencer mode.
    pub sequencer_mode: bool,

    /// P2P network configuration arguments.
    pub p2p: KonaP2PArgs,

    /// Optional RPC listen address for the Kona node API.
    pub rpc_listen_addr: Option<String>,

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
            p2p: KonaP2PArgs::default(),
            rpc_listen_addr: None,
            l1_slot_duration_override: None,
        }
    }

    /// Builds a [`RollupNode`] from this configuration.
    ///
    /// The `engine_config` points kona's internally-constructed engine client at reth's auth RPC
    /// endpoint, which it drives over the standard Engine API (HTTP + JWT).
    pub fn build_rollup_node(
        self,
        engine_config: EngineConfig,
        p2p_config: NetworkConfig,
    ) -> RollupNode {
        let l1_config_builder = L1ConfigBuilder {
            chain_config: kona_genesis::L1ChainConfig::default(),
            trust_rpc: self.l1_trust_rpc,
            beacon: self.l1_beacon_url,
            rpc_url: self.l1_rpc_url,
            slot_duration_override: self.l1_slot_duration_override,
        };

        let builder = RollupNodeBuilder::new(
            (*self.rollup_config).clone(),
            l1_config_builder,
            self.l2_trust_rpc,
            engine_config,
            p2p_config,
            None,
        );

        builder.build()
    }

    /// Builds an [`EngineConfig`] pointed at reth's L2 auth RPC endpoint.
    ///
    /// Canonical kona constructs its own [`kona_engine::OpEngineClient`] from this config and
    /// drives reth's execution engine over the standard authenticated Engine API. The
    /// `l2_auth_rpc_url`/`jwt_secret` must match reth's launched auth server.
    pub fn make_engine_config(&self, l2_auth_rpc_url: Url, jwt_secret: JwtSecret) -> EngineConfig {
        EngineConfig {
            config: self.rollup_config.clone(),
            l2_url: l2_auth_rpc_url,
            l2_jwt_secret: jwt_secret,
            l1_url: self.l1_rpc_url.clone(),
            mode: if self.sequencer_mode {
                NodeMode::Sequencer
            } else {
                NodeMode::Validator
            },
        }
    }
}
