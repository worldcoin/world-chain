use crate::flashblocks::rpc::{FlashblocksAddOnsBuilder, WorldChainEngineApiBuilder};
use reth::rpc::builder::RethRpcModule;
use reth_evm::ConfigureEvm;
use reth_node_api::{
    BuildNextEnv, FullNodeComponents, HeaderTy, NodeAddOns, NodeTypes, PayloadTypes, TxTy,
};
use reth_node_builder::rpc::{
    BasicEngineValidatorBuilder, EngineApiBuilder, EngineValidatorBuilder, EthApiBuilder,
    RethRpcMiddleware, RethRpcServerHandles, RpcAddOns, RpcContext, RpcHandle,
};
use reth_node_builder::rpc::{EngineValidatorAddOn, PayloadValidatorBuilder, RethRpcAddOns};

use reth::rpc::api::DebugApiServer;
use reth::rpc::api::L2EthApiExtServer;
use reth_optimism_forks::OpHardfork;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::OpPooledTx;
use reth_optimism_node::{OpDAConfig, OpNodeTypes};
use reth_optimism_node::{OpEngineValidatorBuilder, OpPayloadPrimitives};
use reth_optimism_payload_builder::OpAttributes;
use reth_optimism_rpc::eth::ext::OpEthExtApi;
use reth_optimism_rpc::historical::{HistoricalRpc, HistoricalRpcClient};
use reth_optimism_rpc::miner::OpMinerExtApi;
use reth_optimism_rpc::witness::OpDebugWitnessApi;
use reth_optimism_rpc::SequencerClient;
use reth_optimism_rpc::{miner::MinerApiExtServer, witness::DebugExecutionWitnessApiServer};
use reth_provider::ChainSpecProvider;
use reth_transaction_pool::TransactionPool;
use serde::de::DeserializeOwned;
use tower::layer::util::Identity;
use tracing::{debug, info};
use world_chain_builder_flashblocks::{
    builder::executor::FlashblocksStateExecutor, rpc::eth::FlashblocksEthApiBuilder,
};

/// Add-ons w.r.t. world chain.
///
/// This type provides optimism-specific addons to the node and exposes the RPC server and engine
/// API.
#[derive(Debug)]
pub struct FlashblocksAddOns<
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
    PVB,
    EB = WorldChainEngineApiBuilder<PVB>,
    EVB = BasicEngineValidatorBuilder<PVB>,
    RpcMiddleware = Identity,
