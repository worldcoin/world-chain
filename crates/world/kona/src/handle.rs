//! Kona service lifecycle management.
//!
//! This module provides [`KonaServiceHandle`], which manages the lifecycle of the Kona consensus
//! node running in-process alongside reth.
//!
//! # Architecture
//!
//! The Kona node is composed of several concurrent actors:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    KonaServiceHandle                         │
//! │                                                              │
//! │  ┌───────────┐  ┌──────────────┐  ┌────────────┐           │
//! │  │ L1 Watcher│  │  Derivation  │  │  Network   │           │
//! │  │  Actor    │  │    Actor     │  │   Actor    │           │
//! │  └─────┬─────┘  └──────┬───────┘  └─────┬──────┘           │
//! │        │               │                │                   │
//! │        │     ┌─────────▼─────────┐      │                   │
//! │        └────►│   Engine Actor    │◄─────┘                   │
//! │              │  (in-process EL)  │                           │
//! │              └───────────────────┘                           │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! All actors communicate via tokio channels and share a [`CancellationToken`] for graceful
//! shutdown. The engine actor uses the [`InProcessEngineClient`] to dispatch calls to reth.

use crate::{InProcessEngineClient, KonaConfig};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Handle to a running Kona consensus node.
///
/// This struct manages the lifecycle of the Kona node actors and provides a cancellation token
/// for graceful shutdown. It is designed to be held alongside reth's `NodeHandle` in the
/// world-chain binary.
///
/// # Lifecycle
///
/// 1. **Creation**: Call [`KonaServiceHandle::spawn`] after reth has fully launched and the
///    engine handle + provider are available.
/// 2. **Running**: The Kona actors run concurrently in the same tokio runtime as reth. The
///    consensus pipeline drives the execution engine via in-process calls.
/// 3. **Shutdown**: Drop the handle or call [`KonaServiceHandle::shutdown`] to cancel all
///    actors gracefully.
///
/// # Example (conceptual)
///
/// ```rust,ignore
/// // After reth launches:
/// let NodeHandle { node_exit_future, node } = builder.node(world_chain_node).launch().await?;
///
/// // Extract engine internals and spawn Kona:
/// let kona_handle = KonaServiceHandle::spawn(kona_config, engine_client).await?;
///
/// // Wait for either reth or kona to exit:
/// tokio::select! {
///     result = node_exit_future => { /* reth exited */ },
///     result = kona_handle.stopped() => { /* kona exited */ },
/// }
/// ```
pub struct KonaServiceHandle {
    /// Cancellation token shared by all Kona actors.
    ///
    /// When cancelled, all actors will begin their graceful shutdown sequence.
    cancellation: CancellationToken,

    /// The join handle for the Kona service's main task.
    ///
    /// This future resolves when all Kona actors have exited.
    task_handle: tokio::task::JoinHandle<Result<(), String>>,
}

