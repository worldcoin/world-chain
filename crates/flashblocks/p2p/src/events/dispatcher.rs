use crate::events::NetworkPeersContext;

use super::PeerEventsListener;
use futures::{stream::FuturesUnordered, StreamExt};
use reth_network::events::{NetworkPeersEvents, PeerEvent};
use reth_tasks::TaskExecutor;
use std::marker::PhantomData;
use std::sync::Arc;
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

/// A typestate builder for [`PeerEventsDispatcher`].
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

impl<N> PeerEventsDispatcher<N>
where
    N: NetworkPeersContext + Send + Sync + 'static,
{
    /// Run the dispatcher until shutdown is signaled.
    /// Listens for peer events and dispatches them concurrently to all registered listeners.
    pub async fn run(self, shutdown_token: CancellationToken) {
        let network = Arc::new(self.network);
        let listeners = Arc::new(self.listeners);

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
    }

    pub fn run_on_task_executor(self, task_executor: &TaskExecutor) {
        task_executor.spawn_critical_with_graceful_shutdown_signal(
            "flashblocks p2p event listeners",
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
