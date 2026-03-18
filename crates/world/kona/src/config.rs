//! Configuration for the Kona integration.
//!
//! Bridges between World Chain's node configuration and Kona's node builder requirements.

use kona_genesis::RollupConfig;
use std::sync::Arc;
use url::Url;

/// Configuration for the in-process Kona node.
///
/// This bridges the gap between World Chain's existing configuration and what Kona's
/// [`kona_node_service::RollupNodeBuilder`] expects.
#[derive(Debug, Clone)]
pub struct KonaConfig {
    /// The OP Stack rollup configuration.
    ///
    /// Defines the chain's genesis, block time, hardfork schedule, and other consensus parameters.
    /// This is shared between Kona (consensus) and Reth (execution).
    pub rollup_config: Arc<RollupConfig>,

    /// L1 RPC endpoint URL for fetching L1 block data.
    ///
    /// Kona's L1 watcher and derivation pipeline use this to read deposits, batches, and
    /// finalization signals from the L1 chain.
    pub l1_rpc_url: Url,

    /// L1 beacon API endpoint URL for fetching blob data.
    ///
    /// Required post-Dencun for reading blob-carried batch data from the L1 beacon chain.
    pub l1_beacon_url: Url,

    /// Whether to trust the L1 RPC without additional verification.
    ///
    /// When `true`, Kona skips receipt validation for L1 data. Should only be used with
    /// trusted L1 RPC providers.
    pub l1_trust_rpc: bool,

    /// Whether to trust the L2 RPC without additional verification.
    pub l2_trust_rpc: bool,

    /// Whether to run in sequencer mode.
    ///
    /// In sequencer mode, Kona drives block production by building payload attributes from L1
    /// data and sending them to the engine. In validator mode, Kona follows the sequencer via
    /// P2P gossip and derivation.
    pub sequencer_mode: bool,

    /// P2P listening port for the Kona node.
    ///
    /// Kona uses libp2p for gossipsub (unsafe block propagation) and discv5 (peer discovery).
    pub p2p_listen_port: u16,

    /// Optional RPC listen address for the Kona node API.
    ///
    /// Exposes the `optimism_syncStatus`, `optimism_outputAtBlock`, and admin endpoints.
    pub rpc_listen_addr: Option<String>,

    /// Optional override for L1 slot duration in seconds.
    ///
    /// If set, Kona will use this instead of querying the L1 beacon for slot configuration.
    /// Useful for devnets or when beacon API is unreliable.
    pub l1_slot_duration_override: Option<u64>,
}

impl KonaConfig {
    /// Creates a basic configuration for a validator node.
    ///
    /// This sets up the most common configuration for a non-sequencing node that follows
    /// the chain via derivation and P2P gossip.
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
            p2p_listen_port: 9222,
            rpc_listen_addr: None,
            l1_slot_duration_override: None,
        }
    }
}
