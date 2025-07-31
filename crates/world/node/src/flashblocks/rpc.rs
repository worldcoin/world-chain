use alloy_rpc_types::engine::{ClientVersionV1, ExecutionData};
use flashblocks::rpc::engine::{FlashblocksState, OpEngineApiExt};
use op_alloy_rpc_types_engine::OpExecutionData;
use reth::{
    payload::PayloadStore,
    version::{CARGO_PKG_VERSION, CLIENT_CODE, VERGEN_GIT_SHA},
};
use reth_node_api::{AddOnsContext, EngineTypes, FullNodeComponents, NodeTypes};
use reth_node_builder::rpc::{EngineApiBuilder, EngineValidatorBuilder};
use reth_optimism_node::OP_NAME_CLIENT;
use reth_optimism_rpc::{OpEngineApi, OP_ENGINE_CAPABILITIES};
use reth_primitives::EthereumHardforks;
use reth_rpc_engine_api::{EngineApi, EngineCapabilities};

/// Builder for basic [`OpEngineApiExt`] implementation.
pub struct WorldChainEngineApiBuilder<EV> {
    /// The engine validator builder.
    engine_validator_builder: EV,
}

impl<N, EV> EngineApiBuilder<N> for WorldChainEngineApiBuilder<EV>
where
    N: FullNodeComponents<
        Types: NodeTypes<
            ChainSpec: EthereumHardforks,
            Payload: EngineTypes<ExecutionData = OpExecutionData>,
        >,
        // TODO: Bound Network = OurCustomNetwork to get access to the FlashblocksHandler
        // This will give us the ability to grab the stream here.
        // Additionally we'll want to store `FlashblocksState` on our `Network` type, so we can use it 
        // from `NodeContext`, and `AddOnsContext` this will allow us to initialize the Any other RPC module overrides with the `FlashblocksState`
        // After initializing the NodeComponents
    >,
    EV: EngineValidatorBuilder<N>,
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
        } = self;

        let engine_validator = engine_validator_builder.build(ctx).await?;

        let client = ClientVersionV1 {
            code: CLIENT_CODE,
            name: OP_NAME_CLIENT.to_string(),
            version: CARGO_PKG_VERSION.to_string(),
            commit: VERGEN_GIT_SHA.to_string(),
        };

        let inner = EngineApi::new(
            ctx.node.provider().clone(),
            ctx.config.chain.clone(),
            ctx.beacon_engine_handle.clone(),
            PayloadStore::new(ctx.node.payload_builder_handle().clone()),
            ctx.node.pool().clone(),
            Box::new(ctx.node.task_executor().clone()),
            client,
            EngineCapabilities::new(OP_ENGINE_CAPABILITIES.iter().copied()),
            engine_validator,
            ctx.config.engine.accept_execution_requests_hash,
        );

        let op_engine_api = OpEngineApi::new(inner);

        let op_engine_api_ext = OpEngineApiExt::new(
            op_engine_api,
            FlashblocksState::default(),
            ctx.node.task_executor(),
            todo!(),
        );
    }
}
