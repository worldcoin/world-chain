//! Manual assembly of the Kona rollup node actors around an in-process engine client.
//!
//! Canonical kona drives reth's execution engine over the authenticated Engine API (HTTP + JWT)
//! via [`kona_node_service::RollupNode::start`], which hard-wires its [`EngineActor`] to an
//! [`kona_engine::OpEngineClient`]. To run the consensus hot path **in-process**, we reproduce the
//! same actor graph here but inject an [`WorldChainKonaEngineClient`](crate::WorldChainKonaEngineClient) into
//! the [`EngineActor`]/[`EngineRpcActor`].
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

use alloy_primitives::{Address, B256, U256, b256};
use alloy_provider::{IpcConnect, Provider, RootProvider};
use alloy_rpc_client::ClientBuilder;
use futures::StreamExt as _;
use kona_derive::StatefulAttributesBuilder;
use kona_engine::{Engine, EngineClient, EngineState};
use kona_genesis::{L1ChainConfig, RollupConfig};
use kona_node_service::{
    BlockStream, ConductorClient, DelayedL1OriginSelectorProvider, DerivationActor, EngineActor,
    EngineRpcActor, JsonrpseeServerLauncher, L1OriginSelector, L1WatcherActor, NetworkActor,
    NetworkBuilder, NetworkConfig, NetworkHandler, NodeActor, QueuedDerivationEngineClient,
    QueuedEngineDerivationClient, QueuedEngineRpcClient, QueuedL1WatcherDerivationClient,
    QueuedNetworkEngineClient, QueuedSequencerAdminAPIClient, QueuedSequencerEngineClient,
    QueuedUnsafePayloadGossipClient, RpcActor, RpcServerLauncher, SequencerActor, SequencerConfig,
};
use kona_protocol::L2BlockInfo;
use kona_providers_alloy::{
    AlloyChainProvider, AlloyL2ChainProvider, OnlineBeaconClient, OnlineBlobProvider,
    OnlinePipeline,
};
use kona_registry::L1Config as RegisteredL1Config;
use kona_rpc::{
    AdminApiServer, AdminRpc, DevEngineApiServer, DevEngineRpc, HealthzApiServer, HealthzRpc,
    OpP2PApiServer, P2pRpc, RollupNodeApiServer, RollupRpc, RpcBuilder, WsRPC, WsServer,
};
use jsonrpsee::RpcModule;
use op_alloy_network::Optimism;
use std::ops::Not as _;
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use reth_engine_primitives::ConsensusEngineHandle;
use reth_optimism_node::OpEngineTypes;
use reth_payload_builder::PayloadStore;
use url::Url;

use crate::{FlashblocksAuthorizationNotifier, KonaConfig, WorldChainKonaEngineClient};

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

const DERIVATION_PROVIDER_CACHE_SIZE: usize = 1024;
const HEAD_STREAM_POLL_INTERVAL: u64 = 4;
const FINALIZED_STREAM_POLL_INTERVAL: u64 = 60;
const CHANNEL_SIZE: usize = 1024;

