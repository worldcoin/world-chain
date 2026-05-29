//! Manual assembly of the Kona rollup node actors around an in-process engine client.
//!
//! Canonical kona drives reth's execution engine over the authenticated Engine API (HTTP + JWT)
//! via [`kona_node_service::RollupNode::start`], which hard-wires its [`EngineActor`] to an
//! [`kona_engine::OpEngineClient`]. To run the consensus hot path **in-process**, we reproduce the
//! same actor graph here but inject an [`WorldChainKonaEngineClient`](crate::WorldChainKonaEngineClient) into
//! the [`EngineProcessor`]/[`EngineRpcProcessor`].
//!
//! The actor graph is identical to upstream:
//!
//! ```text
//!   ┌───────────┐   ┌──────────────┐   ┌────────────┐
//!   │ L1 Watcher│   │  Derivation  │   │  Network   │
//!   └─────┬─────┘   └──────┬───────┘   └─────┬──────┘
//!         │                │                 │
//!         │      ┌─────────▼─────────┐       │
//!         └─────►│   Engine Actor    │◄──────┘
//!                │  (in-process EL)  │
//!                └─────────┬─────────┘
//!        (+ optional Sequencer actor, + optional RPC actor)
//! ```

use std::{sync::Arc, time::Duration};

use alloy_provider::{IpcConnect, RootProvider};
use alloy_rpc_client::ClientBuilder;
use kona_derive::StatefulAttributesBuilder;
use kona_engine::{Engine, EngineState};
use kona_genesis::{L1ChainConfig, RollupConfig};
use kona_node_service::{
    BlockStream, ConductorClient, DelayedL1OriginSelectorProvider, DerivationActor, EngineActor,
    EngineProcessor, EngineRpcProcessor, L1OriginSelector, L1WatcherActor, NetworkActor,
    NetworkBuilder, NetworkConfig, NetworkInboundData, NodeActor, QueuedDerivationEngineClient,
    QueuedEngineDerivationClient, QueuedEngineRpcClient, QueuedL1WatcherDerivationClient,
    QueuedNetworkEngineClient, QueuedSequencerAdminAPIClient, QueuedSequencerEngineClient,
    QueuedUnsafePayloadGossipClient, RpcActor, RpcContext, SequencerActor, SequencerConfig,
};
use kona_protocol::L2BlockInfo;
use kona_providers_alloy::{
    AlloyChainProvider, AlloyL2ChainProvider, OnlineBeaconClient, OnlineBlobProvider,
    OnlinePipeline,
};
use kona_rpc::RpcBuilder;
use op_alloy_network::Optimism;
use std::ops::Not as _;
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::{KonaConfig, WorldChainKonaEngineClient};
use reth_engine_primitives::ConsensusEngineHandle;
use url::Url;

/// How the in-process Kona node reaches reth's standard (non-engine) L2 RPC.
///
/// This transport is used only for the derivation pipeline and the engine actor's infrequent
/// reads — never for the consensus hot path, which is dispatched in-process via
/// [`ConsensusEngineHandle`]. IPC is preferred when reth's IPC server is enabled; otherwise we
/// fall back to reth's HTTP RPC endpoint.
#[derive(Debug, Clone)]
pub enum L2RpcEndpoint {
    /// reth's IPC RPC endpoint (socket path).
    Ipc(String),
    /// reth's HTTP RPC endpoint.
    Http(Url),
}
use reth_optimism_node::OpEngineTypes;
use reth_payload_builder::PayloadStore;

const DERIVATION_PROVIDER_CACHE_SIZE: usize = 1024;
const HEAD_STREAM_POLL_INTERVAL: u64 = 4;
const FINALIZED_STREAM_POLL_INTERVAL: u64 = 60;
const CHANNEL_SIZE: usize = 1024;

