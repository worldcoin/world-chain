use flashblocks_builder::executor::FlashblocksStateExecutor;
use flashblocks_builder::traits::context::PayloadBuilderCtx;
use flashblocks_builder::traits::context_builder::PayloadBuilderCtxBuilder;
use flashblocks_builder::FlashblocksPayloadBuilder;
use op_alloy_consensus::OpTxEnvelope;
use reth::builder::components::PayloadBuilderBuilder;
use reth::builder::{BuilderContext, FullNodeTypes};
use reth::chainspec::EthChainSpec;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::OpPooledTx;
use reth_optimism_node::OpEvmConfig;
use reth_optimism_payload_builder::config::{OpBuilderConfig, OpDAConfig};
use reth_optimism_primitives::OpPrimitives;
use reth_provider::{
    ChainSpecProvider, DatabaseProviderFactory, HeaderProvider, StateProviderFactory,
};
use reth_transaction_pool::{PoolTransaction, TransactionPool};
use world_chain_pool::tx::WorldChainPooledTransaction;
use world_chain_provider::InMemoryState;

use crate::context::FlashblocksContext;
use crate::node::WorldChainNode;

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
    Node: FullNodeTypes<Types = WorldChainNode<FlashblocksContext>>,
    <Node as FullNodeTypes>::Provider: StateProviderFactory
        + ChainSpecProvider<ChainSpec: EthChainSpec + OpHardforks>
        + Clone
        + DatabaseProviderFactory<Provider: HeaderProvider<Header = alloy_consensus::Header>>,
    Node::Provider: InMemoryState<Primitives = OpPrimitives>,
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
        self.flashblocks_state
            .launch::<_, _, _, WorldChainPooledTransaction>(
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