> {
    /// Rpc add-ons responsible for launching the RPC servers and instantiating the RPC handlers
    /// and eth-api.
    pub rpc_add_ons: RpcAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>,
    /// Data availability configuration for the OP builder.
    pub da_config: OpDAConfig,
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
    min_suggested_priority_fee: u64,
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware> FlashblocksAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
{
    /// Creates a new instance from components.
    pub const fn new(
        rpc_add_ons: RpcAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>,
        da_config: OpDAConfig,
        sequencer_url: Option<String>,
        sequencer_headers: Vec<String>,
        historical_rpc: Option<String>,
        enable_tx_conditional: bool,
        min_suggested_priority_fee: u64,
    ) -> Self {
        Self {
            rpc_add_ons,
            da_config,
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            enable_tx_conditional,
            min_suggested_priority_fee,
        }
    }
}

impl<N> Default for FlashblocksAddOns<N, FlashblocksEthApiBuilder, OpEngineValidatorBuilder>
where
    N: FullNodeComponents<Types: OpNodeTypes>,
    FlashblocksEthApiBuilder: EthApiBuilder<N>,
{
    fn default() -> Self {
        unimplemented!("Default is not supported, use `FlashblocksAddOns::builder` instead")
    }
}

impl<N, NetworkT, RpcMiddleware>
    FlashblocksAddOns<
        N,
        FlashblocksEthApiBuilder<NetworkT>,
        OpEngineValidatorBuilder,
        WorldChainEngineApiBuilder<OpEngineValidatorBuilder>,
        RpcMiddleware,
    >
where
    N: FullNodeComponents<Types: OpNodeTypes>,
    FlashblocksEthApiBuilder<NetworkT>: EthApiBuilder<N>,
{
    /// Build a [`FlashblocksAddOns`] using [`OpAddOnsBuilder`].
    pub fn builder(state_executor: FlashblocksStateExecutor) -> FlashblocksAddOnsBuilder<NetworkT> {
        FlashblocksAddOnsBuilder::new(state_executor)
    }
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware> FlashblocksAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
{
    /// Maps the [`reth_node_builder::rpc::EngineApiBuilder`] builder type.
    pub fn with_engine_api<T>(
        self,
        engine_api_builder: T,
    ) -> FlashblocksAddOns<N, EthB, PVB, T, EVB, RpcMiddleware> {
        let Self {
            rpc_add_ons,
            da_config,
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            enable_tx_conditional,
            min_suggested_priority_fee,
            ..
        } = self;
        FlashblocksAddOns::new(
            rpc_add_ons.with_engine_api(engine_api_builder),
            da_config,
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            enable_tx_conditional,
            min_suggested_priority_fee,
        )
    }

    /// Maps the [`PayloadValidatorBuilder`] builder type.
    pub fn with_payload_validator<T>(
        self,
        payload_validator_builder: T,
    ) -> FlashblocksAddOns<N, EthB, T, EB, EVB, RpcMiddleware> {
        let Self {
            rpc_add_ons,
            da_config,
            sequencer_url,
            sequencer_headers,
            enable_tx_conditional,
            min_suggested_priority_fee,
            historical_rpc,
            ..
        } = self;
        FlashblocksAddOns::new(
            rpc_add_ons.with_payload_validator(payload_validator_builder),
            da_config,
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            enable_tx_conditional,
            min_suggested_priority_fee,
        )
    }

    /// Sets the RPC middleware stack for processing RPC requests.
    ///
    /// This method configures a custom middleware stack that will be applied to all RPC requests
    /// across HTTP, `WebSocket`, and IPC transports. The middleware is applied to the RPC service
    /// layer, allowing you to intercept, modify, or enhance RPC request processing.
    ///
    /// See also [`RpcAddOns::with_rpc_middleware`].
    pub fn with_rpc_middleware<T>(
        self,
        rpc_middleware: T,
    ) -> FlashblocksAddOns<N, EthB, PVB, EB, EVB, T> {
        let Self {
            rpc_add_ons,
            da_config,
            sequencer_url,
            sequencer_headers,
            enable_tx_conditional,
            min_suggested_priority_fee,
            historical_rpc,
            ..
        } = self;
        FlashblocksAddOns::new(
            rpc_add_ons.with_rpc_middleware(rpc_middleware),
            da_config,
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            enable_tx_conditional,
            min_suggested_priority_fee,
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

impl<N, EthB, PVB, EB, EVB, Attrs, RpcMiddleware> NodeAddOns<N>
    for FlashblocksAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents<
        Types: NodeTypes<
            ChainSpec: OpHardforks,
            Primitives: OpPayloadPrimitives,
            Payload: PayloadTypes<PayloadBuilderAttributes = Attrs>,
        >,
        Evm: ConfigureEvm<
            NextBlockEnvCtx: BuildNextEnv<
                Attrs,
                HeaderTy<N::Types>,
                <N::Types as NodeTypes>::ChainSpec,
            >,
        >,
        Pool: TransactionPool<Transaction: OpPooledTx>,
    >,
    EthB: EthApiBuilder<N>,
    PVB: Send,
    EB: EngineApiBuilder<N>,
    EVB: EngineValidatorBuilder<N>,
    RpcMiddleware: RethRpcMiddleware,
    Attrs: OpAttributes<Transaction = TxTy<N::Types>, RpcPayloadAttributes: DeserializeOwned>,
{
    type Handle = RpcHandle<N, EthB::EthApi>;

    async fn launch_add_ons(
        self,
        ctx: reth_node_api::AddOnsContext<'_, N>,
    ) -> eyre::Result<Self::Handle> {
        let Self {
            rpc_add_ons,
            da_config,
            sequencer_url,
            sequencer_headers,
            enable_tx_conditional,
            historical_rpc,
            ..
        } = self;

        let maybe_pre_bedrock_historical_rpc = historical_rpc
            .and_then(|historical_rpc| {
                ctx.node
                    .provider()
                    .chain_spec()
                    .op_fork_activation(OpHardfork::Bedrock)
                    .block_number()
                    .filter(|activation| *activation > 0)
                    .map(|bedrock_block| (historical_rpc, bedrock_block))
            })
            .map(|(historical_rpc, bedrock_block)| -> eyre::Result<_> {
                info!(target: "reth::cli", %bedrock_block, ?historical_rpc, "Using historical RPC endpoint pre bedrock");
                let provider = ctx.node.provider().clone();
                let client = HistoricalRpcClient::new(&historical_rpc)?;
                let layer = HistoricalRpc::new(provider, client, bedrock_block);
                Ok(layer)
            })
            .transpose()?
            ;

        let rpc_add_ons = rpc_add_ons.option_layer_rpc_middleware(maybe_pre_bedrock_historical_rpc);

        let builder = reth_optimism_payload_builder::OpPayloadBuilder::new(
            ctx.node.pool().clone(),
            ctx.node.provider().clone(),
            ctx.node.evm_config().clone(),
        );
        // install additional OP specific rpc methods
        let debug_ext = OpDebugWitnessApi::<_, _, _, Attrs>::new(
            ctx.node.provider().clone(),
            Box::new(ctx.node.task_executor().clone()),
            builder,
        );
        let miner_ext = OpMinerExtApi::new(da_config);

        let sequencer_client = if let Some(url) = sequencer_url {
            Some(SequencerClient::new_with_headers(url, sequencer_headers).await?)
        } else {
            None
        };

        let tx_conditional_ext: OpEthExtApi<N::Pool, N::Provider> = OpEthExtApi::new(
            sequencer_client,
            ctx.node.pool().clone(),
            ctx.node.provider().clone(),
        );

        rpc_add_ons
            .launch_add_ons_with(ctx, move |container| {
                let reth_node_builder::rpc::RpcModuleContainer {
                    modules,
                    auth_module,
                    registry,
                } = container;

                debug!(target: "reth::cli", "Installing debug payload witness rpc endpoint");
                modules.merge_if_module_configured(RethRpcModule::Debug, debug_ext.into_rpc())?;

                // extend the miner namespace if configured in the regular http server
                modules.merge_if_module_configured(
                    RethRpcModule::Miner,
                    miner_ext.clone().into_rpc(),
                )?;

                // install the miner extension in the authenticated if configured
                if modules.module_config().contains_any(&RethRpcModule::Miner) {
                    debug!(target: "reth::cli", "Installing miner DA rpc endpoint");
                    auth_module.merge_auth_methods(miner_ext.into_rpc())?;
                }

                // install the debug namespace in the authenticated if configured
                if modules.module_config().contains_any(&RethRpcModule::Debug) {
                    debug!(target: "reth::cli", "Installing debug rpc endpoint");
                    auth_module.merge_auth_methods(registry.debug_api().into_rpc())?;
                }

                if enable_tx_conditional {
                    // extend the eth namespace if configured in the regular http server
                    modules.merge_if_module_configured(
                        RethRpcModule::Eth,
                        tx_conditional_ext.into_rpc(),
                    )?;
                }

                Ok(())
            })
            .await
    }
}

impl<N, EthB, PVB, EB, EVB, Attrs, RpcMiddleware> RethRpcAddOns<N>
    for FlashblocksAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware>
where
    N: FullNodeComponents<
        Types: NodeTypes<
            ChainSpec: OpHardforks,
            Primitives: OpPayloadPrimitives,
            Payload: PayloadTypes<PayloadBuilderAttributes = Attrs>,
        >,
        Evm: ConfigureEvm<
            NextBlockEnvCtx: BuildNextEnv<
                Attrs,
                HeaderTy<N::Types>,
                <N::Types as NodeTypes>::ChainSpec,
            >,
        >,
    >,
    <<N as FullNodeComponents>::Pool as TransactionPool>::Transaction: OpPooledTx,
    EthB: EthApiBuilder<N>,
    PVB: PayloadValidatorBuilder<N>,
    EB: EngineApiBuilder<N>,
    EVB: EngineValidatorBuilder<N>,
    RpcMiddleware: RethRpcMiddleware,
    Attrs: OpAttributes<Transaction = TxTy<N::Types>, RpcPayloadAttributes: DeserializeOwned>,
{
    type EthApi = EthB::EthApi;

    fn hooks_mut(&mut self) -> &mut reth_node_builder::rpc::RpcHooks<N, Self::EthApi> {
        self.rpc_add_ons.hooks_mut()
    }
}

impl<N, NetworkT, PVB, EB, EVB> EngineValidatorAddOn<N>
    for FlashblocksAddOns<N, FlashblocksEthApiBuilder<NetworkT>, PVB, EB, EVB>
where
    N: FullNodeComponents,
    FlashblocksEthApiBuilder<NetworkT>: EthApiBuilder<N>,
    PVB: Send,
    EB: EngineApiBuilder<N>,
    EVB: EngineValidatorBuilder<N>,
{
    type ValidatorBuilder = EVB;

    fn engine_validator_builder(&self) -> Self::ValidatorBuilder {
        EngineValidatorAddOn::engine_validator_builder(&self.rpc_add_ons)
    }
}
