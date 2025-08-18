use alloy_rpc_types::engine::ClientVersionV1;
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use op_alloy_rpc_types_engine::OpExecutionData;
use reth::version::version_metadata;
use reth::{payload::PayloadStore, version::CLIENT_CODE};
use reth_node_api::{
    AddOnsContext, EngineApiValidator, EngineTypes, FullNodeComponents, NodeTypes,
};
use reth_node_builder::rpc::{EngineApiBuilder, PayloadValidatorBuilder};
use reth_optimism_node::OP_NAME_CLIENT;
use reth_optimism_rpc::{OpEngineApi, OP_ENGINE_CAPABILITIES};
use reth_primitives::EthereumHardforks;
use reth_rpc_engine_api::{EngineApi, EngineCapabilities};
use rollup_boost::{ed25519_dalek::VerifyingKey, Authorization};
use world_chain_builder_flashblocks::{primitives::FlashblocksState, rpc::engine::OpEngineApiExt};

/// Builder for basic [`OpEngineApiExt`] implementation.
pub struct WorldChainEngineApiBuilder<EV> {
    /// The engine validator builder.
    pub engine_validator_builder: EV,
    /// The flashblocks handler.
    pub flashblocks_handle: Option<FlashblocksHandle>,
    /// The flashblocks state.
    pub flashblocks_state: Option<FlashblocksState>,
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
            flashblocks_state: None,
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
            flashblocks_handle,
            flashblocks_state,
            to_jobs_generator,
            ..
        } = self;

        let flashblocks_handle = flashblocks_handle.expect("Flashblocks handle is required");
        let flashblocks_state = flashblocks_state.expect("Flashblocks state is required");

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
        let op_engine_api_ext = OpEngineApiExt::new(
            op_engine_api,
            flashblocks_state,
            ctx.node.task_executor(),
            flashblocks_handle.flashblock_stream(),
            to_jobs_generator,
        );

        Ok(op_engine_api_ext)
    }
}
