use op_alloy_consensus::OpTxEnvelope;
use reth::{payload::PayloadJobGenerator, tasks::TaskSpawner};
use reth_basic_payload_builder::{
    BasicPayloadJob, BasicPayloadJobGenerator, BasicPayloadJobGeneratorConfig, HeaderForPayload,
    PayloadBuilder,
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::{txpool::OpPooledTx, OpBuiltPayload};
use reth_provider::{BlockReaderIdExt, ChainSpecProvider, StateProviderFactory};
use reth_transaction_pool::TransactionPool;
use rollup_boost::FlashblocksP2PMsg;
use tokio::sync::broadcast;

use crate::{builder::FlashblocksPayloadBuilder, context::PayloadBuilderCtxBuilder};

/// A type that initiates payload building jobs on the [`FlashblocksPayloadBuilder`].
pub struct FlashblockJobGenerator<Client, Tasks, Builder> {
    inner: BasicPayloadJobGenerator<Client, Tasks, Builder>,
    // TODO: Add broadcast::Receiver<FlashblocksP2PMsg> or maybe this should hold a type that handles all that.
    // We can trigger P2PHandler::start_publish() on `new_payload_job` as the entrypoint
}

impl<Client, Tasks, Builder> FlashblockJobGenerator<Client, Tasks, Builder> {
    /// Creates a new [`FlashblockJobGenerator`].
    pub fn new(
        client: Client,
        executor: Tasks,
        config: BasicPayloadJobGeneratorConfig,
        builder: Builder,
    ) -> Self {
        Self {
            inner: BasicPayloadJobGenerator::with_builder(client, executor, config, builder),
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
        // TODO(@forerunner)
        // P2PHandler::start_publish()
        self.inner.new_payload_job(attr)
    }

    fn on_new_state<N: reth_primitives::NodePrimitives>(
        &mut self,
        new_state: reth_provider::CanonStateNotification<N>,
    ) {
        self.inner.on_new_state(new_state);
    }
}
