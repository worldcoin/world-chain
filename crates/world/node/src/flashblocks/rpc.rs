use alloy_rpc_types::engine::ClientVersionV1;
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use op_alloy_rpc_types_engine::OpExecutionData;
use reth::version::version_metadata;
use reth::{payload::PayloadStore, version::CLIENT_CODE};
use reth_node_api::{
    AddOnsContext, EngineApiValidator, EngineTypes, FullNodeComponents, NodeTypes,
};
use reth_node_builder::rpc::{EngineApiBuilder, EthApiBuilder, PayloadValidatorBuilder, RpcAddOns};
use reth_optimism_node::{OpDAConfig, OP_NAME_CLIENT};
use reth_optimism_rpc::{OpEngineApi, OpEthApiBuilder, OP_ENGINE_CAPABILITIES};
use reth_primitives::EthereumHardforks;
use reth_rpc_engine_api::{EngineApi, EngineCapabilities};
use rollup_boost::{ed25519_dalek::VerifyingKey, Authorization};
use std::marker::PhantomData;
use tower::layer::util::Identity;
use world_chain_builder_flashblocks::builder::executor::FlashblocksStateExecutor;
use world_chain_builder_flashblocks::rpc::engine::OpEngineApiExt;
use world_chain_builder_flashblocks::rpc::eth::FlashblocksEthApiBuilder;

use crate::flashblocks::add_ons::FlashblocksAddOns;

/// Builder for basic [`OpEngineApiExt`] implementation.
pub struct WorldChainEngineApiBuilder<EV> {
    /// The engine validator builder.
    pub engine_validator_builder: EV,
    /// The flashblocks handler.
    pub flashblocks_handle: Option<FlashblocksHandle>,
    /// A watch channel notifier to the jobs generator.
    pub to_jobs_generator: tokio::sync::watch::Sender<Option<Authorization>>,
    /// Verifying key for authorizations.
    pub verifying_key: VerifyingKey,
}

impl<EV: Default> Default for WorldChainEngineApiBuilder<EV> {
    fn default() -> Self {
        let (to_jobs_generator, _) = tokio::sync::watch::channel(None);
        Self {
            engine_validator_builder: Default::default(),
            flashblocks_handle: None,
            to_jobs_generator,
            verifying_key: VerifyingKey::from_bytes(&[0u8; 32]).expect("valid key"),
        }
    }
}

impl<N, EV> EngineApiBuilder<N> for WorldChainEngineApiBuilder<EV>
where
    N: FullNodeComponents<
        Types: NodeTypes<
            ChainSpec: EthereumHardforks + Clone,
            Payload: EngineTypes<ExecutionData = OpExecutionData>,
        >,
    >,
    EV: PayloadValidatorBuilder<N>,
    EV::Validator: EngineApiValidator<<N::Types as NodeTypes>::Payload> + Clone,
{
    type EngineApi = OpEngineApiExt<
        N::Provider,
        <N::Types as NodeTypes>::Payload,
        N::Pool,
        EV::Validator,
        <N::Types as NodeTypes>::ChainSpec,
    >;

    async fn build_engine_api(
        self,
        ctx: &AddOnsContext<'_, N>,
    ) -> eyre::eyre::Result<Self::EngineApi> {
        let Self {
            engine_validator_builder,
            to_jobs_generator,
            ..
        } = self;

        let engine_validator = engine_validator_builder.build(ctx).await?;

        let client = ClientVersionV1 {
            code: CLIENT_CODE,
            name: OP_NAME_CLIENT.to_string(),
            version: version_metadata().cargo_pkg_version.to_string(),
            commit: version_metadata().vergen_git_sha.to_string(),
        };

        let mut capabilities = EngineCapabilities::new(OP_ENGINE_CAPABILITIES.iter().copied());
        capabilities.add_capability("flashblocks_forkChoiceUpdatedV3");
        let inner = EngineApi::new(
            ctx.node.provider().clone(),
            ctx.config.chain.clone(),
            ctx.beacon_engine_handle.clone(),
            PayloadStore::new(ctx.node.payload_builder_handle().clone()),
            ctx.node.pool().clone(),
            Box::new(ctx.node.task_executor().clone()),
            client,
            capabilities,
            engine_validator,
            ctx.config.engine.accept_execution_requests_hash,
        );

        let op_engine_api = OpEngineApi::new(inner);
        let op_engine_api_ext = OpEngineApiExt::new(op_engine_api, to_jobs_generator);

        Ok(op_engine_api_ext)
    }
}

