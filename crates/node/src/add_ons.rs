//! World Chain node add-ons.

use core::marker::PhantomData;
use std::time::{Duration, Instant};

use reth_engine_primitives::ConsensusEngineHandle;
use reth_payload_builder::{PayloadBuilderHandle, PayloadStore};
use reth_tasks::TaskExecutor;

use alloy_consensus::{Block, BlockBody, Header};
use alloy_primitives::Sealed;
use op_alloy_consensus::{OpTransaction, TxPostExec};
use reth_chainspec::ChainSpecProvider;
use reth_evm::{ConfigureEvm, EvmFactory, block::BlockExecutorFactory};
use reth_node_api::{BuildNextEnv, FullNodeComponents, NodeAddOns, NodeTypes, PrimitivesTy};
use reth_node_builder::rpc::{
    BasicEngineValidatorBuilder, EngineApiBuilder, EngineValidatorAddOn, EngineValidatorBuilder,
    EthApiBuilder, Identity, PayloadValidatorBuilder, RethRpcAddOns, RethRpcMiddleware,
    RethRpcServerHandles, RpcAddOns, RpcContext, RpcHandle,
};
use reth_optimism_chainspec::OpHardfork;
use reth_optimism_evm::ConfigurePostExecEvm;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{OpEngineApiBuilder, OpEngineTypes, txpool::OpPooledTx};
use reth_optimism_payload_builder::{
    OpPayloadBuilderAttributes, OpPayloadPrimitives,
    config::{OpDAConfig, OpGasLimitConfig},
};
use reth_optimism_primitives::{OpReceipt, OpTransactionSigned};
use reth_optimism_rpc::{
    SequencerClient as OpSequencerClient,
    eth::ext::OpEthExtApi,
    historical::{HistoricalRpc, HistoricalRpcClient},
    miner::{MinerApiExtServer, OpMinerExtApi},
    witness::{DebugExecutionWitnessApiServer, OpDebugWitnessApi},
};
use reth_primitives_traits::{FullSignedTx, NodePrimitives};
use reth_provider::{BlockReaderIdExt, HeaderProvider, StateProviderFactory};
use reth_rpc_api::{
    DebugApiServer, EthConfigApiServer, L2EthApiExtServer, eth::helpers::config::EthConfigHandler,
};
use reth_rpc_server_types::RethRpcModule;
use reth_transaction_pool::TransactionPool;
use tracing::{debug, error, info};
use world_chain_chainspec::WorldChainSpec;
use world_chain_cli::KonaArgs;
use world_chain_evm::OpTx;
use world_chain_kona::{
    FlashblocksAuthorizationNotifier, KonaConfig, KonaService, KonaServiceHandle, L2RpcEndpoint,
};

use crate::context::build_kona_config;
use world_chain_rpc::{
    EthApiExtServer, SequencerClient as WorldChainSequencerClient, Simulate, SimulateApiServer,
    WorldChainEthApiExt,
    op::{FlashblocksOpApi, OpApiExtServer},
};

/// Primitive bounds required by the OP RPC extensions used by World Chain.
pub trait WorldChainRpcPrimitives<Tx>:
    OpPayloadPrimitives<_Header = Header, _TX = Tx>
    + NodePrimitives<
        Receipt = OpReceipt,
        SignedTx = Tx,
        BlockHeader = Header,
        BlockBody = BlockBody<Tx>,
        Block = Block<Tx>,
    >
where
    Tx: FullSignedTx + OpTransaction,
{
}

impl<T, Tx> WorldChainRpcPrimitives<Tx> for T
where
    Tx: FullSignedTx + OpTransaction,
    T: OpPayloadPrimitives<_Header = Header, _TX = Tx>
        + NodePrimitives<
            Receipt = OpReceipt,
            SignedTx = Tx,
            BlockHeader = Header,
            BlockBody = BlockBody<Tx>,
            Block = Block<Tx>,
        >,
{
}

/// Add-ons w.r.t. World Chain.
///
/// This mirrors OP-Reth's add-ons while installing World Chain RPC extensions during add-on
/// launch.
#[derive(Debug)]
pub struct WorldChainAddOns<
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
    PVB,
    EB = OpEngineApiBuilder<PVB>,
    EVB = BasicEngineValidatorBuilder<PVB>,
    RpcMiddleware = Identity,
    Tx = OpTransactionSigned,
