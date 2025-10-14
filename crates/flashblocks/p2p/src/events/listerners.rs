use async_trait::async_trait;
use reth_network::events::PeerEvent;
use tracing::error;

use crate::events::{NetworkPeersContext, PeerEventsListener};

/// Logs error when a trusted peer disconnects.
pub struct TrustedPeerDisconnectedAlert;

#[async_trait]
impl<N> PeerEventsListener<N> for TrustedPeerDisconnectedAlert
where
    N: NetworkPeersContext + Send + Sync,
{
    async fn on_peer_event(&self, network: &N, event: &PeerEvent) {
        if let &PeerEvent::SessionClosed { peer_id, reason } = event {
            let peer_addr = network
                .get_peer_by_id(peer_id)
                .await
                .ok()
                .flatten()
                .map(|p| p.remote_addr.to_string())
                .unwrap_or(String::from("unknown"));

            let reason = reason
                .map(|r| r.to_string())
                .unwrap_or(String::from("unknown"));

            error!(
                target: "flashblocks::p2p",
                peer_id = %peer_id,
                peer_addr = %peer_addr,
                reason = %reason,
                "trusted peer disconnected"
            );
        }
    }
}
