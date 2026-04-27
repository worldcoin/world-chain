use reth_network_peers::PeerId;

pub const DEFAULT_MAX_SEND_PEERS: usize = 10;
pub const DEFAULT_MAX_RECEIVE_PEERS: usize = 3;
pub const DEFAULT_ROTATION_INTERVAL: u64 = 30;
pub const DEFAULT_SCORE_SAMPLES: i64 = 1000;

/// Flashblocks configuration
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct FanoutArgs {
    /// Override the flashblocks send-set size.
    #[arg(
        long = "flashblocks.max-send-peers",
        alias = "flashblocks.max_send_peers",
        env = "FLASHBLOCKS_MAX_SEND_PEERS",
        required = false,
        default_value_t = DEFAULT_MAX_SEND_PEERS
    )]
    pub max_send_peers: usize,

    /// Override the number of receive peers maintained for flashblocks fanout.
    #[arg(
        long = "flashblocks.max-receive-peers",
        alias = "flashblocks.max_receive_peers",
        env = "FLASHBLOCKS_MAX_RECEIVE_PEERS",
        required = false,
        default_value_t = DEFAULT_MAX_RECEIVE_PEERS
    )]
    pub max_receive_peers: usize,

    /// Override the flashblocks rotation interval in seconds.
    #[arg(
        long = "flashblocks.rotation-interval",
        alias = "flashblocks.rotation_interval",
        env = "FLASHBLOCKS_ROTATION_INTERVAL",
        required = false,
        default_value_t = DEFAULT_ROTATION_INTERVAL
    )]
    pub rotation_interval: u64,

    /// Override the number of latency samples retained for receive-peer scoring.
    #[arg(
        long = "flashblocks.score-samples",
        alias = "flashblocks.score_samples",
        env = "FLASHBLOCKS_SCORE_SAMPLES",
        required = false,
        default_value_t = DEFAULT_SCORE_SAMPLES
    )]
    pub score_samples: i64,

    /// Peers to always receive flashblocks from regardless of their score.
    ///
    /// These peers will be requested as soon as they connect and will never
    /// be evicted by rotation. They count toward `max_receive_peers`.
    #[arg(
        long = "flashblocks.force-receive-peers",
        alias = "flashblocks.force_receive_peers",
        env = "FLASHBLOCKS_FORCE_RECEIVE_PEERS",
        value_delimiter = ',',
        value_name = "PEER_ID",
        required = false
    )]
    pub force_receive_peers: Vec<PeerId>,
}

impl Default for FanoutArgs {
    fn default() -> Self {
        Self {
            max_send_peers: DEFAULT_MAX_SEND_PEERS,
            max_receive_peers: DEFAULT_MAX_RECEIVE_PEERS,
            rotation_interval: DEFAULT_ROTATION_INTERVAL,
            score_samples: DEFAULT_SCORE_SAMPLES,
            force_receive_peers: Vec::new(),
        }
    }
}