> {
    /// Rpc add-ons responsible for launching the RPC servers and instantiating the RPC handlers
    /// and eth-api.
    pub rpc_add_ons: RpcAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>,
    /// Data availability configuration for the OP builder.
    pub da_config: OpDAConfig,
    /// Gas limit configuration for the OP builder.
    pub gas_limit_config: OpGasLimitConfig,
    /// Sequencer client, configured to forward submitted transactions to sequencer of given OP
    /// network.
    pub sequencer_url: Option<String>,
    /// Headers to use for the sequencer client requests.
    pub sequencer_headers: Vec<String>,
    /// RPC endpoint for historical data.
    ///
    /// This can be used to forward pre-bedrock rpc requests (op-mainnet).
    pub historical_rpc: Option<String>,
    /// Enable transaction conditionals.
    enable_tx_conditional: bool,
    /// Minimum suggested priority fee (tip).
    min_suggested_priority_fee: u64,
    /// Enables the World Chain simulate namespace.
    simulate_enabled: bool,
    /// In-process Kona consensus startup intent (the enabled `--kona.*` CLI args).
    ///
    /// When [`Some`], [`launch_add_ons`](NodeAddOns::launch_add_ons) builds the [`KonaConfig`] from
    /// these args (failing the launch if the rollup config is missing/unreadable/unparsable),
    /// assembles a [`WorldChainKonaEngineClient`] from reth's engine handle, and spawns the Kona
    /// consensus node in-process. The build is deferred to launch so misconfiguration aborts node
    /// startup rather than being silently swallowed.
    ///
    /// [`KonaConfig`]: world_chain_kona::KonaConfig
    /// [`WorldChainKonaEngineClient`]: world_chain_kona::WorldChainKonaEngineClient
    kona_args: Option<KonaArgs>,
    /// Flashblocks payload-job authorizer, plumbed into the in-process Kona engine client so a
    /// forkchoice update with attributes notifies (and optionally authorizes) the generator. See
    /// [`FlashblocksAuthorizationNotifier`]; [`None`] when Flashblocks is disabled.
    flashblocks_authorizer: Option<FlashblocksAuthorizationNotifier>,
    /// Transaction type carried by the node primitives.
    _tx: PhantomData<fn() -> Tx>,
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware, Tx>
    WorldChainAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware, Tx>
where
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
{
    /// Creates a new instance from components.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        rpc_add_ons: RpcAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>,
        da_config: OpDAConfig,
        gas_limit_config: OpGasLimitConfig,
        sequencer_url: Option<String>,
        sequencer_headers: Vec<String>,
        historical_rpc: Option<String>,
        enable_tx_conditional: bool,
        min_suggested_priority_fee: u64,
        simulate_enabled: bool,
    ) -> Self {
        Self {
            rpc_add_ons,
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            enable_tx_conditional,
            min_suggested_priority_fee,
            simulate_enabled,
            kona_args: None,
            flashblocks_authorizer: None,
            _tx: PhantomData,
        }
    }
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware, Tx>
    WorldChainAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware, Tx>
where
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
{
    /// Sets the enabled `--kona.*` CLI args which signal the add-ons to build a
    /// [`KonaConfig`](world_chain_kona::KonaConfig) and spawn an in-process Consensus Engine during
    /// launch.
    ///
    /// Passing [`Some`] defers the fallible config build to
    /// [`launch_add_ons`](NodeAddOns::launch_add_ons), so a misconfigured-but-enabled Kona aborts
    /// node startup instead of silently disabling consensus.
    pub fn with_kona_args(mut self, kona_args: Option<KonaArgs>) -> Self {
        self.kona_args = kona_args;
        self
    }

    /// Sets the Flashblocks payload-job authorizer plumbed into the in-process Kona engine client,
    /// so a forkchoice update with attributes notifies (and optionally authorizes) the generator.
    pub fn with_flashblocks_authorizer(
        mut self,
        flashblocks_authorizer: Option<FlashblocksAuthorizationNotifier>,
    ) -> Self {
        self.flashblocks_authorizer = flashblocks_authorizer;
        self
    }

    /// Maps the [`EngineApiBuilder`] builder type.
    pub fn with_engine_api<T>(
        self,
        engine_api_builder: T,
    ) -> WorldChainAddOns<N, EthB, PVB, T, EVB, RpcMiddleware, Tx> {
        let Self {
            rpc_add_ons,
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            enable_tx_conditional,
            min_suggested_priority_fee,
            simulate_enabled,
            ..
        } = self;
        WorldChainAddOns::new(
            rpc_add_ons.with_engine_api(engine_api_builder),
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            enable_tx_conditional,
            min_suggested_priority_fee,
            simulate_enabled,
        )
    }

    /// Maps the [`PayloadValidatorBuilder`] builder type.
    pub fn with_payload_validator<T>(
        self,
        payload_validator_builder: T,
    ) -> WorldChainAddOns<N, EthB, T, EB, EVB, RpcMiddleware, Tx> {
        let Self {
            rpc_add_ons,
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            enable_tx_conditional,
            min_suggested_priority_fee,
            simulate_enabled,
            ..
        } = self;
        WorldChainAddOns::new(
            rpc_add_ons.with_payload_validator(payload_validator_builder),
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            enable_tx_conditional,
            min_suggested_priority_fee,
            simulate_enabled,
        )
    }

    /// Maps the [`EngineValidatorBuilder`] builder type.
    pub fn with_engine_validator<T>(
        self,
        engine_validator_builder: T,
    ) -> WorldChainAddOns<N, EthB, PVB, EB, T, RpcMiddleware, Tx> {
        let Self {
            rpc_add_ons,
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            enable_tx_conditional,
            min_suggested_priority_fee,
            simulate_enabled,
            ..
        } = self;
        WorldChainAddOns::new(
            rpc_add_ons.with_engine_validator(engine_validator_builder),
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            enable_tx_conditional,
            min_suggested_priority_fee,
            simulate_enabled,
        )
    }

    /// Sets the RPC middleware stack for processing RPC requests.
    pub fn with_rpc_middleware<T>(
        self,
        rpc_middleware: T,
    ) -> WorldChainAddOns<N, EthB, PVB, EB, EVB, T, Tx> {
        let Self {
            rpc_add_ons,
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            enable_tx_conditional,
            min_suggested_priority_fee,
            simulate_enabled,
            ..
        } = self;
        WorldChainAddOns::new(
            rpc_add_ons.with_rpc_middleware(rpc_middleware),
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            enable_tx_conditional,
            min_suggested_priority_fee,
            simulate_enabled,
        )
    }

    /// Sets the hook that is run once the rpc server is started.
    pub fn on_rpc_started<F>(mut self, hook: F) -> Self
    where
        F: FnOnce(RpcContext<'_, N, EthB::EthApi>, RethRpcServerHandles) -> eyre::Result<()>
            + Send
            + 'static,
    {
        self.rpc_add_ons = self.rpc_add_ons.on_rpc_started(hook);
        self
    }

    /// Sets the hook that is run to configure the rpc modules.
    pub fn extend_rpc_modules<F>(mut self, hook: F) -> Self
    where
        F: FnOnce(RpcContext<'_, N, EthB::EthApi>) -> eyre::Result<()> + Send + 'static,
    {
        self.rpc_add_ons = self.rpc_add_ons.extend_rpc_modules(hook);
        self
    }
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware, Tx> NodeAddOns<N>
    for WorldChainAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware, Tx>
where
    N: FullNodeComponents<
            Types: NodeTypes<ChainSpec = WorldChainSpec, Payload = OpEngineTypes>,
            Evm: ConfigurePostExecEvm<
                NextBlockEnvCtx: BuildNextEnv<
                    OpPayloadBuilderAttributes<Tx>,
                    Header,
                    WorldChainSpec,
                >,
            >,
            Pool: TransactionPool<Transaction: OpPooledTx<Consensus = Tx>>,
        >,
    PrimitivesTy<N::Types>: WorldChainRpcPrimitives<Tx>,
    N::Provider: BlockReaderIdExt
        + ChainSpecProvider<ChainSpec = WorldChainSpec>
        + HeaderProvider<Header = Header>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
    EthB: EthApiBuilder<N>,
    PVB: Send,
    EB: EngineApiBuilder<N>,
    EVB: EngineValidatorBuilder<N>,
    RpcMiddleware: RethRpcMiddleware,
    Tx: FullSignedTx + OpTransaction + From<Sealed<TxPostExec>>,
    <<N::Evm as ConfigureEvm>::BlockExecutorFactory as BlockExecutorFactory>::EvmFactory:
        EvmFactory<Tx = OpTx>,
{
    type Handle = RpcHandle<N, EthB::EthApi>;

    async fn launch_add_ons(
        self,
        ctx: reth_node_api::AddOnsContext<'_, N>,
    ) -> eyre::Result<Self::Handle> {
        let Self {
            rpc_add_ons,
            da_config,
            gas_limit_config,
            sequencer_url,
            sequencer_headers,
            enable_tx_conditional,
            historical_rpc,
            simulate_enabled,
            kona_args,
            flashblocks_authorizer,
            ..
        } = self;

        // Capture the inputs the in-process Kona consensus node needs from `ctx` *before*
        // `launch_add_ons_with` consumes it. The authoritative L2 IPC endpoint is read from the
        // live RPC server handle after launch (below), so we only stash the engine-layer handles
        // here. We also fail fast if the IPC server is disabled, since Kona connects over it.
        //
        // The [`KonaConfig`](world_chain_kona::KonaConfig) is built here (not in `add_ons`, which
        // cannot return errors) so that an enabled-but-misconfigured Kona — a missing, unreadable,
        // or unparsable rollup config — aborts node startup via `?` rather than silently starting
        // without a consensus engine.
        let kona_inputs = kona_args
            .map(|kona_args| -> eyre::Result<_> {
                let kona_config = build_kona_config(&kona_args)?;
                let engine_handle = ctx.beacon_engine_handle.clone();
                // Carry the (cloneable) payload builder handle rather than a `PayloadStore`: the
                // Kona supervisor rebuilds a fresh `PayloadStore` on each (re)start, and
                // `PayloadStore` is not itself `Clone`.
                let payload_builder_handle = ctx.node.payload_builder_handle().clone();
                let task_executor = ctx.node.task_executor().clone();
                Ok((
                    kona_config,
                    engine_handle,
                    payload_builder_handle,
                    task_executor,
                    flashblocks_authorizer,
                ))
            })
            .transpose()?;

        let eth_config =
            EthConfigHandler::new(ctx.node.provider().clone(), ctx.node.evm_config().clone());

        let maybe_pre_bedrock_historical_rpc = historical_rpc.zip(ctx.node
                     .provider()
                     .chain_spec()
                     .op_fork_activation(OpHardfork::Bedrock)
                     .block_number()
                     .filter(|activation| *activation > 0))
            .map(|(historical_rpc, bedrock_block)| -> eyre::Result<_> {
                info!(target: "reth::cli", %bedrock_block, ?historical_rpc, "Using historical RPC endpoint pre bedrock");
                let provider = ctx.node.provider().clone();
                let client = HistoricalRpcClient::new(&historical_rpc)?;
                let layer = HistoricalRpc::new(provider, client, bedrock_block);
                Ok(layer)
            })
            .transpose()?;

        let rpc_add_ons = rpc_add_ons.option_layer_rpc_middleware(maybe_pre_bedrock_historical_rpc);

        let evm_config = ctx.node.evm_config().clone();
        let builder = reth_optimism_payload_builder::OpPayloadBuilder::new(
            ctx.node.pool().clone(),
            ctx.node.provider().clone(),
            evm_config.clone(),
        );
        let debug_ext = OpDebugWitnessApi::<_, _, _, OpPayloadBuilderAttributes<Tx>>::new(
            ctx.node.provider().clone(),
            ctx.node.task_executor().clone(),
            builder,
            evm_config.clone(),
        );
        let miner_ext = OpMinerExtApi::new(da_config, gas_limit_config);

        let world_chain_sequencer_url = sequencer_url.clone();
        let sequencer_client = if let Some(url) = sequencer_url {
            Some(OpSequencerClient::new_with_headers(url, sequencer_headers).await?)
        } else {
            None
        };

        let tx_conditional_ext: OpEthExtApi<N::Pool, N::Provider> = OpEthExtApi::new(
            sequencer_client,
            ctx.node.pool().clone(),
            ctx.node.provider().clone(),
        );

        let world_chain_eth_ext = WorldChainEthApiExt::new(
            ctx.node.pool().clone(),
            ctx.node.provider().clone(),
            world_chain_sequencer_url.map(WorldChainSequencerClient::new),
        );
        let flashblocks_op_api = FlashblocksOpApi;
        let provider = ctx.node.provider().clone();

        let handle = rpc_add_ons
            .launch_add_ons_with(ctx, move |container| {
                let reth_node_builder::rpc::RpcModuleContainer {
                    modules,
                    auth_module,
                    registry,
                } = container;

                modules.merge_if_module_configured(RethRpcModule::Eth, eth_config.into_rpc())?;

                debug!(target: "reth::cli", "Installing debug payload witness rpc endpoint");
                modules.merge_if_module_configured(RethRpcModule::Debug, debug_ext.into_rpc())?;

                modules.add_or_replace_if_module_configured(
                    RethRpcModule::Miner,
                    miner_ext.clone().into_rpc(),
                )?;

                if modules.module_config().contains_any(&RethRpcModule::Miner) {
                    debug!(target: "reth::cli", "Installing miner DA rpc endpoint");
                    auth_module.merge_auth_methods(miner_ext.into_rpc())?;
                }

                if modules.module_config().contains_any(&RethRpcModule::Debug) {
                    debug!(target: "reth::cli", "Installing debug rpc endpoint");
                    auth_module.merge_auth_methods(registry.debug_api().into_rpc())?;
                }

                if enable_tx_conditional {
                    modules.merge_if_module_configured(
                        RethRpcModule::Eth,
                        tx_conditional_ext.into_rpc(),
                    )?;
                }

                modules.replace_configured(world_chain_eth_ext.into_rpc())?;
                modules.replace_configured(flashblocks_op_api.into_rpc())?;

                if simulate_enabled {
                    let simulate_api =
                        Simulate::from_eth_api(provider, evm_config, registry.eth_api());
                    modules.merge_http(simulate_api.into_rpc())?;
                }

                Ok(())
            })
            .await?;

        // Now that the RPC server is live, spawn the in-process Kona consensus node (if enabled).
        // Kona reaches reth's standard (non-engine) L2 RPC over IPC when the IPC server is enabled,
        // otherwise it falls back to the HTTP RPC endpoint. Both are read from the running server so
        // they reflect what reth actually bound, rather than being re-derived from config.
        if let Some((
            kona_config,
            engine_handle,
            payload_builder_handle,
            task_executor,
            flashblocks_authorizer,
        )) = kona_inputs
        {
            let rpc = &handle.rpc_server_handles.rpc;
            let l2_endpoint = match rpc.ipc_endpoint() {
                Some(ipc_path) => L2RpcEndpoint::Ipc(ipc_path),
                None => {
                    let http_url = rpc.http_url().ok_or_else(|| {
                        eyre::Report::msg(
                            "--kona.enabled requires reth's IPC or HTTP RPC server \
                             (enable at least one of --ipc / --http)",
                        )
                    })?;
                    L2RpcEndpoint::Http(http_url.parse()?)
                }
            };

            spawn_kona(
                kona_config,
                engine_handle,
                payload_builder_handle,
                task_executor,
                l2_endpoint,
                flashblocks_authorizer,
            )
            .await?;
        }

        Ok(handle)
    }
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware, Tx> RethRpcAddOns<N>
    for WorldChainAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware, Tx>
where
    N: FullNodeComponents<
            Types: NodeTypes<ChainSpec = WorldChainSpec, Payload = OpEngineTypes>,
            Evm: ConfigurePostExecEvm<
                NextBlockEnvCtx: BuildNextEnv<
                    OpPayloadBuilderAttributes<Tx>,
                    Header,
                    WorldChainSpec,
                >,
            >,
            Pool: TransactionPool<Transaction: OpPooledTx<Consensus = Tx>>,
        >,
    PrimitivesTy<N::Types>: WorldChainRpcPrimitives<Tx>,
    N::Provider: BlockReaderIdExt
        + ChainSpecProvider<ChainSpec = WorldChainSpec>
        + HeaderProvider<Header = Header>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
    EthB: EthApiBuilder<N>,
    PVB: PayloadValidatorBuilder<N>,
    EB: EngineApiBuilder<N>,
    EVB: EngineValidatorBuilder<N>,
    RpcMiddleware: RethRpcMiddleware,
    Tx: FullSignedTx + OpTransaction + From<Sealed<TxPostExec>>,
    <<N::Evm as ConfigureEvm>::BlockExecutorFactory as BlockExecutorFactory>::EvmFactory:
        EvmFactory<Tx = OpTx>,
{
    type EthApi = EthB::EthApi;

    fn hooks_mut(&mut self) -> &mut reth_node_builder::rpc::RpcHooks<N, Self::EthApi> {
        self.rpc_add_ons.hooks_mut()
    }
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware, Tx> EngineValidatorAddOn<N>
    for WorldChainAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware, Tx>
where
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
    PVB: Send,
    EB: EngineApiBuilder<N>,
    EVB: EngineValidatorBuilder<N>,
    RpcMiddleware: Send,
{
    type ValidatorBuilder = EVB;

    fn engine_validator_builder(&self) -> Self::ValidatorBuilder {
        EngineValidatorAddOn::engine_validator_builder(&self.rpc_add_ons)
    }
}

/// Assembles the in-process Kona consensus node from reth's engine handle and the live RPC
/// server's IPC endpoint, then spawns it on the node's task executor.
///
/// Builds the kona service's in-process engine client from:
/// - `engine_handle` — reth's `ConsensusEngineHandle`, for the FCU / new-payload consensus hot
///   path,
/// - `payload_store` — wrapping reth's payload builder handle, for `get_payload`,
/// - an L2 alloy provider connected over `l2_endpoint` (the live RPC endpoint reported by the
///   running server — IPC when enabled, otherwise HTTP — not re-derived from config), for the
///   infrequent reads the engine actor performs during sync,
/// - an L1 alloy provider over HTTP from `--kona.l1-rpc-url`.
///
/// Connecting the L2 provider may be asynchronous (IPC), so this is an `async fn`. The assembled
/// [`KonaService`] is then run on the provided `task_executor` for the node's lifetime.
async fn spawn_kona(
    kona_config: KonaConfig,
    engine_handle: ConsensusEngineHandle<OpEngineTypes>,
    payload_builder_handle: PayloadBuilderHandle<OpEngineTypes>,
    task_executor: TaskExecutor,
    l2_endpoint: L2RpcEndpoint,
    flashblocks_authorizer: Option<FlashblocksAuthorizationNotifier>,
) -> eyre::Result<()> {
    let sequencer_mode = kona_config.sequencer_mode;
    let l1_chain_id = kona_config.rollup_config.l1_chain_id;
    let l2_chain_id: u64 = kona_config.rollup_config.l2_chain_id.into();

    // Build once up front so a misconfigured Kona (missing/unparsable rollup config, unreachable
    // endpoints) aborts node startup via `?` rather than starting a node without a working
    // consensus engine.
    let initial_service = build_kona_service(
        &kona_config,
        &engine_handle,
        &payload_builder_handle,
        &l2_endpoint,
        &flashblocks_authorizer,
    )
    .await?;

    info!(
        target: "world_chain::kona",
        %l1_chain_id,
        %l2_chain_id,
        sequencer = sequencer_mode,
        "Starting in-process Kona consensus node (direct ConsensusEngineHandle transport)"
    );

    // Spawn on the node's task executor so the service lives for the node's lifetime.
    //
    // The in-process Kona node is reth's only engine driver: if it stops while the node is
    // running, reth keeps serving but silently stops advancing (no FCU / new-payload), leaving a
    // half-dead node that liveness probes on the EL don't catch — so Kona staying down is fatal.
    //
    // A *transient* Kona stop, however, is recoverable and must not be fatal. A node that has
    // fallen far behind (its safe head's L1 origin is beyond the sequencing window) while gossip
    // drives its unsafe head to the tip can hit a consolidation seal race
    // (`Consolidate(SealTaskFailed(UnsafeHeadChangedSinceBuild))`), which kona's engine classifies
    // as a critical error and tears the consensus node down. Rebuilding the service re-reads
    // reth's *current* EL head and re-initialises the engine forkchoice/derivation from there,
    // letting the node catch up — exactly the recovery a fresh start would perform. So we supervise
    // the consensus node with bounded restart-and-backoff instead of panicking the whole process on
    // the first stop. A node that genuinely cannot stay up (too many restarts inside the quiet
    // period) still panics, bringing the pod down so the orchestrator restarts it and op-conductor
    // fails over.
    //
    // `spawn_critical_task` only notifies reth's `TaskManager` on a panic (normal completion is
    // ignored), and it wraps the future in `select(on_shutdown, ..)` — during a legitimate node
    // shutdown `on_shutdown` wins and this future is dropped before the loop observes a stop. So the
    // panic below fires only when Kona cannot stay alive on its own.
    const MAX_CONSECUTIVE_RESTARTS: u32 = 10;
    const RESTART_QUIET_PERIOD: Duration = Duration::from_secs(60);
    const RESTART_BACKOFF: Duration = Duration::from_secs(2);

    task_executor.spawn_critical_task("kona-consensus", async move {
        let mut next_service = Some(initial_service);
        let mut consecutive_restarts = 0u32;

        loop {
            let service = match next_service.take() {
                Some(service) => service,
                None => match build_kona_service(
                    &kona_config,
                    &engine_handle,
                    &payload_builder_handle,
                    &l2_endpoint,
                    &flashblocks_authorizer,
                )
                .await
                {
                    Ok(service) => service,
                    Err(error) => {
                        error!(target: "world_chain::kona", %error, "Failed to rebuild Kona consensus node; bringing down the node");
                        panic!("Failed to rebuild Kona consensus node: {error}");
                    }
                },
            };

            // Run the service to completion. Dropping the handle here (before the rebuild/backoff)
            // cancels the old service's tasks and releases its P2P listener socket.
            let started = Instant::now();
            let outcome = {
                let mut handle = KonaServiceHandle::spawn(service);
                handle.stopped().await
            };

            // Treat a run that stayed up for the quiet period as healthy: reset the counter so only
            // a tight crash loop trips the fatal limit.
            if started.elapsed() >= RESTART_QUIET_PERIOD {
                consecutive_restarts = 0;
            }
            consecutive_restarts += 1;

            match outcome {
                Ok(()) => error!(
                    target: "world_chain::kona",
                    restart = consecutive_restarts,
                    "Kona consensus node stopped unexpectedly; restarting"
                ),
                Err(error) => error!(
                    target: "world_chain::kona",
                    %error,
                    restart = consecutive_restarts,
                    "Kona consensus node exited with error; restarting"
                ),
            }

            if consecutive_restarts >= MAX_CONSECUTIVE_RESTARTS {
                error!(
                    target: "world_chain::kona",
                    restarts = consecutive_restarts,
                    "Kona consensus node restarted too many times without staying up; bringing down the node"
                );
                panic!("Kona consensus node restarted {consecutive_restarts} times without staying up");
            }

            tokio::time::sleep(RESTART_BACKOFF).await;
        }
    });

    Ok(())
}

/// Builds a fresh [`KonaService`] from the supervisor's retained, cloneable inputs.
///
/// A fresh `PayloadStore` is constructed per call from the (cloneable) payload builder handle since
/// `PayloadStore` is not itself `Clone`. Used both for the initial build and for each supervised
/// restart, so each (re)start re-reads reth's current head when re-initialising derivation.
async fn build_kona_service(
    kona_config: &KonaConfig,
    engine_handle: &ConsensusEngineHandle<OpEngineTypes>,
    payload_builder_handle: &PayloadBuilderHandle<OpEngineTypes>,
    l2_endpoint: &L2RpcEndpoint,
    flashblocks_authorizer: &Option<FlashblocksAuthorizationNotifier>,
) -> eyre::Result<KonaService> {
    KonaService::build(
        kona_config.clone(),
        engine_handle.clone(),
        PayloadStore::new(payload_builder_handle.clone()),
        l2_endpoint.clone(),
        flashblocks_authorizer.clone(),
    )
    .await
}
