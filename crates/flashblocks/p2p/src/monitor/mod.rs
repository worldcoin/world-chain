use reth_ethereum::network::api::PeerId;
use reth_network::{Peers, PeersInfo};
use reth_tasks::TaskExecutor;
use std::collections::HashMap;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Default polling interval for checking trusted peer status (production)
const DEFAULT_PEER_MONITOR_INTERVAL_SECS: u64 = 30;

/// Get the peer monitor polling interval from environment variable or use default.
/// Mainly for testing purposes.
fn get_peer_monitor_interval() -> Duration {
    if let Ok(interval_str) = std::env::var("PEER_MONITOR_INTERVAL_MS") {
        if let Ok(interval) = interval_str.parse::<u64>() {
            return Duration::from_millis(interval);
        } else {
            tracing::trace!(
                target: "flashblocks::p2p",
                interval_str = interval_str,
                "Invalid PEER_MONITOR_INTERVAL_MS environment variable, using default"
            );
        }
    }

    if let Ok(interval_str) = std::env::var("PEER_MONITOR_INTERVAL_SECS") {
        if let Ok(interval) = interval_str.parse::<u64>() {
            return Duration::from_secs(interval);
        } else {
            tracing::trace!(
                target: "flashblocks::p2p",
                interval_str = interval_str,
                "Invalid PEER_MONITOR_INTERVAL_SECS environment variable, using default"
            );
        }
    }

    Duration::from_secs(DEFAULT_PEER_MONITOR_INTERVAL_SECS)
}

/// Tracks the state of a known peer
#[derive(Debug, Clone, Copy)]
struct PeerState {
    /// Number of consecutive ticks this peer has been disconnected (0 if currently connected)
    disconnected_ticks: u64,
}

/// Monitors trusted peers and detects disconnections by maintaining a grow-only set
/// of peers that have been seen and comparing against currently connected peers.
pub struct PeerMonitor<N> {
    network: N,
    /// Map of peer ID to their state (connected or disconnected with tick count)
    known_peers: HashMap<PeerId, PeerState>,
    interval: Duration,
}

impl<N> PeerMonitor<N>
where
    N: Peers + PeersInfo + Send + Sync + 'static,
{
    /// Create a new peer monitor
    pub fn new(network: N) -> Self {
        let interval = get_peer_monitor_interval();
        Self {
            network,
            known_peers: HashMap::new(),
            interval,
        }
    }

    /// Run the peer monitor until shutdown is signaled.
    /// Periodically checks trusted peers and detects disconnections.
    pub async fn run(mut self, shutdown_token: CancellationToken) {
        tracing::info!(
            target: "flashblocks::p2p",
            interval_secs = self.interval.as_secs(),
            "PeerMonitor started"
        );
        let mut interval = tokio::time::interval(self.interval);

        loop {
            // FIXME: Just for debugging purposes. Remove this later.
            tracing::error!(
                target: "flashblocks::p2p",
                num_known_peers = self.known_peers.len(),
                "PeerMonitor tick"
            );
            tokio::select! {
                _ = interval.tick() => {
                    self.check_peers().await;
                }
                _ = shutdown_token.cancelled() => {
                    tracing::debug!("Peer monitor shutting down");
                    break;
                }
            }
        }
    }

    /// Run the peer monitor on the given task executor to be managed by reth.
    pub fn run_on_task_executor(self, task_executor: &TaskExecutor) {
        task_executor.spawn_critical_with_graceful_shutdown_signal(
            "flashblocks peer monitor",
            |reth_shutdown| async move {
                let shutdown_token = CancellationToken::new();
                let shutdown_token_clone = shutdown_token.clone();

                tokio::spawn(async move {
                    let _guard = reth_shutdown.await;
                    shutdown_token_clone.cancel();
                });

                self.run(shutdown_token).await;
            },
        );
    }

    /// Check current trusted peers and detect disconnections
    async fn check_peers(&mut self) {
        match self.network.get_trusted_peers().await {
            Ok(peers) => {
                let current_peer_ids: std::collections::HashSet<PeerId> =
                    peers.iter().map(|p| p.remote_id).collect();

                // Add new peers to the grow-only map with connected state
                // Mainly useful if peers are added after the peer monitor is started via admin API
                for peer_id in &current_peer_ids {
                    // Either reset the disconnect counter for existing peers or insert new peers
                    self.known_peers
                        .entry(*peer_id)
                        .and_modify(|state| state.disconnected_ticks = 0)
                        .or_insert(PeerState {
                            disconnected_ticks: 0,
                        });
                }

                // Detect disconnected peers (in known_peers but not in current_peer_ids)
                for (peer_id, state) in self.known_peers.iter_mut() {
                    if !current_peer_ids.contains(peer_id) {
                        // Increment disconnect tick count
                        state.disconnected_ticks += 1;

                        // Calculate disconnect duration
                        let disconnect_duration = self.interval * state.disconnected_ticks as u32;

                        tracing::warn!(
                            target: "flashblocks::p2p",
                            peer_id = %peer_id,
                            disconnected_for_secs = disconnect_duration.as_secs(),
                            "trusted peer disconnected (detected by peer monitor)"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    target: "flashblocks::p2p",
                    error = ?e,
                    "Failed to get trusted peers in peer monitor"
                );
            }
        }
    }
}
