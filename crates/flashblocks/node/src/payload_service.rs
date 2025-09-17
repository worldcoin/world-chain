use flashblocks_builder::executor::FlashblocksStateExecutor;
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use flashblocks_payload::generator::{
    FlashblocksJobGeneratorConfig, FlashblocksPayloadJobGenerator,
};
use flashblocks_primitives::p2p::Authorization;
use flashblocks_provider::InMemoryState;
use reth::payload::{PayloadBuilderHandle, PayloadBuilderService};
use reth_node_api::{FullNodeTypes, NodeTypes, PayloadTypes};
use reth_node_builder::{
    components::{PayloadBuilderBuilder, PayloadServiceBuilder},
    BuilderContext,
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::{OpBuiltPayload, OpPayloadBuilderAttributes};
use reth_optimism_primitives::OpPrimitives;
use reth_provider::{
    CanonStateSubscriptions, ChainSpecProvider, DatabaseProviderFactory, HeaderProvider,
    StateProviderFactory,
};
use reth_transaction_pool::TransactionPool;

/// Basic payload service builder that spawns a [`BasicPayloadJobGenerator`]
#[derive(Debug, Clone)]
pub struct FlashblocksPayloadServiceBuilder<PB> {
    pb: PB,
    p2p_handler: FlashblocksHandle,
    flashblocks_state: FlashblocksStateExecutor,
    authorizations_rx: tokio::sync::watch::Receiver<Option<Authorization>>,
}

impl<PB> FlashblocksPayloadServiceBuilder<PB> {
    /// Create a new [`FlashblocksPayloadServiceBuilder`].
    pub const fn new(
        pb: PB,
        p2p_handler: FlashblocksHandle,
        flashblocks_state: FlashblocksStateExecutor,
        authorizations_rx: tokio::sync::watch::Receiver<Option<Authorization>>,
    ) -> Self {
        Self {
            pb,
            p2p_handler,
            flashblocks_state,
            authorizations_rx,
        }
    }
}

impl<Node, Pool, PB, EvmConfig> PayloadServiceBuilder<Node, Pool, EvmConfig>
    for FlashblocksPayloadServiceBuilder<PB>
where
    Node: FullNodeTypes,
    Node::Provider: StateProviderFactory
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + HeaderProvider<Header = alloy_consensus::Header>
        + Clone
        + DatabaseProviderFactory<Provider: HeaderProvider<Header = alloy_consensus::Header>>
        + InMemoryState<Primitives = OpPrimitives>,
    Node::Types: NodeTypes<
        ChainSpec = OpChainSpec,
        Payload: PayloadTypes<
            BuiltPayload = OpBuiltPayload,
            PayloadBuilderAttributes = OpPayloadBuilderAttributes<op_alloy_consensus::OpTxEnvelope>,
        >,
    >,
    Pool: TransactionPool,
    EvmConfig: Send,
    PB: PayloadBuilderBuilder<Node, Pool, EvmConfig>,
{
    async fn spawn_payload_builder_service(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        evm_config: EvmConfig,
    ) -> eyre::Result<PayloadBuilderHandle<<Node::Types as NodeTypes>::Payload>> {
        let payload_builder = self.pb.build_payload_builder(ctx, pool, evm_config).await?;

        let conf = ctx.config().builder.clone();

        let payload_job_config = FlashblocksJobGeneratorConfig::default()
            .interval(conf.interval)
            .deadline(conf.deadline);

        let payload_generator = FlashblocksPayloadJobGenerator::with_builder(
            ctx.provider().clone(),
            ctx.task_executor().clone(),
            payload_job_config,
            payload_builder,
            self.p2p_handler,
            self.authorizations_rx.clone(),
            self.flashblocks_state,
        );

        let (payload_service, payload_service_handle) =
            PayloadBuilderService::new(payload_generator, ctx.provider().canonical_state_stream());

        ctx.task_executor()
            .spawn_critical("payload builder service", Box::pin(payload_service));

        Ok(payload_service_handle)
    }
}
