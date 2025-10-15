use async_trait::async_trait;
use reth_network::events::{NetworkPeersEvents, PeerEvent};
use reth_network::{Peers, PeersInfo};

pub mod dispatcher;
pub mod listerners;

pub use dispatcher::{PeerEventsDispatcher, PeerEventsDispatcherBuilder};

pub trait NetworkPeersContext: NetworkPeersEvents + Peers + PeersInfo {}
impl<T> NetworkPeersContext for T where T: NetworkPeersEvents + Peers + PeersInfo {}

/// Trait for listening to peer events with access to the network handle.
#[async_trait]
pub trait PeerEventsListener<N>: Send + Sync
where
    N: NetworkPeersContext + Send + Sync,
{
    /// Handle a peer event with access to the network handle for peer management/ info
    async fn on_peer_event(&self, network: &N, event: &PeerEvent);
}
