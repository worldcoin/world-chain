use async_trait::async_trait;
use reth_network::events::{NetworkPeersEvents, PeerEvent};
use reth_network::{Peers, PeersInfo};

pub mod dispatcher;
pub mod listerners;

// Re-export commonly used dispatcher types for easier access
pub use dispatcher::{PeerEventsDispatcher, PeerEventsDispatcherBuilder};

/// A helper trait to bundle all necessary network traits for peer event handling.
/// This can be used as a bound (`N: NetworkPeersContext`) for generics.
pub trait NetworkPeersContext: NetworkPeersEvents + Peers + PeersInfo {}
impl<T> NetworkPeersContext for T where T: NetworkPeersEvents + Peers + PeersInfo {}

/// Trait for listening to peer events with access to the network handle.
///
/// Listeners are invoked concurrently by the dispatcher - each event is cloned and
/// dispatched to all registered listeners in parallel. This allows listeners to perform
/// blocking operations without delaying other listeners or new events.
///
/// The network handle provides full access to peer management capabilities through
/// the [`NetworkPeersContext`] trait, which bundles [`NetworkPeersEvents`], [`Peers`],
/// and [`PeersInfo`] traits.
///
/// # Type Parameters
/// - `N`: The network handle type that implements [`NetworkPeersContext`]
///
/// # Examples
/// ```no_run
/// use async_trait::async_trait;
/// use flashblocks_p2p::events::{NetworkPeersContext, PeerEventsListener};
/// use reth_network::events::PeerEvent;
///
/// pub struct MyListener;
///
/// #[async_trait]
/// impl<N> PeerEventsListener<N> for MyListener
/// where
///     N: NetworkPeersContext + Send + Sync,
/// {
///     async fn on_peer_event(&self, network: &N, event: &PeerEvent) {
///         match event {
///             PeerEvent::SessionClosed { peer_id, .. } => {
///                 // Access full network capabilities
///                 let trusted = network.get_trusted_peers();
///                 if let Ok(Some(peer)) = network.get_peer_by_id(*peer_id).await {
///                     // Handle peer disconnection
///                 }
///             }
///             _ => {}
///         }
///     }
/// }
/// ```
#[async_trait]
pub trait PeerEventsListener<N>: Send + Sync
where
    N: NetworkPeersContext + Send + Sync,
{
    /// Handle a peer event with access to the network handle.
    ///
    /// This method is called concurrently for each registered listener when an event occurs.
    /// The event is cloned for each listener, so listeners have independent ownership.
    ///
    /// # Parameters
    /// - `network`: Reference to the network handle for querying peer info, managing connections, etc.
    /// - `event`: The peer event to process (cloned for each listener)
    ///
    /// # Available Network Operations
    /// Through the `NetworkPeersContext` trait, the network handle provides:
    ///
    /// **From `Peers` trait:**
    /// - `add_peer()` / `remove_peer()` - Manage peer connections
    /// - `disconnect_peer()` / `disconnect_peer_with_reason()` - Terminate connections
    /// - `connect()` - Initiate outbound connections
    /// - `reputation_change()` - Update peer reputation
    ///
    /// **From `PeersInfo` trait:**
    /// - `num_connected_peers()` - Get connection count
    /// - `peer_by_id()` - Look up peer details by ID
    /// - `peers_iter()` - Iterate over all connected peers
    /// - `get_trusted_peers()` / `get_trusted_peer_ids()` - Query trusted peers
    /// - `add_trusted_peer_id()` / `remove_trusted_peer_id()` - Manage trusted peer list
    ///
    /// **From `NetworkPeersEvents` trait:**
    /// - `peer_events()` - Stream of peer lifecycle events
    async fn on_peer_event(&self, network: &N, event: &PeerEvent);
}
