use std::time::Duration;

use ed25519_dalek::SigningKey;
use flashblocks_builder::{
    coordinator::FlashblocksExecutionCoordinator, traits::payload_builder::FlashblockPayloadBuilder,
};
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use flashblocks_payload::{
    generator::{FlashblocksJobGeneratorConfig, FlashblocksPayloadJobGenerator},
    metrics::PayloadBuilderMetrics,
};
use flashblocks_primitives::p2p::Authorization;
use reth::payload::{PayloadBuilderHandle, PayloadBuilderService};
use reth_basic_payload_builder::{BasicPayloadJobGenerator, BasicPayloadJobGeneratorConfig};
use reth_node_api::{FullNodeTypes, NodeTypes, PayloadTypes};
use reth_node_builder::{
    BuilderContext,
    components::{PayloadBuilderBuilder, PayloadServiceBuilder},
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::{OpBuiltPayload, OpEngineTypes, OpNodeTypes, OpPayloadBuilderAttributes};
use reth_provider::{
    CanonStateSubscriptions, ChainSpecProvider, DatabaseProviderFactory, HeaderProvider,
    StateProviderFactory,
};
use reth_transaction_pool::TransactionPool;

/// Basic payload service builder that spawns a [`BasicPayloadJobGenerator`]
#[derive(Debug)]
pub struct FlashblocksPayloadServiceBuilder<PB> {
    pb: PB,
    p2p_handler: Option<FlashblocksHandle>,
    flashblocks_state: Option<FlashblocksExecutionCoordinator>,
    authorizations_rx: Option<tokio::sync::watch::Receiver<Option<Authorization>>>,
    override_authorizer_sk: Option<SigningKey>,
    force_publish: bool,
    interval: Duration,
    recommitment_interval: Duration,
}

impl<PB> FlashblocksPayloadServiceBuilder<PB> {
    /// Create a new [`FlashblocksPayloadServiceBuilder`].
    pub const fn new(
        pb: PB,
        p2p_handler: Option<FlashblocksHandle>,
        flashblocks_state: Option<FlashblocksExecutionCoordinator>,
        authorizations_rx: Option<tokio::sync::watch::Receiver<Option<Authorization>>>,
        override_authorizer_sk: Option<SigningKey>,
        force_publish: bool,
        interval: Duration,
        recommitment_interval: Duration,
    ) -> Self {
        Self {
            pb,
            p2p_handler,
            flashblocks_state,
            authorizations_rx,
            override_authorizer_sk,
            force_publish,
            interval,
            recommitment_interval,
        }
    }
}

impl<Node, Pool, PB, EvmConfig> PayloadServiceBuilder<Node, Pool, EvmConfig>
    for FlashblocksPayloadServiceBuilder<PB>
where
    Node: FullNodeTypes<Types: OpNodeTypes<Payload = OpEngineTypes>>,
    Node::Provider: StateProviderFactory
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + HeaderProvider<Header = alloy_consensus::Header>
        + Clone
        + DatabaseProviderFactory<Provider: HeaderProvider<Header = alloy_consensus::Header>>,
    Node::Types: NodeTypes<
            ChainSpec = OpChainSpec,
            Payload: PayloadTypes<
                BuiltPayload = OpBuiltPayload,
                PayloadBuilderAttributes = OpPayloadBuilderAttributes<
                    op_alloy_consensus::OpTxEnvelope,
                >,
            >,
        >,
    Pool: TransactionPool,
    EvmConfig: Send,
    PB: PayloadBuilderBuilder<Node, Pool, EvmConfig>,
    PB::PayloadBuilder: FlashblockPayloadBuilder,
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
            .interval(self.interval)
            .recommitment_interval(self.recommitment_interval)
            .deadline(conf.deadline)
            .max_payload_tasks(conf.max_payload_tasks);

        let metrics = PayloadBuilderMetrics::default();
        if let (Some(p2p_handler), Some(authorizations_rx), Some(flashblocks_state)) = (
            self.p2p_handler,
            self.authorizations_rx,
            self.flashblocks_state,
        ) {
            // flashblocks enabled
            let payload_generator = FlashblocksPayloadJobGenerator::with_builder(
                ctx.provider().clone(),
                ctx.task_executor().clone(),
                payload_job_config,
                payload_builder,
                p2p_handler,
                authorizations_rx.clone(),
                self.override_authorizer_sk,
                self.force_publish,
                flashblocks_state.clone(),
                metrics,
            );

            let (payload_service, payload_service_handle) = PayloadBuilderService::new(
                payload_generator,
                ctx.provider().canonical_state_stream(),
            );

            let payload_events = payload_service.payload_events_handle();
            flashblocks_state.register_payload_events(payload_events);

            ctx.task_executor()
                .spawn_critical("payload builder service", Box::pin(payload_service));

            Ok(payload_service_handle)
        } else {
            // flahsblocks disabled
            let payload_job_config = BasicPayloadJobGeneratorConfig::default()
                .interval(conf.interval)
                .deadline(conf.deadline)
                .max_payload_tasks(conf.max_payload_tasks);

            let payload_generator = BasicPayloadJobGenerator::with_builder(
                ctx.provider().clone(),
                ctx.task_executor().clone(),
                payload_job_config,
                payload_builder,
            );
            let (payload_service, payload_service_handle) = PayloadBuilderService::new(
                payload_generator,
                ctx.provider().canonical_state_stream(),
            );

            ctx.task_executor()
                .spawn_critical("payload builder service", Box::pin(payload_service));

            Ok(payload_service_handle)
        }
    }
}