/// Inputs required to assemble and run the Kona consensus node in-process.
///
/// These are the canonical kona node settings *minus* the Engine API transport, which is replaced
/// by the in-process [`WorldChainKonaEngineClient`].
pub struct KonaService {
    /// The OP Stack rollup configuration, shared with reth.
    pub rollup_config: Arc<RollupConfig>,
    /// The L1 chain configuration.
    pub l1_chain_config: Arc<L1ChainConfig>,
    /// Whether to trust the L1 RPC without receipt re-verification.
    pub l1_trust_rpc: bool,
    /// The L1 EL provider (used by derivation, sequencer origin selection, and L1 watcher).
    pub l1_provider: RootProvider,
    /// The L1 beacon client (blob data availability).
    pub l1_beacon: OnlineBeaconClient,
    /// The L2 EL provider over reth's standard IPC RPC (used by derivation for safe-head reads).
    pub l2_provider: RootProvider<Optimism>,
    /// The in-process engine client driving reth's execution engine.
    pub engine_client: Arc<WorldChainKonaEngineClient>,
    /// Whether the node runs in sequencer mode.
    pub sequencer_mode: bool,
    /// The sequencer configuration.
    pub sequencer_config: SequencerConfig,
    /// The P2P network configuration.
    pub p2p_config: NetworkConfig,
    /// The Kona node RPC server configuration, if enabled.
    pub rpc_builder: Option<RpcBuilder>,
}

impl KonaService {
    /// Assembles a [`KonaService`] from a [`KonaConfig`] and the reth execution-layer handles
    /// obtained from the node add-ons after launch.
    ///
    /// Constructs the [`WorldChainKonaEngineClient`] (wrapping the engine handle + payload store + L2/L1
    /// providers), the L1 beacon client, and the P2P network configuration. The `l2_endpoint` should
    /// point at reth's standard (unauthenticated) RPC — IPC when available, otherwise HTTP; it is
    /// used only for the derivation pipeline and the engine actor's infrequent reads, never for the
    /// consensus hot path. The L1 provider remains an HTTP connection (`--kona.l1-rpc-url`).
    pub async fn build(
        config: KonaConfig,
        engine_handle: ConsensusEngineHandle<OpEngineTypes>,
        payload_store: PayloadStore<OpEngineTypes>,
        l2_endpoint: L2RpcEndpoint,
    ) -> eyre::Result<Self> {
        let l1_provider = RootProvider::new_http(config.l1_rpc_url.clone());
        let l2_client = match l2_endpoint {
            L2RpcEndpoint::Ipc(path) => {
                ClientBuilder::default().ipc(IpcConnect::new(path)).await?
            }
            L2RpcEndpoint::Http(url) => ClientBuilder::default().http(url),
        };
        let l2_provider = RootProvider::<Optimism>::new(l2_client);

        let engine_client = Arc::new(WorldChainKonaEngineClient::new(
            config.rollup_config.clone(),
            engine_handle,
            payload_store,
            l2_provider.clone(),
            l1_provider.clone(),
        ));

        let l2_chain_id: u64 = config.rollup_config.l2_chain_id.into();
        let p2p_config = config
            .p2p
            .clone()
            .build_network_config(&config.rollup_config, l2_chain_id)?;

        let mut l1_beacon = OnlineBeaconClient::new_http(config.l1_beacon_url.to_string());
        if let Some(slot) = config.l1_slot_duration_override {
            l1_beacon = l1_beacon.with_l1_slot_duration_override(slot);
        }

        Ok(Self {
            rollup_config: config.rollup_config.clone(),
            l1_chain_config: Arc::new(L1ChainConfig::default()),
            l1_trust_rpc: config.l1_trust_rpc,
            l1_provider,
            l1_beacon,
            l2_provider,
            engine_client,
            sequencer_mode: config.sequencer_mode,
            sequencer_config: config.make_sequencer_config(),
            p2p_config,
            rpc_builder: config.make_rpc_builder(),
        })
    }