impl KonaServiceHandle {
    /// Spawn the Kona consensus node as an in-process service.
    ///
    /// This starts all Kona actors (L1 watcher, derivation pipeline, engine actor, network)
    /// in the current tokio runtime. The engine actor uses the provided [`InProcessEngineClient`]
    /// to dispatch Engine API calls directly to reth.
    ///
    /// # Arguments
    ///
    /// * `config` — Kona-specific configuration (L1 RPC, beacon URL, P2P settings, etc.)
    /// * `engine_client` — The in-process engine client connected to reth's engine handler.
    ///
    /// # Returns
    ///
    /// A [`KonaServiceHandle`] that can be used to monitor or shut down the Kona service.
    ///
    /// # Errors
    ///
    /// Returns an error if the Kona node builder fails to assemble the service (e.g., invalid
    /// rollup config, network initialization failure).
    pub async fn spawn<L2Provider>(
        config: KonaConfig,
        _engine_client: InProcessEngineClient<L2Provider>,
    ) -> eyre::Result<Self>
    where
        L2Provider: reth_provider::BlockReaderIdExt
            + reth_storage_api::BlockReader
            + reth_provider::HeaderProvider
            + reth_provider::StateProviderFactory
            + Clone
            + Send
            + Sync
            + 'static,
    {
        let cancellation = CancellationToken::new();
        let cancel_clone = cancellation.clone();
        let rollup_config = config.rollup_config.clone();

        info!(
            target: "world_chain::kona",
            chain_id = rollup_config.chain_id,
            block_time = rollup_config.block_time,
            "Starting Kona consensus node (in-process mode)"
        );

        let task_handle = tokio::spawn(async move {
            // TODO(kona-integration): Wire up the full Kona RollupNode here.
            //
            // The full implementation would:
            //
            // 1. Construct a `RollupNodeBuilder` with:
            //    - rollup_config from `config`
            //    - L1 providers (beacon client, chain provider) from `config.l1_*`
            //    - Engine config pointing to our in-process client
            //    - P2P config from `config.p2p_*`
            //    - Optional RPC config
            //
            // 2. However, the current `RollupNodeBuilder` constructs its own `OpEngineClient`
            //    internally (in `create_engine_actor`). To use our `InProcessEngineClient`,
            //    we need to either:
            //    a. Modify Kona upstream to accept a generic/injected engine client, or
            //    b. Construct the individual Kona actors manually (EngineActor, DerivationActor,
            //       L1WatcherActor, NetworkActor) with our in-process client.
            //
            // 3. Option (b) is more practical for a WIP integration. The actor wiring would be:
            //
            //    ```rust,ignore
            //    // Create engine state and task queue
            //    let engine_state = EngineState::default();
            //    let (state_tx, state_rx) = watch::channel(engine_state);
            //    let (queue_tx, queue_rx) = watch::channel(0);
            //    let engine = Engine::new(engine_state, state_tx, queue_tx);
            //
            //    // Create the engine processor with our in-process client
            //    let engine_processor = EngineProcessor::new(
            //        Arc::new(engine_client),
            //        rollup_config.clone(),
            //        derivation_client,
            //        engine,
            //        None, // unsafe_head_tx (only in sequencer mode)
            //    );
            //
            //    // Create derivation pipeline
            //    let pipeline = OnlinePipeline::new_polled(
            //        rollup_config.clone(),
            //        l1_chain_config,
            //        blob_provider,
            //        l1_provider,
            //        l2_provider,
            //    );
            //
            //    // Create and start derivation actor
            //    let derivation = DerivationActor::new(
            //        derivation_engine_client,
            //        cancellation.clone(),
            //        derivation_rx,
            //        pipeline,
            //    );
            //
            //    // Spawn all actors...
            //    ```
            //
            // For now, we just log and wait for cancellation, demonstrating the lifecycle
            // management.

            info!(
                target: "world_chain::kona",
                "Kona service started — awaiting full actor wiring (WIP)"
            );

            // In the complete implementation, this would be replaced by the actor supervision
            // loop that monitors all spawned actors and handles failures.
            cancel_clone.cancelled().await;

            info!(
                target: "world_chain::kona",
                "Kona service received shutdown signal"
            );

            Ok(())
        });

        Ok(Self {
            cancellation,
            task_handle,
        })
    }

    /// Returns a future that resolves when the Kona service stops.
    ///
    /// This can be used in a `tokio::select!` alongside reth's exit future.
    pub async fn stopped(self) -> Result<(), String> {
        match self.task_handle.await {
            Ok(result) => result,
            Err(join_error) => {
                error!(
                    target: "world_chain::kona",
                    %join_error,
                    "Kona service task panicked"
                );
                Err(format!("Kona service task panicked: {join_error}"))
            }
        }
    }

    /// Initiates graceful shutdown of the Kona service.
    ///
    /// This cancels all Kona actors. The [`stopped`](Self::stopped) future will resolve
    /// shortly after.
    pub fn shutdown(&self) {
        warn!(
            target: "world_chain::kona",
            "Shutting down Kona consensus node"
        );
        self.cancellation.cancel();
    }

    /// Returns a reference to the cancellation token.
    ///
    /// This can be used to link Kona's lifecycle to other components.
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation
    }
}

impl Drop for KonaServiceHandle {
    fn drop(&mut self) {
        // Ensure actors are cancelled when the handle is dropped.
        self.cancellation.cancel();
    }
}
