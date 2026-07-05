//! World Chain node add-ons.

use core::marker::PhantomData;

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
use reth_optimism_node::{OpEngineApiBuilder, txpool::OpPooledTx};
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
use tracing::{debug, info};
use world_chain_chainspec::WorldChainSpec;
use world_chain_evm::OpTx;
use world_chain_rpc::{
    AdminApiExtServer, EthApiExtServer, SequencerClient as WorldChainSequencerClient, Simulate,
    SimulateApiServer, WorldChainAdminApiExt, WorldChainEthApiExt,
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
            Types: NodeTypes<ChainSpec = WorldChainSpec>,
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
            ..
        } = self;

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

        rpc_add_ons
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

                let admin = RethRpcModule::Admin;
                let admin_on_http = modules.module_config().contains_http(&admin);
                let admin_on_ws = modules.module_config().contains_ws(&admin);
                if admin_on_http || admin_on_ws {
                    let admin_ext = WorldChainAdminApiExt::new().into_rpc();
                    if admin_on_http {
                        modules.merge_http(admin_ext.clone())?;
                    }
                    if admin_on_ws {
                        modules.merge_ws(admin_ext)?;
                    }
                }

                if simulate_enabled {
                    let simulate_api =
                        Simulate::from_eth_api(provider, evm_config, registry.eth_api());
                    modules.merge_http(simulate_api.into_rpc())?;
                }

                Ok(())
            })
            .await
    }
}

impl<N, EthB, PVB, EB, EVB, RpcMiddleware, Tx> RethRpcAddOns<N>
    for WorldChainAddOns<N, EthB, PVB, EB, EVB, RpcMiddleware, Tx>
where
    N: FullNodeComponents<
            Types: NodeTypes<ChainSpec = WorldChainSpec>,
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