    /// Builds an [`AlloyChainProvider`] for the L1 chain.
    fn l1_derivation_provider(&self) -> AlloyChainProvider {
        AlloyChainProvider::new_with_trust(
            self.l1_provider.clone(),
            DERIVATION_PROVIDER_CACHE_SIZE,
            self.l1_trust_rpc,
        )
    }

    /// Builds an [`AlloyL2ChainProvider`] over reth's standard L2 RPC.
    fn l2_derivation_provider(&self) -> AlloyL2ChainProvider {
        AlloyL2ChainProvider::new_with_trust(
            self.l2_provider.clone(),
            self.rollup_config.clone(),
            DERIVATION_PROVIDER_CACHE_SIZE,
            false,
        )
    }

    /// Builds the [`StatefulAttributesBuilder`] used by the sequencer actor.
    fn create_attributes_builder(
        &self,
    ) -> StatefulAttributesBuilder<AlloyChainProvider, AlloyL2ChainProvider> {
        StatefulAttributesBuilder::new(
            self.rollup_config.clone(),
            self.l1_chain_config.clone(),
            self.l2_derivation_provider(),
            self.l1_derivation_provider(),
            None,
        )
    }

    /// Builds the online (polled) derivation pipeline.
    async fn create_pipeline(&self) -> OnlinePipeline {
        OnlinePipeline::new_polled(
            self.rollup_config.clone(),
            self.l1_chain_config.clone(),
            OnlineBlobProvider::init(self.l1_beacon.clone()).await,
            self.l1_derivation_provider(),
            self.l2_derivation_provider(),
            None,
        )
    }

    /// Assembles the [`EngineActor`] around our [`WorldChainKonaEngineClient`].
    ///
    /// This mirrors `RollupNode::create_engine_actor`, but constructs the processors from the
    /// injected in-process client instead of building an [`kona_engine::OpEngineClient`] from an
    /// [`kona_node_service::EngineConfig`].
    #[allow(clippy::type_complexity)]
    fn create_engine_actor(
        &self,
        cancellation: CancellationToken,
        engine_request_rx: mpsc::Receiver<kona_node_service::EngineActorRequest>,
        derivation_client: QueuedEngineDerivationClient,
        unsafe_head_tx: watch::Sender<L2BlockInfo>,
    ) -> EngineActor<
        EngineProcessor<WorldChainKonaEngineClient, QueuedEngineDerivationClient>,
        EngineRpcProcessor<WorldChainKonaEngineClient>,
    > {
        let engine_state = EngineState::default();
        let (engine_state_tx, engine_state_rx) = watch::channel(engine_state);
        let (engine_queue_length_tx, engine_queue_length_rx) = watch::channel(0);
        let engine = Engine::new(engine_state, engine_state_tx, engine_queue_length_tx);

        let engine_processor = EngineProcessor::new(
            self.engine_client.clone(),
            self.rollup_config.clone(),
            derivation_client,
            engine,
            self.sequencer_mode.then_some(unsafe_head_tx),
        );

        let engine_rpc_processor = EngineRpcProcessor::new(
            self.engine_client.clone(),
            self.rollup_config.clone(),
            engine_state_rx,
            engine_queue_length_rx,
        );

        EngineActor::new(
            cancellation,
            engine_request_rx,
            engine_processor,
            engine_rpc_processor,
        )
    }

