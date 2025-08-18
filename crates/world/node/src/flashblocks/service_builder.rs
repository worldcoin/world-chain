use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use reth::payload::{PayloadBuilderHandle, PayloadBuilderService};
use reth_node_api::{FullNodeTypes, NodeTypes};
use reth_node_builder::{
    components::{PayloadBuilderBuilder, PayloadServiceBuilder},
    BuilderContext,
};
use reth_provider::CanonStateSubscriptions;
use reth_transaction_pool::TransactionPool;
use rollup_boost::{ed25519_dalek::SigningKey, Authorization};
use world_chain_builder_flashblocks::{
    payload::generator::{FlashblocksJobGeneratorConfig, WorldChainPayloadJobGenerator},
    primitives::FlashblocksState,
};

use crate::{context::FlashblocksContext, node::WorldChainNode};

/// Basic payload service builder that spawns a [`BasicPayloadJobGenerator`]
#[derive(Debug, Clone)]
pub struct FlashblocksPayloadServiceBuilder<PB> {
    pb: PB,
    p2p_handler: FlashblocksHandle,
    flashblocks_state: FlashblocksState,
    authorizations_rx: tokio::sync::watch::Receiver<Option<Authorization>>,
    builder_sk: SigningKey,
}

impl<PB> FlashblocksPayloadServiceBuilder<PB> {
    /// Create a new [`FlashblocksPayloadServiceBuilder`].
    pub const fn new(
        pb: PB,
        p2p_handler: FlashblocksHandle,
        flashblocks_state: FlashblocksState,
        authorizations_rx: tokio::sync::watch::Receiver<Option<Authorization>>,
        builder_sk: SigningKey,
    ) -> Self {
        Self {
            pb,
            p2p_handler,
            flashblocks_state,
            authorizations_rx,
            builder_sk,
        }
    }
}

impl<Node, Pool, PB, EvmConfig> PayloadServiceBuilder<Node, Pool, EvmConfig>
    for FlashblocksPayloadServiceBuilder<PB>
where
    Node: FullNodeTypes<Types = WorldChainNode<FlashblocksContext>>,
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

        let payload_generator = WorldChainPayloadJobGenerator::with_builder(
            ctx.provider().clone(),
            ctx.task_executor().clone(),
            payload_job_config,
            payload_builder,
            self.p2p_handler,
            self.authorizations_rx.clone(),
            self.flashblocks_state,
            self.builder_sk,
        );

        let (payload_service, payload_service_handle) =
            PayloadBuilderService::new(payload_generator, ctx.provider().canonical_state_stream());

        ctx.task_executor()
            .spawn_critical("payload builder service", Box::pin(payload_service));

        Ok(payload_service_handle)
    }
}
