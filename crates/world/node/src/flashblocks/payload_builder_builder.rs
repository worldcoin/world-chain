use alloy_primitives::Address;
use eyre::eyre::Context;
use reth::builder::components::PayloadBuilderBuilder;
use reth::builder::{BuilderContext, FullNodeTypes, NodeTypes};
use reth::chainspec::EthChainSpec;
use reth_node_api::FullNodeComponents;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::{OpPooledTransaction, OpPooledTx};
use reth_optimism_node::OpEvmConfig;
use reth_optimism_payload_builder::builder::OpPayloadTransactions;
use reth_optimism_payload_builder::config::{OpBuilderConfig, OpDAConfig};
use reth_optimism_primitives::OpPrimitives;
use reth_payload_util::PayloadTransactions;
use reth_provider::{ChainSpecProvider, StateProviderFactory};
use reth_transaction_pool::BlobStore;
use world_chain_builder_flashblocks::builder::executor::FlashblocksStateExecutor;
use world_chain_builder_flashblocks::builder::FlashblocksPayloadBuilder;
use world_chain_builder_payload::context::WorldChainPayloadBuilderCtxBuilder;
use world_chain_builder_pool::tx::WorldChainPooledTransaction;
use world_chain_builder_pool::WorldChainTransactionPool;
use world_chain_provider::InMemoryState;

use crate::context::FlashblocksContext;
use crate::node::WorldChainNode;

#[derive(Debug, Clone)]
pub struct FlashblocksPayloadBuilderBuilder<Txs = ()> {
    /// By default the pending block equals the latest block
    /// to save resources and not leak txs from the tx-pool,
    /// this flag enables computing of the pending block
    /// from the tx-pool instead.
    ///
    /// If `compute_pending_block` is not enabled, the payload builder
    /// will use the payload attributes from the latest block. Note
    /// that this flag is not yet functional.
    pub compute_pending_block: bool,
    /// The type responsible for yielding the best transactions for the payload if mempool
    /// transactions are allowed.
    pub best_transactions: Txs,
    /// This data availability configuration specifies constraints for the payload builder
    /// when assembling payloads
    pub da_config: OpDAConfig,
    pub verified_blockspace_capacity: u8,
    pub pbh_entry_point: Address,
    pub pbh_signature_aggregator: Address,

    /// Sets the private key of the builder
    /// used for signing the stampBlock transaction
    pub builder_private_key: String,

    pub flashblocks_state: FlashblocksStateExecutor,
}

impl FlashblocksPayloadBuilderBuilder {
    /// Create a new instance with the given `compute_pending_block` flag and data availability
    /// config.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        compute_pending_block: bool,
        verified_blockspace_capacity: u8,
        pbh_entry_point: Address,
        pbh_signature_aggregator: Address,
        builder_private_key: String,
        flashblocks_state: FlashblocksStateExecutor,
    ) -> Self {
        Self {
            compute_pending_block,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            best_transactions: (),
            builder_private_key,
            da_config: OpDAConfig::default(),
            flashblocks_state,
        }
    }

    /// Configure the data availability configuration for the OP payload builder.
    pub fn with_da_config(mut self, da_config: OpDAConfig) -> Self {
        self.da_config = da_config;
        self
    }
}

impl<Txs: OpPayloadTransactions<WorldChainPooledTransaction>>
    FlashblocksPayloadBuilderBuilder<Txs>
{
    /// Configures the type responsible for yielding the transactions that should be included in the
    /// payload.
    pub fn with_transactions<T>(self, best: T) -> FlashblocksPayloadBuilderBuilder<T> {
        let Self {
            compute_pending_block,
            da_config,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            builder_private_key,
            best_transactions: _,
            flashblocks_state,
        } = self;

        FlashblocksPayloadBuilderBuilder {
            compute_pending_block,
            da_config,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            builder_private_key,
            best_transactions: best,
            flashblocks_state,
        }
    }

    /// A helper method to initialize [`reth_optimism_payload_builder::OpPayloadBuilder`] with the
    /// given EVM config.
    #[allow(clippy::type_complexity)]
    pub fn build<Node, S>(
        self,
        evm_config: OpEvmConfig,
        ctx: &BuilderContext<Node>,
        pool: WorldChainTransactionPool<Node::Provider, S>,
    ) -> eyre::Result<
        FlashblocksPayloadBuilder<
            WorldChainTransactionPool<Node::Provider, S>,
            Node::Provider,
            WorldChainPayloadBuilderCtxBuilder<
                Node::Provider,
                WorldChainTransactionPool<Node::Provider, S>,
            >,
            Txs,
        >,
    >
    where
        Node: FullNodeTypes<Types: NodeTypes<ChainSpec = OpChainSpec, Primitives = OpPrimitives>>,

        Node::Provider: InMemoryState<Primitives = OpPrimitives>, // + FullNodeComponents<Evm = OpEvmConfig>,
        Node::Types: NodeTypes<ChainSpec = OpChainSpec>,
        S: BlobStore + Clone,
        Txs: OpPayloadTransactions<WorldChainPooledTransaction>,
    {
        let ctx_builder = WorldChainPayloadBuilderCtxBuilder {
            client: ctx.provider().clone(),
            pool: pool.clone(),
            verified_blockspace_capacity: self.verified_blockspace_capacity,
            pbh_entry_point: self.pbh_entry_point,
            pbh_signature_aggregator: self.pbh_signature_aggregator,
            builder_private_key: self
                .builder_private_key
                .parse()
                .context("Failed to parse builder private key")?,
        };

        self.flashblocks_state
            .launch::<_, _, WorldChainPooledTransaction>(
                ctx,
                ctx_builder.clone(),
                evm_config.clone(),
            );

        let payload_builder = FlashblocksPayloadBuilder {
            evm_config,
            pool,
            client: ctx.provider().clone(),
            // TODO: Allow overriding
            config: OpBuilderConfig {
                da_config: self.da_config,
            },
            best_transactions: self.best_transactions.clone(),
            ctx_builder,
        };

        Ok(payload_builder)
    }
}

impl<Node, S, Txs>
    PayloadBuilderBuilder<Node, WorldChainTransactionPool<Node::Provider, S>, OpEvmConfig>
    for FlashblocksPayloadBuilderBuilder<Txs>
where
    Node: FullNodeTypes<Types = WorldChainNode<FlashblocksContext>>,
    <Node as FullNodeTypes>::Provider:
        StateProviderFactory + ChainSpecProvider<ChainSpec: EthChainSpec + OpHardforks> + Clone,
    Node::Provider: InMemoryState<Primitives = OpPrimitives>, // + FullNodeComponents<Evm = OpEvmConfig>,
    S: BlobStore + Clone,
    Txs: OpPayloadTransactions<WorldChainPooledTransaction>,
{
    type PayloadBuilder = FlashblocksPayloadBuilder<
        WorldChainTransactionPool<Node::Provider, S>,
        Node::Provider,
        WorldChainPayloadBuilderCtxBuilder<
            Node::Provider,
            WorldChainTransactionPool<Node::Provider, S>,
        >,
        Txs,
    >;

    async fn build_payload_builder(
        self,
        ctx: &BuilderContext<Node>,
        pool: WorldChainTransactionPool<Node::Provider, S>,
        evm_config: OpEvmConfig,
    ) -> eyre::Result<Self::PayloadBuilder> {
        self.build(evm_config, ctx, pool)
    }
}
