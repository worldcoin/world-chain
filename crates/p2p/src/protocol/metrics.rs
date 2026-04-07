use metrics::{Counter, Gauge, Histogram, Label};
use metrics_derive::Metrics;
use reth_ethereum::network::api::PeerId;

/// Aggregate metrics for the flashblocks P2P protocol.
#[derive(Clone, Metrics)]
#[metrics(scope = "flashblocks.p2p")]
pub struct FlashblocksP2PMetrics {
    /// Current number of connected flashblocks peers.
    #[metric(rename = "peers")]
    pub connected_peers: Gauge,
    /// Total number of stale epochs rejected by the event stream reducer.
    #[metric(rename = "event_stream.epochs_stale")]
    pub stale_event_stream_epochs_total: Counter,
    /// Histogram of encoded flashblock message sizes in bytes.
    #[metric(rename = "size")]
    pub flashblock_size_bytes: Histogram,
    /// Histogram of gas used by broadcast flashblocks.
    #[metric(rename = "gas_used")]
    pub flashblock_gas_used: Histogram,
    /// Histogram of transaction counts per broadcast flashblock.
    #[metric(rename = "tx_count")]
    pub flashblock_tx_count: Histogram,
    /// Histogram of time between sequential flashblocks in seconds.
    #[metric(rename = "interval")]
    pub flashblock_interval_seconds: Histogram,
}

impl FlashblocksP2PMetrics {
    pub fn increment_connected_peers(&self) {
        self.connected_peers.increment(1.0);
    }

    pub fn decrement_connected_peers(&self) {
        self.connected_peers.decrement(1.0);
    }

    pub fn increment_stale_event_stream_epochs(&self) {
        self.stale_event_stream_epochs_total.increment(1);
    }

    pub fn record_flashblock_size_bytes(&self, size_bytes: usize) {
        self.flashblock_size_bytes.record(size_bytes as f64);
    }

    pub fn record_flashblock_gas_used(&self, gas_used: u64) {
        self.flashblock_gas_used.record(gas_used as f64);
    }

    pub fn record_flashblock_tx_count(&self, tx_count: usize) {
        self.flashblock_tx_count.record(tx_count as f64);
    }

    pub fn record_flashblock_interval_seconds(&self, interval_seconds: f64) {
        self.flashblock_interval_seconds.record(interval_seconds);
    }
}

/// Peer-scoped flashblocks P2P metrics.
#[derive(Clone, Metrics)]
#[metrics(scope = "flashblocks.p2p")]
pub struct FlashblocksPeerMetrics {
    /// Total inbound bandwidth attributed to this peer in bytes.
    #[metric(rename = "bandwidth_inbound")]
    pub inbound_bandwidth_bytes_total: Counter,
    /// Total outbound bandwidth attributed to this peer in bytes.
    #[metric(rename = "bandwidth_outbound")]
    pub outbound_bandwidth_bytes_total: Counter,
    /// Histogram of flashblock delivery latency from this peer in seconds.
    #[metric(rename = "latency")]
    pub flashblock_latency_seconds: Histogram,
    /// Latest moving score for this peer.
    #[metric(rename = "peer_score")]
    pub score: Gauge,
    /// Number of flashblocks this peer failed to deliver within the grace window.
    #[metric(rename = "missed_flashblocks")]
    pub missed_flashblocks_total: Counter,
}

impl FlashblocksPeerMetrics {
    pub fn for_peer(peer_id: PeerId) -> Self {
        Self::new_with_labels(vec![Label::new("peer_id", format!("{:#x}", peer_id))])
    }

    pub fn record_inbound_bandwidth_bytes(&self, size_bytes: usize) {
        self.inbound_bandwidth_bytes_total
            .increment(size_bytes as u64);
    }

    pub fn record_outbound_bandwidth_bytes(&self, size_bytes: usize) {
        self.outbound_bandwidth_bytes_total
            .increment(size_bytes as u64);
    }

    pub fn record_flashblock_latency_seconds(&self, latency_seconds: f64) {
        self.flashblock_latency_seconds.record(latency_seconds);
    }

    pub fn set_score(&self, score: i64) {
        self.score.set(score as f64);
    }

    pub fn increment_missed_flashblocks(&self) {
        self.missed_flashblocks_total.increment(1);
    }
}
