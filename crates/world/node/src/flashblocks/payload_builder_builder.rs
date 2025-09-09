use std::marker::PhantomData;

use alloy_primitives::Address;
use eyre::eyre::Context;
use op_alloy_consensus::OpTxEnvelope;
use reth::builder::components::PayloadBuilderBuilder;
use reth::builder::{BuilderContext, FullNodeTypes};
use reth::chainspec::EthChainSpec;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::OpPooledTx;
use reth_optimism_node::OpEvmConfig;
use reth_optimism_payload_builder::builder::OpPayloadTransactions;
use reth_optimism_payload_builder::config::{OpBuilderConfig, OpDAConfig};
use reth_optimism_primitives::OpPrimitives;
use reth_provider::{
    ChainSpecProvider, DatabaseProviderFactory, HeaderProvider, StateProviderFactory,
};
use reth_transaction_pool::{BlobStore, PoolTransaction, TransactionPool};
use world_chain_builder_flashblocks::builder::executor::FlashblocksStateExecutor;
use world_chain_builder_flashblocks::builder::FlashblocksPayloadBuilder;
use world_chain_builder_flashblocks::{PayloadBuilderCtx, PayloadBuilderCtxBuilder};
use world_chain_builder_payload::context::WorldChainPayloadBuilderCtxBuilder;
use world_chain_builder_pool::tx::WorldChainPooledTransaction;
use world_chain_builder_pool::WorldChainTransactionPool;
use world_chain_provider::InMemoryState;

use crate::context::FlashblocksContext;
use crate::node::WorldChainNode;

#[derive(Debug, Clone)]
pub struct FlashblocksPayloadBuilderBuilder<Pool, CtxBuilder> {
    // /// By default the pending block equals the latest block
    // /// to save resources and not leak txs from the tx-pool,
    // /// this flag enables computing of the pending block
    // /// from the tx-pool instead.
    // ///
    // /// If `compute_pending_block` is not enabled, the payload builder
    // /// will use the payload attributes from the latest block. Note
    // /// that this flag is not yet functional.
    // pub compute_pending_block: bool,
    /// The type responsible for yielding the best transactions for the payload if mempool
    /// transactions are allowed.
    // pub best_transactions: Txs,
    pub ctx_builder: CtxBuilder,
    // /// Sets the private key of the builder
    // /// used for signing the stampBlock transaction
    // pub builder_private_key: String,
    pub flashblocks_state: FlashblocksStateExecutor,

    /// Da config
    pub da_config: OpDAConfig,

    pub _pool: PhantomData<Pool>,
}

impl<Pool, CtxBuilder> FlashblocksPayloadBuilderBuilder<Pool, CtxBuilder> {
    /// Create a new instance with the given `compute_pending_block` flag and data availability
    /// config.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ctx_builder: CtxBuilder,
        // compute_pending_block: bool,
        flashblocks_state: FlashblocksStateExecutor,
        da_config: OpDAConfig,
    ) -> Self {
        Self {
            // compute_pending_block,
            ctx_builder,
            // best_transactions: (),
            da_config,
            flashblocks_state,
            _pool: Default::default(),
        }
    }

    // /// Configure the data availability configuration for the OP payload builder.
    // pub fn with_da_config(mut self, da_config: OpDAConfig) -> Self {
    //     self.da_config = da_config;
    //     self
    // }
}

// impl<Txs: OpPayloadTransactions<WorldChainPooledTransaction>>
//     FlashblocksPayloadBuilderBuilder<Txs>
// {
//     /// Configures the type responsible for yielding the transactions that should be included in the
//     /// payload.
//     pub fn with_transactions<T>(self, best: T) -> FlashblocksPayloadBuilderBuilder<T> {
//         let Self {
//             compute_pending_block,
//             da_config,
//             verified_blockspace_capacity,
//             pbh_entry_point,
//             pbh_signature_aggregator,
//             builder_private_key,
//             best_transactions: _,
//             flashblocks_state,
//         } = self;
//
//         FlashblocksPayloadBuilderBuilder {
//             compute_pending_block,
//             da_config,
//             verified_blockspace_capacity,
//             pbh_entry_point,
//             pbh_signature_aggregator,
//             builder_private_key,
//             best_transactions: best,
//             flashblocks_state,
//         }
//     }
// }

impl<Node, Pool, CtxBuilder> PayloadBuilderBuilder<Node, Pool, OpEvmConfig>
    for FlashblocksPayloadBuilderBuilder<Pool, CtxBuilder>
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
    // Txs: OpPayloadTransactions<<Pool as TransactionPool>::Transaction>,
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
            best_transactions: (), // self.best_transactions.clone(),
            ctx_builder: self.ctx_builder,
        };

        Ok(payload_builder)
    }
}