    /// Spawns the full Kona actor graph on the current tokio runtime and runs to completion.
    ///
    /// Returns when any actor errors (cancelling the rest) or when a shutdown signal is observed.
    /// The provided `cancellation` token is shared by all actors and is the same token the caller
    /// can use to trigger a graceful shutdown.
    pub async fn run(self, cancellation: CancellationToken) -> Result<(), String> {
        let (derivation_actor_request_tx, derivation_actor_request_rx) =
            mpsc::channel(CHANNEL_SIZE);
        let (engine_actor_request_tx, engine_actor_request_rx) = mpsc::channel(CHANNEL_SIZE);
        let (unsafe_head_tx, unsafe_head_rx) = watch::channel(L2BlockInfo::default());

        // Engine actor (in-process EL).
        let engine_actor = self.create_engine_actor(
            cancellation.clone(),
            engine_actor_request_rx,
            QueuedEngineDerivationClient::new(derivation_actor_request_tx.clone()),
            unsafe_head_tx,
        );

        // Derivation actor.
        let derivation = DerivationActor::<_, OnlinePipeline>::new(
            QueuedDerivationEngineClient {
                engine_actor_request_tx: engine_actor_request_tx.clone(),
            },
            cancellation.clone(),
            derivation_actor_request_rx,
            self.create_pipeline().await,
        );

        // Network (p2p) actor.
        let (
            NetworkInboundData {
                signer,
                p2p_rpc: network_rpc,
                gossip_payload_tx,
                admin_rpc: net_admin_rpc,
            },
            network,
        ) = NetworkActor::new(
            QueuedNetworkEngineClient {
                engine_actor_request_tx: engine_actor_request_tx.clone(),
            },
            cancellation.clone(),
            NetworkBuilder::from(self.p2p_config.clone()),
        );

        // L1 origin selection (sequencer only) + L1 watcher.
        let (l1_head_updates_tx, l1_head_updates_rx) = watch::channel(None);
        let delayed_l1_provider = DelayedL1OriginSelectorProvider::new(
            self.l1_provider.clone(),
            l1_head_updates_rx,
            self.sequencer_config.l1_conf_delay,
        );
        let delayed_origin_selector =
            L1OriginSelector::new(self.rollup_config.clone(), delayed_l1_provider);

        let conductor = self
            .sequencer_config
            .conductor_rpc_url
            .clone()
            .map(ConductorClient::new_http);

        let (l1_query_tx, l1_query_rx) = mpsc::channel(CHANNEL_SIZE);

        let head_stream = BlockStream::new_as_stream(
            self.l1_provider.clone(),
            alloy_eips::BlockNumberOrTag::Latest,
            Duration::from_secs(HEAD_STREAM_POLL_INTERVAL),
        )
        .map_err(|e| format!("failed to build L1 head stream: {e}"))?;
        let finalized_stream = BlockStream::new_as_stream(
            self.l1_provider.clone(),
            alloy_eips::BlockNumberOrTag::Finalized,
            Duration::from_secs(FINALIZED_STREAM_POLL_INTERVAL),
        )
        .map_err(|e| format!("failed to build L1 finalized stream: {e}"))?;

        let l1_watcher = L1WatcherActor::new(
            self.rollup_config.clone(),
            self.l1_provider.clone(),
            l1_query_rx,
            l1_head_updates_tx,
            QueuedL1WatcherDerivationClient {
                derivation_actor_request_tx,
            },
            signer,
            cancellation.clone(),
            head_stream,
            finalized_stream,
        );

        // Optional sequencer actor.
        let (sequencer_actor, sequencer_admin_client) = if self.sequencer_mode {
            let sequencer_engine_client = QueuedSequencerEngineClient {
                engine_actor_request_tx: engine_actor_request_tx.clone(),
                unsafe_head_rx,
            };
            let (sequencer_admin_api_tx, sequencer_admin_api_rx) = mpsc::channel(CHANNEL_SIZE);
            let queued_gossip_client =
                QueuedUnsafePayloadGossipClient::new(gossip_payload_tx.clone());

            (
                Some(SequencerActor {
                    admin_api_rx: sequencer_admin_api_rx,
                    attributes_builder: self.create_attributes_builder(),
                    cancellation_token: cancellation.clone(),
                    conductor,
                    engine_client: sequencer_engine_client,
                    is_active: self.sequencer_config.sequencer_stopped.not(),
                    in_recovery_mode: self.sequencer_config.sequencer_recovery_mode,
                    origin_selector: delayed_origin_selector,
                    rollup_config: self.rollup_config.clone(),
                    unsafe_payload_gossip_client: queued_gossip_client,
                }),
                Some(QueuedSequencerAdminAPIClient::new(sequencer_admin_api_tx)),
            )
        } else {
            (None, None)
        };

        // Optional RPC actor.
        let rpc = self.rpc_builder.clone().map(|b| {
            RpcActor::new(
                b,
                QueuedEngineRpcClient::new(engine_actor_request_tx.clone()),
                sequencer_admin_client,
            )
        });

        // Spawn all actors, cancelling the rest on first failure. This reimplements kona's
        // crate-private `spawn_and_wait!` macro and `shutdown_signal()` helper.
        let mut tasks: JoinSet<Result<(), String>> = JoinSet::new();

        macro_rules! spawn_actor {
            ($actor:expr, $ctx:expr) => {{
                let cancel = cancellation.clone();
                let actor = $actor;
                let ctx = $ctx;
                tasks.spawn(async move {
                    let _guard = cancel.drop_guard();
                    actor.start(ctx).await.map_err(|e| format!("{e:?}"))
                });
            }};
        }

        if let Some(rpc) = rpc {
            spawn_actor!(
                rpc,
                RpcContext {
                    cancellation: cancellation.clone(),
                    p2p_network: network_rpc,
                    network_admin: net_admin_rpc,
                    l1_watcher_queries: l1_query_tx,
                }
            );
        }
        if let Some(sequencer_actor) = sequencer_actor {
            spawn_actor!(sequencer_actor, ());
        }
        spawn_actor!(network, ());
        spawn_actor!(l1_watcher, ());
        spawn_actor!(derivation, ());
        spawn_actor!(engine_actor, ());

        let shutdown = shutdown_signal();
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    info!(target: "world_chain::kona", "Received shutdown signal, cancelling Kona actors");
                    cancellation.cancel();
                    return Ok(());
                }
                _ = cancellation.cancelled() => {
                    return Ok(());
                }
                result = tasks.join_next() => {
                    match result {
                        Some(Ok(Ok(()))) => {}
                        Some(Ok(Err(e))) => {
                            error!(target: "world_chain::kona", error = %e, "Kona actor failed");
                            cancellation.cancel();
                            return Err(e);
                        }
                        Some(Err(e)) => {
                            let msg = format!("Kona actor task join error: {e}");
                            error!(target: "world_chain::kona", error = %e, "Kona actor task join error");
                            cancellation.cancel();
                            return Err(msg);
                        }
                        None => return Ok(()),
                    }
                }
            }
        }
    }
}

