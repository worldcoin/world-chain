use flashblocks_builder::executor::FlashblocksStateExecutor;
use flashblocks_builder::traits::context::PayloadBuilderCtx;
use flashblocks_builder::traits::context_builder::PayloadBuilderCtxBuilder;
use flashblocks_builder::FlashblocksPayloadBuilder;
use flashblocks_provider::InMemoryState;
use op_alloy_consensus::OpTxEnvelope;
use reth::builder::components::PayloadBuilderBuilder;
use reth::builder::{BuilderContext, FullNodeTypes};
use reth_node_api::{NodeTypes, PayloadTypes};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::txpool::OpPooledTx;
use reth_optimism_node::{OpBuiltPayload, OpEvmConfig, OpPayloadBuilderAttributes};
use reth_optimism_payload_builder::config::{OpBuilderConfig, OpDAConfig};
use reth_optimism_primitives::OpPrimitives;
use reth_provider::{
    ChainSpecProvider, DatabaseProviderFactory, HeaderProvider, StateProviderFactory,
};
use reth_transaction_pool::{PoolTransaction, TransactionPool};

#[derive(Debug, Clone)]
pub struct FlashblocksPayloadBuilderBuilder<CtxBuilder> {
    pub ctx_builder: CtxBuilder,
    pub flashblocks_state: FlashblocksStateExecutor,
    pub da_config: OpDAConfig,
}

impl<CtxBuilder> FlashblocksPayloadBuilderBuilder<CtxBuilder> {
    /// Create a new instance with the given `compute_pending_block` flag and data availability
    /// config.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ctx_builder: CtxBuilder,
        flashblocks_state: FlashblocksStateExecutor,
        da_config: OpDAConfig,
    ) -> Self {
        Self {
            ctx_builder,
            da_config,
            flashblocks_state,
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
        + InMemoryState<Primitives = OpPrimitives>
        + HeaderProvider<Header = alloy_consensus::Header>,
    Node::Types: NodeTypes<
        ChainSpec = OpChainSpec,
        Payload: PayloadTypes<
            BuiltPayload = OpBuiltPayload,
            PayloadBuilderAttributes = OpPayloadBuilderAttributes<op_alloy_consensus::OpTxEnvelope>,
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
        self.flashblocks_state.launch::<_, _, _>(
            ctx,
            pool.clone(),
            self.ctx_builder.clone(),
            evm_config.clone(),
        );

        let payload_builder = FlashblocksPayloadBuilder {
            evm_config,
            pool,
            client: ctx.provider().clone(),
            config: OpBuilderConfig {
                da_config: self.da_config,
            },
            best_transactions: (),
            ctx_builder: self.ctx_builder,
        };

        Ok(payload_builder)
    }
}
