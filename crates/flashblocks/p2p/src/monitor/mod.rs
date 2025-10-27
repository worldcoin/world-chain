//! Peer monitor for tracking trusted peers and logging warnings for disconnected peers
use futures::StreamExt;
use parking_lot::Mutex;
use reth_ethereum::network::api::PeerId;
use reth_network::events::{NetworkPeersEvents, PeerEvent};
use reth_network::{Peers, PeersInfo};
use reth_tasks::TaskExecutor;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tokio::time::interval;
use tokio_util::sync::CancellationToken;

/// Configuration for peer monitoring
#[derive(Debug, Clone, Copy)]
pub struct PeerMonitorConfig {
    /// Interval between peer monitor checks
    pub peer_monitor_interval: Duration,
    /// Connection initialization timeout
    pub connection_init_timeout: Duration,
}

/// State of trusted peer
#[derive(Debug, Clone, Copy)]
struct PeerState {
    connection_established: bool,
    added_time: Instant,
    disconnected_time: Option<Instant>,
}

/// Tracks the state of known peers via network handle
pub struct PeerMonitor<N> {
    network: N,
    trusted_peers: Mutex<HashMap<PeerId, PeerState>>,
    config: PeerMonitorConfig,
}

impl<N> PeerMonitor<N>
where
    N: Peers + PeersInfo + NetworkPeersEvents + Send + Sync + 'static,
{
    /// Create a new peer monitor
    pub fn new(network: N, config: PeerMonitorConfig) -> Self {
        let trusted_peers = Mutex::new(HashMap::new());
        Self {
            network,
            trusted_peers,
            config,
        }
    }

    /// Add initial peers to the peer monitor, e.g., from --trusted-peers CLI flag
    pub fn with_initial_peers(self, peers: impl IntoIterator<Item = PeerId>) -> Self {
        for peer_id in peers {
            self.add_peer_idle(peer_id);
        }

        self
    }

    /// Run the peer monitor on reth's task executor
    pub fn run_on_task_executor(self, task_executor: &TaskExecutor) {
        task_executor.spawn_with_graceful_shutdown_signal(|reth_shutdown| async move {
            let shutdown_token = CancellationToken::new();
            let shutdown_token_clone = shutdown_token.clone();

            tokio::spawn(async move {
                let _guard = reth_shutdown.await;
                shutdown_token_clone.cancel();
            });

            tracing::info!(
                target: "flashblocks::p2p",
                interval_secs = %self.config.peer_monitor_interval.as_secs(),
                "PeerMonitor started"
            );

            tokio::join! {
                self.periodically_check_peers(shutdown_token.clone()),
                self.process_peer_events(shutdown_token),
            };
        });
    }

    /// Periodically check trusted peers and log warnings for disconnected peers
    async fn periodically_check_peers(&self, shutdown_token: CancellationToken) {
        let mut interval = interval(self.config.peer_monitor_interval);
        loop {
            tracing::trace!(
                target: "flashblocks::p2p",
                trusted_peers = ?self.trusted_peers.lock().keys().collect::<Vec<&PeerId>>(),
                "tick"
            );

            tokio::select! {
                _ = interval.tick() => {
                    self.check_peers().await;
                }
                _ = shutdown_token.cancelled() => {
                    tracing::info!("Peer monitor shutting down");
                    break;
                }
            }
        }
    }

    /// Check current trusted peers against tracked trusted peers
    async fn check_peers(&self) {
        match self.network.get_trusted_peers().await {
            Ok(peers) => {
                let mut current_peer_ids: HashSet<PeerId> =
                    peers.iter().map(|p| p.remote_id).collect();

                // Detect disconnected peers (in known_peers but not in current_peer_ids)
                for (peer_id, state) in self.trusted_peers.lock().iter_mut() {
                    if !current_peer_ids.remove(peer_id) {
                        let now = Instant::now();
                        // If peer was connected and disconnected_time is None, set it to now
                        // Happens when connection event was not received e.g., because peer was added via admin RPC
                        if state.connection_established && state.disconnected_time.is_none() {
                            state.disconnected_time = Some(now);
                        }
                        // Case when peer was connected and then disconnected since disconnected_time is guaranteed to be set
                        let info = if let Some(dc_time) = state.disconnected_time {
                            let dc_since = now.duration_since(dc_time).as_secs();

                            format!("disconnected since {dc_since}s")
                        // Peer was never connected
                        } else {
                            "never connected".to_string()
                        };

                        // Log warning either if already connected peer is now disconnected or connection to trusted peer was not established before timeout
                        if state.connection_established
                            || now.duration_since(state.added_time) > self.config.connection_init_timeout
                        {
                            tracing::warn!(
                                target: "flashblocks::p2p",
                                peer_id = %peer_id,
                                info = %info,
                                "trusted peer disconnected"
                            );
                        }
                    }
                }
                // Update tracked peers and connection status
                for peer_id in current_peer_ids {
                    self.add_or_update_connected(peer_id);
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

    /// Additionally process peer events to get more accurate information about peer connections and allow for removing trusted peers via admin RPC
    async fn process_peer_events(&self, shutdown_token: CancellationToken) {
        self.network
            .peer_events()
            .take_until(shutdown_token.cancelled())
            .for_each(|event| async {
                match event {
                    // Fired when connection with any peer is closed
                    PeerEvent::SessionClosed { peer_id, reason } => {
                        if self.update_peer_state(peer_id, |state| {
                            state.disconnected_time = Some(Instant::now());
                        }) {
                            let reason = reason
                                .map(|r| r.to_string())
                                .unwrap_or("unknown".to_string());

                            tracing::info!(
                                target: "flashblocks::p2p",
                                peer_id = %peer_id,
                                reason = %reason,
                                "trusted peer disconnected"
                            );
                        }
                    }
                    // Fired when connection with any peer is established
                    PeerEvent::SessionEstablished(session_info) => {
                        // Note: If the peer was added via admin RPC, it will not be in the trusted_peers map yet.
                        if self.update_peer_state(session_info.peer_id, |state| {
                            state.connection_established = true;
                            state.disconnected_time = None;
                        }) {
                            tracing::info!(
                                target: "flashblocks::p2p",
                                peer_id = %session_info.peer_id,
                                "connection to trusted peer established"
                            );
                        }
                    }
                    // Fired when any peer is removed via admin RPC
                    PeerEvent::PeerRemoved(fixed_bytes) => {
                        if self.remove_peer(fixed_bytes) {
                            tracing::debug!(
                                target: "flashblocks::p2p",
                                peer_id = %fixed_bytes,
                                "trusted peer removed"
                            );
                        }
                    }
                    // Fired when any peer is added via admin RPC
                    PeerEvent::PeerAdded(_) => {
                        // Don't do anything here, can not tell if it's a trusted peer or not due to reth API limitations
                    }
                }
            })
            .await;
    }

    /// Add a peer with no connection established
    fn add_peer_idle(&self, peer_id: PeerId) {
        let mut trusted_peers = self.trusted_peers.lock();

        let peer_state = PeerState {
            connection_established: false,
            added_time: Instant::now(),
            disconnected_time: None,
        };

        trusted_peers.insert(peer_id, peer_state);
    }

    /// Either insert a new peer with connection established or update the connection status if peer already exists
    fn add_or_update_connected(&self, peer_id: PeerId) {
        self.trusted_peers
            .lock()
            .entry(peer_id)
            .and_modify(|state| {
                state.connection_established = true;
                state.disconnected_time = None;
            })
            .or_insert(PeerState {
                connection_established: true,
                added_time: Instant::now(),
                disconnected_time: None,
            });
    }

    /// Remove a peer from the peer monitor
    fn remove_peer(&self, peer_id: PeerId) -> bool {
        let mut trusted_peers = self.trusted_peers.lock();
        trusted_peers.remove(&peer_id).is_some()
    }

    /// Update the state of a peer
    fn update_peer_state(&self, peer_id: PeerId, f: impl FnOnce(&mut PeerState)) -> bool {
        let mut trusted_peers = self.trusted_peers.lock();
        trusted_peers.get_mut(&peer_id).map(f).is_some()
    }
}
