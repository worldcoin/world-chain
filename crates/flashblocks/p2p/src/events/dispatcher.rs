use crate::events::NetworkPeersContext;

use super::PeerEventsListener;
use futures::{stream::FuturesUnordered, StreamExt};
use reth_network::events::{NetworkPeersEvents, PeerEvent};
use std::marker::PhantomData;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

mod sealed {
    /// Sealed marker trait for builder states.
    pub trait BuilderState {}
}

use sealed::BuilderState;

// Typestate markers for the builder
// These must be public for the builder to be usable outside the crate,
// but they are not intended for direct use (only as type parameters)
pub struct Uninitialized;
pub struct NetworkConfigured;
pub struct HasListeners;

impl BuilderState for Uninitialized {}
impl BuilderState for NetworkConfigured {}
impl BuilderState for HasListeners {}

/// A builder for [`PeerEventsDispatcher`] that uses the typestate pattern to enforce
/// correct construction order at compile time.
///
/// The builder goes through three states:
/// - `Uninitialized`: Initial state, must call `with_network` to configure
/// - `NetworkConfigured`: Network handle set, must add at least one listener
/// - `HasListeners`: At least one listener added, can add more or build
///
/// # Example
/// ```no_run
/// # use flashblocks_p2p::events::{PeerEventsDispatcherBuilder, PeerEventsListener};
/// # use reth_network::NetworkHandle;
/// # async fn example(network_handle: NetworkHandle, my_listener: impl PeerEventsListener<NetworkHandle> + 'static, another_listener: impl PeerEventsListener<NetworkHandle> + 'static) {
/// let handle = PeerEventsDispatcherBuilder::new()
///     .with_network(network_handle)
///     .add_listener(Box::new(my_listener))  // Transitions to HasListeners
///     .add_listener(Box::new(another_listener))  // Can add more
///     .build()  // No Result needed - always valid!
///     .start();  // Returns DispatcherHandle
///
/// // Later, shutdown gracefully
/// handle.shutdown().await;
/// # }
/// ```
#[allow(private_interfaces)]
pub struct PeerEventsDispatcherBuilder<N, State = Uninitialized>
where
    State: BuilderState,
{
    network: Option<N>,
    listeners: Vec<Box<dyn PeerEventsListener<N>>>,
    _state: PhantomData<State>,
}

// Initial state: Uninitialized
impl<N> PeerEventsDispatcherBuilder<N, Uninitialized> {
    pub fn new() -> Self {
        Self {
            network: None,
            listeners: Vec::new(),
            _state: PhantomData,
        }
    }

    /// Set the network handle and transition to NetworkConfigured state.
    pub fn with_network(self, network: N) -> PeerEventsDispatcherBuilder<N, NetworkConfigured> {
        PeerEventsDispatcherBuilder {
            network: Some(network),
            listeners: self.listeners,
            _state: PhantomData,
        }
    }
}

impl<N> Default for PeerEventsDispatcherBuilder<N, Uninitialized> {
    fn default() -> Self {
        Self::new()
    }
}

// NetworkConfigured state: Must add at least one listener
impl<N> PeerEventsDispatcherBuilder<N, NetworkConfigured> {
    /// Add listener to the dispatcher.
    pub fn add_listener(
        mut self,
        listener: Box<dyn PeerEventsListener<N>>,
    ) -> PeerEventsDispatcherBuilder<N, HasListeners> {
        self.listeners.push(listener);
        PeerEventsDispatcherBuilder {
            network: self.network,
            listeners: self.listeners,
            _state: PhantomData,
        }
    }
}

// HasListeners state: Can add more listeners or build
impl<N> PeerEventsDispatcherBuilder<N, HasListeners> {
    /// Add listener to the dispatcher.
    pub fn add_listener(mut self, listener: Box<dyn PeerEventsListener<N>>) -> Self {
        self.listeners.push(listener);
        self
    }

    /// Build the dispatcher.
    pub fn build(self) -> PeerEventsDispatcher<N> {
        PeerEventsDispatcher {
            network: self
                .network
                .expect("network must be set in HasListeners state"),
            listeners: self.listeners,
        }
    }
}

/// A peer events dispatcher that distributes network peer events to registered listeners.
///
/// Once started, the dispatcher runs in the background and cannot be reused.
/// Use [`PeerEventsDispatcherBuilder`] to construct instances.
pub struct PeerEventsDispatcher<N> {
    network: N,
    listeners: Vec<Box<dyn PeerEventsListener<N>>>,
}

/// Handle to a running dispatcher that allows awaiting completion and graceful shutdown.
#[derive(Debug)]
pub struct DispatcherHandle {
    task: JoinHandle<()>,
    shutdown: CancellationToken,
}

impl DispatcherHandle {
    /// Gracefully shutdown the dispatcher.
    /// This signals the background task to stop and waits for it to complete.
    pub async fn shutdown(self) {
        self.shutdown.cancel();
        let _ = self.task.await;
    }

    /// Wait for the dispatcher task to complete.
    /// This will block until the dispatcher is shutdown or the task panics.
    pub async fn wait(self) -> Result<(), tokio::task::JoinError> {
        self.task.await
    }

    /// Get a reference to the shutdown token.
    /// This can be cloned and used to trigger shutdown from other parts of the code.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }
}

impl<N> PeerEventsDispatcher<N>
where
    N: NetworkPeersContext + Send + Sync + 'static,
{
    /// Start the dispatcher.
    /// This spawns a background task that listens for peer events and dispatches
    /// them to all registered listeners.
    ///
    /// Returns a [`DispatcherHandle`] that can be used to await completion or
    /// trigger graceful shutdown.
    ///
    /// # Examples
    /// ```no_run
    /// # use flashblocks_p2p::events::PeerEventsDispatcher;
    /// # use reth_network::NetworkHandle;
    /// # async fn example(dispatcher: PeerEventsDispatcher<NetworkHandle>) {
    /// // Start and run in background
    /// let handle = dispatcher.start();
    ///
    /// // Later, shutdown gracefully
    /// handle.shutdown().await;
    /// # }
    /// ```
    pub fn start(self) -> DispatcherHandle {
        let network = Arc::new(self.network);
        let listeners = Arc::new(self.listeners);
        let shutdown = CancellationToken::new();
        let shutdown_token = shutdown.clone();

        let task = tokio::spawn(async move {
            network
                .peer_events()
                .take_until(shutdown_token.cancelled())
                .for_each_concurrent(None, |event| {
                    let network = Arc::clone(&network);
                    let listeners = Arc::clone(&listeners);
                    async move {
                        Self::dispatch_concurrent(&network, &listeners, &event).await;
                    }
                })
                .await;
        });

        DispatcherHandle { task, shutdown }
    }

    /// Dispatch an event to all registered listeners concurrently.
    /// Each listener runs in its own task, allowing multiple listeners to process
    /// the same event simultaneously and new events to be processed while previous
    /// ones are still being handled.
    async fn dispatch_concurrent(
        network: &N,
        listeners: &[Box<dyn PeerEventsListener<N>>],
        event: &PeerEvent,
    ) {
        listeners
            .iter()
            .map(|listener| {
                let event = event.clone();
                async move {
                    listener.on_peer_event(network, &event).await;
                }
            })
            .collect::<FuturesUnordered<_>>()
            .collect::<Vec<_>>()
            .await;
    }
}
