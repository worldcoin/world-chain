use alloy_primitives::FixedBytes;
use alloy_rpc_types::engine::ClientVersionV1;
use flashblocks::rpc::engine::{FlashblocksState, OpEngineApiExt};
use flashblocks_p2p::protocol::handler::{
    FlashblocksHandle, FlashblocksP2PCtx, FlashblocksP2PState,
};
use op_alloy_rpc_types_engine::OpExecutionData;
use parking_lot::lock_api::Mutex;
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
use rollup_boost::ed25519_dalek::{SigningKey, VerifyingKey};
use std::sync::Arc;

/// Builder for basic [`OpEngineApiExt`] implementation.
pub struct WorldChainEngineApiBuilder<EV> {
    /// The engine validator builder.
    pub engine_validator_builder: EV,
    /// The flashblocks handler.
    pub flashblocks_handle: FlashblocksHandle,
    /// The flashblocks state.
    pub flashblocks_state: FlashblocksState,
}

impl<EV: Default> Default for WorldChainEngineApiBuilder<EV> {
    fn default() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(0);
        let (peer_tx, _) = tokio::sync::broadcast::channel(0);
        Self {
            engine_validator_builder: EV::default(),
            flashblocks_handle: FlashblocksHandle {
                ctx: FlashblocksP2PCtx {
                    authorizer_vk: VerifyingKey::default(),
                    builder_sk: SigningKey::from_bytes(FixedBytes::<32>::default().as_ref()),
                    peer_tx,
                    flashblock_tx: tx,
                },
                state: Arc::new(Mutex::new(FlashblocksP2PState::default())),
            },
            flashblocks_state: FlashblocksState::default(),
        }
    }
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
            flashblocks_handle,
            flashblocks_state,
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
            flashblocks_state,
            ctx.node.task_executor(),
            flashblocks_handle.flashblock_stream(),
        );

        Ok(op_engine_api_ext)
    }
}
