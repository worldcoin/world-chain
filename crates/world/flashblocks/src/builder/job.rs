use std::sync::atomic::AtomicBool;

use flashblocks_p2p::protocol::handler::FlashblocksHandler;
use reth::{
    api::{PayloadAttributes, PayloadBuilderAttributes},
    payload::{PayloadJob, PayloadJobGenerator},
    rpc::builder::auth,
    tasks::TaskSpawner,
};
use reth_basic_payload_builder::{
    BasicPayloadJob, BasicPayloadJobGenerator, BasicPayloadJobGeneratorConfig, HeaderForPayload,
    PayloadBuilder,
};

use reth_primitives::NodePrimitives;
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use rollup_boost::{
    ed25519_dalek::{SigningKey, VerifyingKey},
    Auth, Authorization, FlashblocksP2PMsg,
};
use tokio::{sync::broadcast, task::JoinHandle};

#[derive(Debug)]
pub struct FlashblockP2PManager<N> {
    /// The P2P network handler.
    pub p2p_handler: FlashblocksHandler<N>,
    /// The receiver for flashblocks messages.
    pub flashblock_rx: broadcast::Receiver<FlashblocksP2PMsg>,
    /// The authorization for the builder.
    pub authorization: Authorization,
    /// A bool representing whether the P2P manager is publishing or not.
    pub publishing: AtomicBool,
}

impl<N: NodePrimitives> FlashblockP2PManager<N> {
    pub fn new(
        p2p_handler: FlashblocksHandler<N>,
        flashblock_rx: broadcast::Receiver<FlashblocksP2PMsg>,
        authorization: Authorization,
    ) -> Self {
        Self {
            p2p_handler,
            flashblock_rx,
            publishing: AtomicBool::new(false),
            authorization,
        }
    }

    /// Spawns the [`FlashblockP2PManager`] into a perpetual task that listens for new flashblocks.
    pub fn spawn(mut self) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                // Start publishing
                let p2p_msg = self.flashblock_rx.recv().await;
            }
        })
    }
}

/// A type that initiates payload building jobs on the [`FlashblocksPayloadBuilder`].
pub struct FlashblockJobGenerator<N, Client, Tasks, Builder> {
    /// The inner payload job generator.
    inner: BasicPayloadJobGenerator<Client, Tasks, Builder>,
    /// The P2P handler for flashblocks.
    p2p_handler: FlashblocksHandler<N>,
    /// The authorization signing key for the builder.
    authorizer_sk: SigningKey,
    /// The verifying key for the builder.
    builder_vk: VerifyingKey,
}

impl<N, Client, Tasks, Builder> FlashblockJobGenerator<N, Client, Tasks, Builder> {
    /// Creates a new [`FlashblockJobGenerator`].
    pub fn new(
        client: Client,
        executor: Tasks,
        config: BasicPayloadJobGeneratorConfig,
        builder: Builder,
        p2p_handler: FlashblocksHandler<N>,
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

impl<N, Client, Tasks, Builder> PayloadJobGenerator
    for FlashblockJobGenerator<N, Client, Tasks, Builder>
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
    N: NodePrimitives + Clone + Unpin + 'static,
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
            self.builder_vk.clone(),
        );

        // Spawn

        self.inner.new_payload_job(attr)
    }

    fn on_new_state<Node: NodePrimitives>(
        &mut self,
        new_state: reth_provider::CanonStateNotification<Node>,
    ) {
        self.inner.on_new_state(new_state);
    }
}