/// A regular optimism evm and executor builder.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FlashblocksAddOnsBuilder<NetworkT, RpcMiddleware = Identity> {
    /// Sequencer client, configured to forward submitted transactions to sequencer of given OP
    /// network.
    sequencer_url: Option<String>,
    /// Headers to use for the sequencer client requests.
    sequencer_headers: Vec<String>,
    /// RPC endpoint for historical data.
    historical_rpc: Option<String>,
    /// Data availability configuration for the OP builder.
    da_config: Option<OpDAConfig>,
    /// Enable transaction conditionals.
    enable_tx_conditional: bool,
    /// Marker for network types.
    _nt: PhantomData<NetworkT>,
    /// Minimum suggested priority fee (tip)
    min_suggested_priority_fee: u64,
    /// RPC middleware to use
    rpc_middleware: RpcMiddleware,
    /// Optional tokio runtime to use for the RPC server.
    tokio_runtime: Option<tokio::runtime::Handle>,
    /// The flashblocks executor
    state_executor: FlashblocksStateExecutor,
}

impl<NetworkT> FlashblocksAddOnsBuilder<NetworkT, Identity> {
    pub fn new(state_executor: FlashblocksStateExecutor) -> Self {
        Self {
            sequencer_url: None,
            sequencer_headers: Vec::new(),
            historical_rpc: None,
            da_config: None,
            enable_tx_conditional: false,
            min_suggested_priority_fee: 1_000_000,
            _nt: PhantomData,
            tokio_runtime: None,
            state_executor,
            rpc_middleware: Identity::new(),
        }
    }
}

impl<NetworkT, RpcMiddleware> FlashblocksAddOnsBuilder<NetworkT, RpcMiddleware> {
    /// With a [`SequencerClient`].
    pub fn with_sequencer(mut self, sequencer_client: Option<String>) -> Self {
        self.sequencer_url = sequencer_client;
        self
    }

    /// With headers to use for the sequencer client requests.
    pub fn with_sequencer_headers(mut self, sequencer_headers: Vec<String>) -> Self {
        self.sequencer_headers = sequencer_headers;
        self
    }

    /// Configure the data availability configuration for the OP builder.
    pub fn with_da_config(mut self, da_config: OpDAConfig) -> Self {
        self.da_config = Some(da_config);
        self
    }

    /// Configure if transaction conditional should be enabled.
    pub const fn with_enable_tx_conditional(mut self, enable_tx_conditional: bool) -> Self {
        self.enable_tx_conditional = enable_tx_conditional;
        self
    }

    /// Configure the minimum priority fee (tip)
    pub const fn with_min_suggested_priority_fee(mut self, min: u64) -> Self {
        self.min_suggested_priority_fee = min;
        self
    }

    /// Configures the endpoint for historical RPC forwarding.
    pub fn with_historical_rpc(mut self, historical_rpc: Option<String>) -> Self {
        self.historical_rpc = historical_rpc;
        self
    }

    /// Configures a custom tokio runtime for the RPC server.
    ///
    /// Caution: This runtime must not be created from within asynchronous context.
    pub fn with_tokio_runtime(mut self, tokio_runtime: Option<tokio::runtime::Handle>) -> Self {
        self.tokio_runtime = tokio_runtime;
        self
    }

    /// Configure the RPC middleware to use
    pub fn with_rpc_middleware<T>(
        self,
        rpc_middleware: T,
    ) -> FlashblocksAddOnsBuilder<NetworkT, T> {
        let Self {
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            da_config,
            enable_tx_conditional,
            min_suggested_priority_fee,
            tokio_runtime,
            _nt,
            state_executor,
            ..
        } = self;
        FlashblocksAddOnsBuilder {
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            da_config,
            enable_tx_conditional,
            min_suggested_priority_fee,
            _nt,
            rpc_middleware,
            tokio_runtime,
            state_executor,
        }
    }
}

impl<NetworkT, RpcMiddleware> FlashblocksAddOnsBuilder<NetworkT, RpcMiddleware> {
    /// Builds an instance of [`OpAddOns`].
    pub fn build<N, PVB, EB, EVB>(
        self,
    ) -> FlashblocksAddOns<N, FlashblocksEthApiBuilder<NetworkT>, PVB, EB, EVB, RpcMiddleware>
    where
        N: FullNodeComponents<Types: NodeTypes>,
        FlashblocksEthApiBuilder<NetworkT>: EthApiBuilder<N> + Default,
        PVB: PayloadValidatorBuilder<N> + Default,
        EB: Default,
        EVB: Default,
    {
        let Self {
            sequencer_url,
            sequencer_headers,
            da_config,
            enable_tx_conditional,
            min_suggested_priority_fee,
            historical_rpc,
            rpc_middleware,
            tokio_runtime,
            state_executor,
            ..
        } = self;

        let op_eth_api = OpEthApiBuilder::default()
            .with_sequencer(sequencer_url.clone())
            .with_sequencer_headers(sequencer_headers.clone())
            .with_min_suggested_priority_fee(min_suggested_priority_fee);

        FlashblocksAddOns::new(
            RpcAddOns::new(
                FlashblocksEthApiBuilder::new(op_eth_api, state_executor),
                PVB::default(),
                EB::default(),
                EVB::default(),
                rpc_middleware,
            )
            .with_tokio_runtime(tokio_runtime),
            da_config.unwrap_or_default(),
            sequencer_url,
            sequencer_headers,
            historical_rpc,
            enable_tx_conditional,
            min_suggested_priority_fee,
        )
    }
}
