use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use reth::{api::PayloadBuilderAttributes, payload::PayloadJobGenerator, tasks::TaskSpawner};
use reth_basic_payload_builder::{
    BasicPayloadJob, BasicPayloadJobGenerator, BasicPayloadJobGeneratorConfig, HeaderForPayload,
    PayloadBuilder,
};

use reth_primitives::NodePrimitives;
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use rollup_boost::{
    ed25519_dalek::{SigningKey, VerifyingKey},
    Authorization,
};

/// A type that initiates payload building jobs on the [`FlashblocksPayloadBuilder`].
pub struct FlashblockJobGenerator<Client, Tasks, Builder> {
    /// The inner payload job generator.
    inner: BasicPayloadJobGenerator<Client, Tasks, Builder>,
    /// The P2P handler for flashblocks.
    p2p_handler: FlashblocksHandle,
    /// The authorization signing key for the builder.
    authorizer_sk: SigningKey,
    /// The verifying key for the builder.
    builder_vk: VerifyingKey,
}

impl<Client, Tasks, Builder> FlashblockJobGenerator<Client, Tasks, Builder> {
    /// Creates a new [`FlashblockJobGenerator`].
    pub fn new(
        client: Client,
        executor: Tasks,
        config: BasicPayloadJobGeneratorConfig,
        builder: Builder,
        p2p_handler: FlashblocksHandle,
        authorizer_sk: SigningKey,
        builder_vk: VerifyingKey,
    ) -> Self {
        Self {
            inner: BasicPayloadJobGenerator::with_builder(client, executor, config, builder),
            p2p_handler,
            authorizer_sk,
            builder_vk,
        }
    }
}

impl<Client, Tasks, Builder> PayloadJobGenerator for FlashblockJobGenerator<Client, Tasks, Builder>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Header = HeaderForPayload<Builder::BuiltPayload>>
        + Clone
        + Unpin
        + 'static,
    Tasks: TaskSpawner + Clone + Unpin + 'static,
    Builder: PayloadBuilder + Unpin + 'static,
    Builder::Attributes: Unpin + Clone,
    Builder::BuiltPayload: Unpin + Clone,
{
    type Job = BasicPayloadJob<Tasks, Builder>;

    fn new_payload_job(
        &self,
        attr: <Self::Job as reth::payload::PayloadJob>::PayloadAttributes,
    ) -> Result<Self::Job, reth::api::PayloadBuilderError> {
        let payload_id = attr.payload_id();
        let timestamp = attr.timestamp();

        let authorization = Authorization::new(
            payload_id,
            timestamp,
            &self.authorizer_sk.clone(),
            self.builder_vk,
        );

        self.p2p_handler.start_publishing(authorization);

        self.inner.new_payload_job(attr)
    }

    fn on_new_state<N: NodePrimitives>(
        &mut self,
        new_state: reth_provider::CanonStateNotification<N>,
    ) {
        self.inner.on_new_state(new_state);
    }
}
