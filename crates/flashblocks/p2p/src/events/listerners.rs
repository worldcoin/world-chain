use async_trait::async_trait;
use reth_network::events::PeerEvent;

use crate::events::{NetworkPeersContext, PeerEventsListener};

/// Logs error when a trusted peer disconnects.
pub struct TrustedPeerDisconnectedAlert;

#[async_trait]
impl<N> PeerEventsListener<N> for TrustedPeerDisconnectedAlert
where
    N: NetworkPeersContext + Send + Sync,
{
    async fn on_peer_event(&self, _network: &N, event: &PeerEvent) {
        if let &PeerEvent::SessionClosed { peer_id, reason } = event {
            let reason = reason
                .map(|r| r.to_string())
                .unwrap_or(String::from("unknown"));

            tracing::warn!(
                target: "flashblocks::p2p",
                peer_id = %peer_id,
                reason = %reason,
                "trusted peer disconnected"
            );
        }
    }
}