/// Handle to a running Kona consensus node assembled in-process.
///
/// Owns the shared [`CancellationToken`] and the spawned tokio task. Designed to be held by the
/// node's add-ons and dropped on node exit.
#[derive(Debug)]
pub struct KonaServiceHandle {
    cancellation: CancellationToken,
    /// Wrapped in `Option` so [`stopped`](Self::stopped) can take ownership without conflicting
    /// with the `Drop` impl.
    task_handle: Option<tokio::task::JoinHandle<Result<(), String>>>,
}

impl KonaServiceHandle {
    /// Spawns the assembled [`KonaService`] on the current tokio runtime.
    pub fn spawn(service: KonaService) -> Self {
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let task_handle = tokio::spawn(async move { service.run(token).await });
        Self {
            cancellation,
            task_handle: Some(task_handle),
        }
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
                error!(target: "world_chain::kona", %join_error, "Kona service task panicked");
                Err(format!("Kona service task panicked: {join_error}"))
            }
        }
    }

    /// Initiates graceful shutdown of the Kona service.
    pub fn shutdown(&self) {
        warn!(target: "world_chain::kona", "Shutting down Kona consensus node");
        self.cancellation.cancel();
    }

    /// Returns a reference to the shared cancellation token.
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation
    }
}

impl Drop for KonaServiceHandle {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

/// Listens for OS shutdown signals (SIGTERM, SIGINT). Reimplements kona's crate-private
/// `service::shutdown_signal`.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!(target: "world_chain::kona", error = %e, "failed to install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                error!(target: "world_chain::kona", error = %e, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
