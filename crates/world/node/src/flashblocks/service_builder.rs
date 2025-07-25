use flashblocks::builder::job::FlashblockJobGenerator;
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use reth::payload::{PayloadBuilderHandle, PayloadBuilderService};
use reth_basic_payload_builder::BasicPayloadJobGeneratorConfig;
use reth_node_api::{FullNodeTypes, NodeTypes};
use reth_node_builder::{
    components::{PayloadBuilderBuilder, PayloadServiceBuilder},
    BuilderContext,
};
use reth_provider::CanonStateSubscriptions;
use reth_transaction_pool::TransactionPool;
use rollup_boost::ed25519_dalek::{SigningKey, VerifyingKey};

/// Basic payload service builder that spawns a [`BasicPayloadJobGenerator`]
#[derive(Debug, Clone)]
pub struct FlashblocksPayloadServiceBuilder<PB> {
    pb: PB,
    builder_vk: VerifyingKey,
    authorizer_sk: SigningKey,
    p2p_handler: FlashblocksHandle,
}

impl<PB> FlashblocksPayloadServiceBuilder<PB> {
    /// Create a new [`FlashblocksPayloadServiceBuilder`].
    pub const fn new(
        pb: PB,
        builder_vk: VerifyingKey,
        authorizer_sk: SigningKey,
        p2p_handler: FlashblocksHandle,
    ) -> Self {
        Self {
            pb,
            builder_vk,
            authorizer_sk,
            p2p_handler,
        }
    }
}

impl<Node, Pool, PB, EvmConfig> PayloadServiceBuilder<Node, Pool, EvmConfig>
    for FlashblocksPayloadServiceBuilder<PB>
where
    Node: FullNodeTypes,
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

        let payload_job_config = BasicPayloadJobGeneratorConfig::default()
            .interval(conf.interval)
            .deadline(conf.deadline)
            .max_payload_tasks(conf.max_payload_tasks);

        let payload_generator = FlashblockJobGenerator::new(
            ctx.provider().clone(),
            ctx.task_executor().clone(),
            payload_job_config,
            payload_builder,
            self.p2p_handler,
            self.authorizer_sk.clone(),
            self.builder_vk.clone(),
        );

        let (payload_service, payload_service_handle) =
            PayloadBuilderService::new(payload_generator, ctx.provider().canonical_state_stream());

        ctx.task_executor()
            .spawn_critical("payload builder service", Box::pin(payload_service));

        Ok(payload_service_handle)
    }
}
