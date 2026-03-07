use flashblocks_builder::{
    FlashblocksPayloadBuilderConfig,
    coordinator::FlashblocksPayloadExecutor,
    event_stream::{FlashblocksEventStream, PendingBlockRef},
    payload_builder::FlashblocksPayloadBuilder,
    traits::{context::PayloadBuilderCtx, context_builder::PayloadBuilderCtxBuilder},
};
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use op_alloy_consensus::OpTxEnvelope;
use reth::builder::{BuilderContext, FullNodeTypes, components::PayloadBuilderBuilder};
use reth_node_api::{NodeTypes, PayloadTypes};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::{
    OpBuiltPayload, OpEvmConfig, OpPayloadBuilderAttributes, txpool::OpPooledTx,
};
use reth_optimism_primitives::OpPrimitives;
use reth_provider::{
    ChainSpecProvider, DatabaseProviderFactory, HeaderProvider, StateProviderFactory,
};
use reth_transaction_pool::{PoolTransaction, TransactionPool};
use tokio::sync::watch;

/// Builds the flashblocks payload builder and spawns the event stream.
#[derive(Debug)]
pub struct FlashblocksPayloadBuilderBuilder<CtxBuilder> {
    pub ctx_builder: CtxBuilder,
    pub p2p_handle: Option<FlashblocksHandle>,
    pub pending_block_tx: Option<watch::Sender<PendingBlockRef<OpPrimitives>>>,
    pub builder_config: FlashblocksPayloadBuilderConfig,
}

impl<CtxBuilder> FlashblocksPayloadBuilderBuilder<CtxBuilder> {
    pub fn new(
        ctx_builder: CtxBuilder,
        p2p_handle: Option<FlashblocksHandle>,
        pending_block_tx: Option<watch::Sender<PendingBlockRef<OpPrimitives>>>,
        builder_config: FlashblocksPayloadBuilderConfig,
    ) -> Self {
        Self {
            ctx_builder,
            builder_config,
            p2p_handle,
            pending_block_tx,
        }
    }
}

impl<Node, Pool, CtxBuilder> PayloadBuilderBuilder<Node, Pool, OpEvmConfig>
    for FlashblocksPayloadBuilderBuilder<CtxBuilder>
where
    Node: FullNodeTypes,
    Node::Provider: StateProviderFactory
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + Clone
        + DatabaseProviderFactory<Provider: HeaderProvider<Header = alloy_consensus::Header>>
        + HeaderProvider<Header = alloy_consensus::Header>,
    Node::Types: NodeTypes<
            ChainSpec = OpChainSpec,
            Payload: PayloadTypes<
                BuiltPayload = OpBuiltPayload,
                PayloadBuilderAttributes = OpPayloadBuilderAttributes<
                    op_alloy_consensus::OpTxEnvelope,
                >,
            >,
        >,
    Pool: TransactionPool<Transaction: OpPooledTx + PoolTransaction<Consensus = OpTxEnvelope>>
        + Unpin
        + 'static,
    CtxBuilder: PayloadBuilderCtxBuilder<
            Node::Provider,
            OpEvmConfig,
            OpChainSpec,
            PayloadBuilderCtx: PayloadBuilderCtx<Transaction = Pool::Transaction>,
        > + 'static,
{
    type PayloadBuilder = FlashblocksPayloadBuilder<Pool, Node::Provider, CtxBuilder, ()>;

    async fn build_payload_builder(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        evm_config: OpEvmConfig,
    ) -> eyre::Result<Self::PayloadBuilder> {
        if let (Some(p2p_handle), Some(pending_block_tx)) = (self.p2p_handle, self.pending_block_tx)
        {
            let p2p_stream = Box::pin(p2p_handle.flashblock_stream());
            let executor = FlashblocksPayloadExecutor::new(
                ctx.provider().clone(),
                evm_config.clone(),
                ctx.chain_spec().clone(),
            );
            let event_stream = FlashblocksEventStream::new(p2p_stream, executor, pending_block_tx);
            ctx.task_executor()
                .spawn_critical("flashblocks event stream", event_stream.run());
        }

        let payload_builder = FlashblocksPayloadBuilder {
            evm_config,
            pool,
            client: ctx.provider().clone(),
            config: self.builder_config,
            best_transactions: (),
            ctx_builder: self.ctx_builder,
        };

        Ok(payload_builder)
    }
}