fn load_registered_l1_chain_config(l1_chain_id: u64) -> Arc<L1ChainConfig> {
    match RegisteredL1Config::get_l1_genesis(l1_chain_id) {
        Ok(config) => {
            info!(
                target: "world_chain::kona",
                l1_chain_id,
                "Loaded registered L1 chain config for in-process Kona"
            );
            Arc::new(config.into())
        }
        Err(error) => {
            warn!(
                target: "world_chain::kona",
                l1_chain_id,
                %error,
                "failed to load registered L1 chain config; falling back to default"
            );
            Arc::new(L1ChainConfig::default())
        }
    }
}

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
        flashblocks_authorizer: Option<FlashblocksAuthorizationNotifier>,
    ) -> eyre::Result<Self> {
        let l1_provider = RootProvider::new_http(config.l1_rpc_url.clone());
        let l1_chain_id: u64 = config.rollup_config.l1_chain_id;
        let chain_config = load_registered_l1_chain_config(l1_chain_id);
        let l2_client = match l2_endpoint {
            L2RpcEndpoint::Ipc(path) => ClientBuilder::default().ipc(IpcConnect::new(path)).await?,
            L2RpcEndpoint::Http(url) => ClientBuilder::default().http(url),
        };
        let l2_provider = RootProvider::<Optimism>::new(l2_client);

        let engine_client = Arc::new(WorldChainKonaEngineClient::new(
            config.rollup_config.clone(),
            engine_handle,
            payload_store,
            l2_provider.clone(),
            l1_provider.clone(),
            flashblocks_authorizer,
        ));

        let l2_chain_id: u64 = config.rollup_config.l2_chain_id.into();
        let mut p2p_config = config
            .p2p
            .clone()
            .build_network_config(&config.rollup_config, l2_chain_id)?;

        // The unsafe-block signer (the address P2P gossip validates each block's signature against)
        // lives in the L1 `SystemConfig` contract. Canonical kona seeds it from L1 at startup and the
        // L1 watcher keeps it updated via `SystemConfigUpdate` events. Our P2P arg builder only honors
        // an explicit `--p2p.unsafe.block.signer` override and otherwise leaves it zero, which makes
        // gossip reject every block with `Signer { expected: 0x0, .. }`. Seed it from L1 here when not
        // overridden (the L1 watcher still applies any later on-chain changes).
        if p2p_config.unsafe_block_signer.is_zero() {
            match fetch_unsafe_block_signer(
                &l1_provider,
                config.rollup_config.l1_system_config_address,
            )
            .await
            {
                Ok(signer) => {
                    info!(target: "world_chain::kona", %signer, "Loaded unsafe block signer from L1 system config");
                    p2p_config.unsafe_block_signer = signer;
                }
                Err(e) => {
                    warn!(target: "world_chain::kona", error = %e, "failed to fetch unsafe block signer from L1; P2P gossip will reject blocks until a SystemConfig update is observed");
                }
            }
        }

        let mut l1_beacon = OnlineBeaconClient::new_http(config.l1_beacon_url.to_string());
        if let Some(slot) = config.l1_slot_duration_override {
            l1_beacon = l1_beacon.with_l1_slot_duration_override(slot);
        }

        Ok(Self {
            rollup_config: config.rollup_config.clone(),
            l1_chain_config: chain_config,
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

    /// Assembles the [`EngineActor`] and [`EngineRpcActor`] around our
    /// [`WorldChainKonaEngineClient`].
    ///
    /// This mirrors `RollupNode::build_engine_actors`, but injects the in-process client instead of
    /// building an [`kona_engine::OpEngineClient`] from an [`kona_node_service::EngineConfig`].
    ///
    /// When `el_sync_finished` is `true`, the engine starts with EL sync already considered
    /// complete. This is used when reth's execution layer already holds a chain (a restart from an
    /// existing/snapshotted database): rather than waiting for a gossip-driven forkchoice update to
    /// confirm EL sync — which never arrives if no peer is actively gossiping unsafe blocks — the
    /// engine immediately performs its initial reset (reading the EL head and issuing the bootstrap
    /// forkchoice update) so derivation can proceed from L1. This mirrors Base's follower, which
    /// seeds its engine from the local EL head on startup.
    #[allow(clippy::type_complexity)]
    fn create_engine_actors(
        &self,
        engine_request_rx: mpsc::Receiver<kona_node_service::EngineActorRequest>,
        engine_rpc_request_rx: mpsc::Receiver<kona_node_service::EngineRpcRequest>,
        derivation_client: QueuedEngineDerivationClient,
        unsafe_head_tx: watch::Sender<L2BlockInfo>,
        el_sync_finished: bool,
    ) -> (
        EngineActor<WorldChainKonaEngineClient, QueuedEngineDerivationClient>,
        EngineRpcActor<WorldChainKonaEngineClient>,
        watch::Receiver<EngineState>,
    ) {
        let engine_state = EngineState {
            el_sync_finished,
            ..Default::default()
        };
        let (engine_state_tx, engine_state_rx) = watch::channel(engine_state);
        let (engine_queue_length_tx, engine_queue_length_rx) = watch::channel(0);
        let engine = Engine::new(engine_state, engine_state_tx, engine_queue_length_tx);

        // The unsafe-head feed is only meaningful in sequencer mode; validators ignore it.
        let unsafe_head_tx_opt = self.sequencer_mode.then_some(unsafe_head_tx);

        let engine_actor = EngineActor::new(
            self.engine_client.clone(),
            self.rollup_config.clone(),
            derivation_client,
            engine,
            unsafe_head_tx_opt,
            engine_request_rx,
        );

        // A second receiver on the engine state, handed back to the caller to gate the L1
        // finality feed on the engine's safe head (see the finalized-stream wiring in `run`).
        let engine_state_rx_for_gate = engine_state_rx.clone();

        let engine_rpc_actor = EngineRpcActor::new(
            self.engine_client.clone(),
            self.rollup_config.clone(),
            engine_state_rx,
            engine_queue_length_rx,
            engine_rpc_request_rx,
        );

        (engine_actor, engine_rpc_actor, engine_state_rx_for_gate)
    }

    /// Spawns the full Kona actor graph on the current tokio runtime and runs to completion.
    ///
    /// Returns when any actor errors (cancelling the rest) or when a shutdown signal is observed.
    /// The provided `cancellation` token is shared by all actors and is the same token the caller
    /// can use to trigger a graceful shutdown.
    pub async fn run(self, cancellation: CancellationToken) -> Result<(), String> {
        // Cross-actor channels. The network actor's inbound channels (signer, p2p RPC, network
        // admin, gossip payload) were previously surfaced via `NetworkInboundData`; upstream now
        // requires the caller to own them, so we create them here.
        let (derivation_actor_request_tx, derivation_actor_request_rx) =
            mpsc::channel(CHANNEL_SIZE);
        let (engine_actor_request_tx, engine_actor_request_rx) = mpsc::channel(CHANNEL_SIZE);
        let (engine_rpc_request_tx, engine_rpc_request_rx) = mpsc::channel(CHANNEL_SIZE);
        let (l1_query_tx, l1_query_rx) = mpsc::channel(CHANNEL_SIZE);
        let (sequencer_admin_api_tx, sequencer_admin_api_rx) = mpsc::channel(CHANNEL_SIZE);
        let (signer_tx, signer_rx) = mpsc::channel(CHANNEL_SIZE);
        let (p2p_rpc_tx, p2p_rpc_rx) = mpsc::channel(CHANNEL_SIZE);
        let (network_admin_tx, network_admin_rx) = mpsc::channel(CHANNEL_SIZE);
        let (gossip_payload_tx, gossip_payload_rx) = mpsc::channel(CHANNEL_SIZE);
        let (unsafe_head_tx, unsafe_head_rx) = watch::channel(L2BlockInfo::default());
        let (l1_head_updates_tx, l1_head_updates_rx) = watch::channel(None);

        // Determine whether reth's EL already holds a chain beyond genesis. If so, EL sync needs no
        // gossip-driven snap sync to bootstrap: mark EL sync complete up front so the engine
        // performs its initial reset (reading the EL head + forkchoice) and derivation can proceed
        // from L1 immediately. Without this, a verifier whose peers are not gossiping unsafe blocks
        // (e.g. a devnet replaying history via derivation) deadlocks in `AwaitingELSyncCompletion`.
        let el_sync_finished = match self
            .engine_client
            .l2_block_info_by_label(alloy_eips::BlockNumberOrTag::Latest)
            .await
        {
            Ok(Some(head)) if head.block_info.number > self.rollup_config.genesis.l2.number => {
                info!(
                    target: "world_chain::kona",
                    el_head = head.block_info.number,
                    "reth EL already holds a chain; marking EL sync complete to bootstrap derivation from L1 without waiting for unsafe-block gossip"
                );
                true
            }
            Ok(_) => false,
            Err(e) => {
                warn!(
                    target: "world_chain::kona",
                    error = %e,
                    "failed to read reth EL head at startup; falling back to gossip-driven EL sync"
                );
                false
            }
        };

        // Engine actors (in-process EL). The returned receiver tracks the engine's safe head and
        // is used below to gate the L1 finality feed.
        let (engine_actor, engine_rpc_actor, engine_safe_head_rx) = self.create_engine_actors(
            engine_actor_request_rx,
            engine_rpc_request_rx,
            QueuedEngineDerivationClient::new(derivation_actor_request_tx.clone()),
            unsafe_head_tx,
            el_sync_finished,
        );

        // Derivation actor.
        let derivation = DerivationActor::<_, OnlinePipeline>::new(
            QueuedDerivationEngineClient {
                engine_actor_request_tx: engine_actor_request_tx.clone(),
            },
            derivation_actor_request_rx,
            self.create_pipeline().await,
        );

        // Network (p2p) actor. The libp2p swarm is built and started first so the constructor
        // stays synchronous, mirroring upstream `RollupNode::start`.
        let network_handler: NetworkHandler = NetworkBuilder::from(self.p2p_config.clone())
            .build()
            .map_err(|e| format!("failed to build network: {e:?}"))?
            .start()
            .await
            .map_err(|e| format!("failed to start network: {e:?}"))?;
        let network = NetworkActor::new(
            QueuedNetworkEngineClient {
                engine_actor_request_tx: engine_actor_request_tx.clone(),
            },
            network_handler,
            signer_rx,
            p2p_rpc_rx,
            network_admin_rx,
            gossip_payload_rx,
        );

        // L1 origin selection (sequencer only) + L1 watcher.
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

        // Both streams are boxed to a single concrete type because `L1WatcherActor` is generic
        // over one `BlockStream` type shared by its head and finalized streams.
        let head_stream = BlockStream::new_as_stream(
            self.l1_provider.clone(),
            alloy_eips::BlockNumberOrTag::Latest,
            Duration::from_secs(HEAD_STREAM_POLL_INTERVAL),
        )
        .map_err(|e| format!("failed to build L1 head stream: {e}"))?
        .boxed();

        // Clamp the L1 finality feed to the engine's safe head.
        //
        // kona's `FinalizeTask` rejects a finalize target above the current safe head with a
        // `Critical` error, which tears down the in-process engine — and, since the engine is
        // reth's only driver, the whole node. That target is the highest L2 block derived from a
        // finalized L1 block, so we clamp each finalized L1 block to strictly below the safe head's
        // L1 origin. Every L2 block whose L1 origin is `<=` the clamped value then lies in an epoch
        // before the safe head's, so it is already safe and the derived target can never exceed the
        // safe head. (Strictly below, not at: consecutive L2 blocks usually share an L1 origin, so
        // the safe head's own epoch may still contain an as-yet-unsafe block.)
        //
        // Only `BlockInfo::number` is consumed downstream — the L1 watcher forwards the block as-is
        // and kona's finalizer keys solely off the number — so clamping the number suffices. In a
        // synced node the finalized L1 block already trails the safe head's origin by far more than
        // this, so the clamp is a no-op; it only engages while the node replays history behind the
        // finalized point (e.g. a restart bootstrapping derivation from L1 with no unsafe-block
        // gossip), which is exactly the window where the unclamped feed crashes the node.
        let finalized_stream = BlockStream::new_as_stream(
            self.l1_provider.clone(),
            alloy_eips::BlockNumberOrTag::Finalized,
            Duration::from_secs(FINALIZED_STREAM_POLL_INTERVAL),
        )
        .map_err(|e| format!("failed to build L1 finalized stream: {e}"))?
        .map(move |mut finalized| {
            let safe_origin = engine_safe_head_rx
                .borrow()
                .sync_state
                .safe_head()
                .l1_origin
                .number;
            finalized.number = finalized.number.clamp(0, safe_origin.saturating_sub(1));
            finalized
        })
        .boxed();

        let l1_watcher = L1WatcherActor::new(
            self.rollup_config.clone(),
            self.l1_provider.clone(),
            l1_query_rx,
            l1_head_updates_tx,
            QueuedL1WatcherDerivationClient {
                derivation_actor_request_tx,
            },
            signer_tx,
            head_stream,
            finalized_stream,
        );

        // Optional sequencer actor.
        let sequencer_actor = if self.sequencer_mode {
            let sequencer_engine_client = QueuedSequencerEngineClient {
                engine_actor_request_tx: engine_actor_request_tx.clone(),
                unsafe_head_rx,
            };
            let queued_gossip_client = QueuedUnsafePayloadGossipClient::new(gossip_payload_tx);

            Some(SequencerActor::new(
                sequencer_admin_api_rx,
                self.create_attributes_builder(),
                conductor,
                sequencer_engine_client,
                self.sequencer_config.sequencer_stopped.not(),
                self.sequencer_config.sequencer_recovery_mode,
                delayed_origin_selector,
                self.rollup_config.clone(),
                queued_gossip_client,
            ))
        } else {
            None
        };
        let sequencer_admin_client = sequencer_actor
            .is_some()
            .then(|| QueuedSequencerAdminAPIClient::new(sequencer_admin_api_tx));

        // Optional RPC actor. Upstream `RollupNode::build_rpc_actor` now assembles the JSON-RPC
        // module set, performs the initial server launch, and hands the live handle to the actor;
        // the actor is no longer driven by an `RpcContext`.
        let rpc = if let Some(config) = self.rpc_builder.clone() {
            let engine_rpc_client = QueuedEngineRpcClient::new(engine_rpc_request_tx);
            let mut modules = RpcModule::new(());
            modules
                .merge(HealthzApiServer::into_rpc(HealthzRpc {}))
                .map_err(|e| format!("failed to register healthz module: {e:?}"))?;
            modules
                .merge(P2pRpc::new(p2p_rpc_tx).into_rpc())
                .map_err(|e| format!("failed to register p2p module: {e:?}"))?;
            modules
                .merge(AdminRpc::new(sequencer_admin_client, network_admin_tx).into_rpc())
                .map_err(|e| format!("failed to register admin module: {e:?}"))?;
            modules
                .merge(RollupRpc::new(engine_rpc_client.clone(), l1_query_tx).into_rpc())
                .map_err(|e| format!("failed to register rollup module: {e:?}"))?;
            if config.dev_enabled() {
                modules
                    .merge(DevEngineRpc::new(engine_rpc_client.clone()).into_rpc())
                    .map_err(|e| format!("failed to register dev engine module: {e:?}"))?;
            }
            if config.ws_enabled() {
                modules
                    .merge(WsRPC::new(engine_rpc_client.clone()).into_rpc())
                    .map_err(|e| format!("failed to register ws module: {e:?}"))?;
            }

            let restarts_remaining = config.restart_count();
            let launcher = JsonrpseeServerLauncher::new(config);
            let handle = launcher
                .launch(modules.clone())
                .await
                .map_err(|e: std::io::Error| format!("failed to launch rpc server: {e:?}"))?;
            Some(RpcActor::new(launcher, modules, handle, restarts_remaining))
        } else {
            None
        };

        // Spawn all actors, cancelling the rest on first failure. This reimplements kona's
        // crate-private `spawn_and_wait!` macro and `shutdown_signal()` helper: each actor's
        // `step` is driven in a loop until it errors or the shared cancellation token fires.
        let mut tasks: JoinSet<Result<(), String>> = JoinSet::new();

        macro_rules! spawn_actor {
            ($actor:expr) => {{
                if let Some(mut actor) = $actor {
                    let cancel = cancellation.clone();
                    tasks.spawn(async move {
                        let _guard = cancel.clone().drop_guard();
                        loop {
                            tokio::select! {
                                biased;
                                _ = cancel.cancelled() => return Ok(()),
                                result = actor.step() => {
                                    result.map_err(|e| format!("{e:?}"))?;
                                }
                            }
                        }
                    });
                }
            }};
        }

        spawn_actor!(rpc);
        spawn_actor!(sequencer_actor);
        spawn_actor!(Some(network));
        spawn_actor!(Some(l1_watcher));
        spawn_actor!(Some(derivation));
        spawn_actor!(Some(engine_actor));
        spawn_actor!(Some(engine_rpc_actor));

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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_eips::eip7840::BlobParams;

    const SEPOLIA_L1_CHAIN_ID: u64 = 11_155_111;
    const SEPOLIA_BPO2_TIMESTAMP: u64 = 1_761_607_008;

    #[test]
    fn registered_l1_config_uses_sepolia_blob_schedule() {
        let chain_config = load_registered_l1_chain_config(SEPOLIA_L1_CHAIN_ID);

        assert_eq!(chain_config.bpo2_time, Some(SEPOLIA_BPO2_TIMESTAMP));
        assert_ne!(chain_config.bpo2_time, L1ChainConfig::default().bpo2_time);

        let blob_schedule = chain_config.blob_schedule_blob_params();
        assert_eq!(
            blob_schedule.active_scheduled_params_at_timestamp(SEPOLIA_BPO2_TIMESTAMP),
            Some(&BlobParams::bpo2())
        );
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

/// Reads the unsafe-block signer address from the L1 `SystemConfig` contract.
///
/// Mirrors canonical kona's startup fetch (`bin/node`'s `unsafe_block_signer`): the address is held
/// at storage slot `bytes32(uint256(keccak256("systemconfig.unsafeblocksigner")) - 1)` and read at
/// the latest L1 block. The low 20 bytes of the slot are the signer address.
async fn fetch_unsafe_block_signer(
    l1_provider: &RootProvider,
    system_config_address: Address,
) -> eyre::Result<Address> {
    /// `bytes32(uint256(keccak256("systemconfig.unsafeblocksigner")) - 1)`.
    const UNSAFE_BLOCK_SIGNER_SLOT: B256 =
        b256!("0x65a7ed542fb37fe237fdfbdd70b31598523fe5b32879e307bae27a0bd9581c08");

    let value = l1_provider
        .get_storage_at(
            system_config_address,
            U256::from_be_bytes(UNSAFE_BLOCK_SIGNER_SLOT.0),
        )
        .await?;
    Ok(Address::from_slice(&value.to_be_bytes::<32>()[12..]))
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
