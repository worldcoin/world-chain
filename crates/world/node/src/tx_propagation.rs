use std::collections::HashSet;

use reth_eth_wire_types::primitives::NetworkPrimitives;
use reth_network::transactions::config::TransactionPropagationPolicy;
use reth_network::transactions::PeerMetadata;
use reth_network_peers::PeerId;

/// Transaction propagation policy for World Chain that restricts propagation to a specific peer list.
///
/// Transactions will only be propagated to peers whose IDs are in the allowed set.
#[derive(Debug, Clone)]
pub struct WorldChainTransactionPropagationPolicy {
    allowed_peers: HashSet<PeerId>,
}

impl WorldChainTransactionPropagationPolicy {
    /// Creates a new propagation policy that only propagates to the specified peers
    pub fn new(peers: impl IntoIterator<Item = PeerId>) -> Self {
        Self {
            allowed_peers: peers.into_iter().collect(),
        }
    }

    /// Returns the number of allowed peers
    pub fn peer_count(&self) -> usize {
        self.allowed_peers.len()
    }
}

impl TransactionPropagationPolicy for WorldChainTransactionPropagationPolicy {
    fn can_propagate<N: NetworkPrimitives>(&self, peer: &mut PeerMetadata<N>) -> bool {
        // Access peer_id via request_tx().peer_id
        let peer_id = &peer.request_tx().peer_id;
        let allowed = self.allowed_peers.contains(peer_id);

        // FIXME: Remove
        tracing::debug!(
            target: "world_chain::tx_propagation",
            ?peer_id,
            allowed,
            allowed_peer_count = self.allowed_peers.len(),
            "Checking if transactions can be propagated to peer"
        );

        allowed
    }

    fn on_session_established<N: NetworkPrimitives>(&mut self, _peer: &mut PeerMetadata<N>) {
        // No dynamic updates needed
    }

    fn on_session_closed<N: NetworkPrimitives>(&mut self, _peer: &mut PeerMetadata<N>) {
        // No cleanup needed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reth_eth_wire::EthVersion;
    use reth_eth_wire_types::EthNetworkPrimitives;
    use reth_network::test_utils::new_mock_session;

    /// Helper to create test peer metadata for a given peer ID
    fn create_test_peer(peer_id: PeerId) -> PeerMetadata<EthNetworkPrimitives> {
        let (peer, _rx) = new_mock_session(peer_id, EthVersion::Eth68);
        peer
    }

    #[test]
    fn test_can_propagate_allowed_peer() {
        let allowed = PeerId::random();
        let policy = WorldChainTransactionPropagationPolicy::new(vec![allowed]);

        let mut peer_metadata = create_test_peer(allowed);

        assert!(
            policy.can_propagate(&mut peer_metadata),
            "Should allow propagation to allowed peer"
        );
    }

    #[test]
    fn test_cannot_propagate_disallowed_peer() {
        let allowed = PeerId::random();
        let disallowed = PeerId::random();
        let policy = WorldChainTransactionPropagationPolicy::new(vec![allowed]);

        let mut peer_metadata = create_test_peer(disallowed);

        assert!(
            !policy.can_propagate(&mut peer_metadata),
            "Should not allow propagation to disallowed peer"
        );
    }
}
