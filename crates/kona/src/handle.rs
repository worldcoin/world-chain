//! Kona service lifecycle management.
//!
//! This module provides [`KonaServiceHandle`], which manages the lifecycle of the Kona consensus
//! node running in-process alongside reth.
//!
//! # Architecture
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

use crate::{InProcessEngineClient, KonaConfig};
use alloy_rpc_types_engine::JwtSecret;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use url::Url;

/// Handle to a running Kona consensus node.
///
/// Manages the lifecycle of the Kona node actors and provides a cancellation token for graceful
/// shutdown. Designed to be held alongside reth's `NodeHandle` in the world-chain binary.
pub struct KonaServiceHandle {
    cancellation: CancellationToken,

    /// Wrapped in `Option` so [`stopped`](Self::stopped) can take ownership
    /// without conflicting with the `Drop` impl.
    task_handle: Option<tokio::task::JoinHandle<Result<(), String>>>,
}

impl KonaServiceHandle {
    /// Spawn the Kona consensus node as an in-process service.
    ///
    /// This builds a [`kona_node_service::RollupNode`] from the provided [`KonaConfig`] and
    /// starts it using [`RollupNode::start_with_engine_client`], which injects the provided
    /// [`InProcessEngineClient`] into Kona's actor system instead of constructing a default
    /// HTTP-based `OpEngineClient`.
    ///
    /// # Arguments
    ///
    /// * `config` — Kona-specific configuration (L1 RPC, beacon URL, P2P settings, etc.)
    /// * `engine_client` — The in-process engine client connected to reth's engine handler.
    /// * `l2_rpc_url` — URL of reth's L2 RPC endpoint (for Kona's derivation pipeline).
    /// * `jwt_secret` — JWT secret for the L2 RPC endpoint.
    pub async fn spawn<L2Provider>(
        config: KonaConfig,
        engine_client: InProcessEngineClient<L2Provider>,
        l2_rpc_url: Url,
        jwt_secret: JwtSecret,
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
        let rollup_config = config.rollup_config.clone();

        info!(
            target: "world_chain::kona",
            l2_chain_id = %rollup_config.l2_chain_id,
            block_time = rollup_config.block_time,
            "Starting Kona consensus node (in-process mode)"
        );

        let engine_config = config.make_engine_config(l2_rpc_url, jwt_secret);

        let l2_chain_id = rollup_config.l2_chain_id;
        let p2p_config = config.p2p.build_network_config(&rollup_config, l2_chain_id)?;

        let rollup_node = config.build_rollup_node(engine_config, p2p_config);
        let engine_client = Arc::new(engine_client);

        let task_handle = tokio::spawn(async move {
            rollup_node
                .start_with_engine_client(engine_client)
                .await
        });

        Ok(Self {
            cancellation,
            task_handle: Some(task_handle),
        })
    }

    /// Returns a future that resolves when the Kona service stops.
    pub async fn stopped(&mut self) -> Result<(), String> {
        let handle = self
            .task_handle
            .take()
            .ok_or_else(|| "Kona service already stopped".to_string())?;

        match handle.await {
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
    pub fn shutdown(&self) {
        warn!(
            target: "world_chain::kona",
            "Shutting down Kona consensus node"
        );
        self.cancellation.cancel();
    }

    /// Returns a reference to the cancellation token.
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation
    }
}

impl Drop for KonaServiceHandle {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}
