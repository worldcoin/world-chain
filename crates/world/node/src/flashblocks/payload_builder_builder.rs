use alloy_primitives::Address;
use flashblocks::builder::FlashblocksPayloadBuilder;
use reth::builder::components::PayloadBuilderBuilder;
use reth::builder::{BuilderContext, FullNodeTypes, NodeTypes};
use reth::chainspec::EthChainSpec;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::OpEvmConfig;
use reth_optimism_payload_builder::builder::OpPayloadTransactions;
use reth_optimism_payload_builder::config::{OpBuilderConfig, OpDAConfig};
use reth_optimism_primitives::OpPrimitives;
use reth_provider::{ChainSpecProvider, StateProviderFactory};
use reth_transaction_pool::BlobStore;
use world_chain_builder_payload::builder::WorldChainPayloadBuilder;
use world_chain_builder_payload::ctx::WorldChainPayloadBuilderCtx;
use world_chain_builder_pool::tx::WorldChainPooledTransaction;
use world_chain_builder_pool::WorldChainTransactionPool;

// TODO: NOTE: update this struct
/// A basic World Chain payload service builder
#[derive(Debug, Default, Clone)]
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
}

impl FlashblocksPayloadBuilderBuilder {
    /// Create a new instance with the given `compute_pending_block` flag and data availability
    /// config.
    pub fn new(
        compute_pending_block: bool,
        verified_blockspace_capacity: u8,
        pbh_entry_point: Address,
        pbh_signature_aggregator: Address,
        builder_private_key: String,
    ) -> Self {
        Self {
            compute_pending_block,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            best_transactions: (),
            builder_private_key,
            da_config: OpDAConfig::default(),
        }
    }

    /// Configure the data availability configuration for the OP payload builder.
    pub fn with_da_config(mut self, da_config: OpDAConfig) -> Self {
        self.da_config = da_config;
        self
    }
}

impl<Txs> FlashblocksPayloadBuilderBuilder<Txs> {
    /// Configures the type responsible for yielding the transactions that should be included in the
    /// payload.
    pub fn with_transactions<T>(self, best_transactions: T) -> FlashblocksPayloadBuilderBuilder<T> {
        let Self {
            compute_pending_block,
            da_config,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            builder_private_key,
            ..
        } = self;

        FlashblocksPayloadBuilderBuilder {
            compute_pending_block,
            da_config,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            best_transactions,
            builder_private_key,
        }
    }

    /// A helper method to initialize [`reth_optimism_payload_builder::OpPayloadBuilder`] with the
    /// given EVM config.
    pub fn build<Node, S>(
        &self,
        evm_config: OpEvmConfig,
        ctx: &BuilderContext<Node>,
        pool: WorldChainTransactionPool<Node::Provider, S>,
    ) -> eyre::Result<WorldChainPayloadBuilder<Node::Provider, S, Txs>>
    where
        Node: FullNodeTypes<Types: NodeTypes<ChainSpec = OpChainSpec, Primitives = OpPrimitives>>,
        S: BlobStore + Clone,
        Txs: OpPayloadTransactions<WorldChainPooledTransaction>,
    {
        let payload_builder = WorldChainPayloadBuilder::with_builder_config(
            pool,
            ctx.provider().clone(),
            evm_config,
            OpBuilderConfig {
                da_config: self.da_config.clone(),
            },
            self.compute_pending_block,
            self.verified_blockspace_capacity,
            self.pbh_entry_point,
            self.pbh_signature_aggregator,
            self.builder_private_key.clone(),
        )
        .with_transactions(self.best_transactions.clone());

        Ok(payload_builder)
    }
}

impl<Node, S, Txs> PayloadBuilderBuilder<Node, WorldChainTransactionPool<Node::Provider, S>>
    for FlashblocksPayloadBuilderBuilder<Txs>
// where
//     Node: FullNodeTypes<Types: NodeTypes<ChainSpec = OpChainSpec, Primitives = OpPrimitives>>,
//     Node::Provider: StateProviderFactory + BlockReaderIdExt + BlockReader<Block = OpBlock>,
//     S: BlobStore + Clone,
//     Txs: OpPayloadTransactions<WorldChainPooledTransaction>,
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = OpChainSpec, Primitives = OpPrimitives>>,
    <Node as FullNodeTypes>::Provider:
        StateProviderFactory + ChainSpecProvider<ChainSpec: EthChainSpec + OpHardforks> + Clone,
    S: BlobStore + Clone,
    // Pool: TransactionPool<
    //     Transaction: MaybeInteropTransaction + PoolTransaction<Consensus = OpPrimitives::SignedTx>,
    // >,
    // Evm: reth_evm::Evm
    //     + EthereumHardforks
    //     + ConfigureEvm<Primitives = N, NextBlockEnvCtx = OpNextBlockEnvAttributes>,
    Txs: OpPayloadTransactions<WorldChainPooledTransaction>,
{
    // TODO: NOTE: update this toflashblocks payload builder
    type PayloadBuilder = FlashblocksPayloadBuilder<
        WorldChainTransactionPool<Node::Provider, S>,
        Node::Provider,
        OpEvmConfig,
        WorldChainPayloadBuilderCtx<Node::Provider, WorldChainTransactionPool<Node::Provider, S>>,
        Txs,
    >;

    async fn build_payload_builder(
        self,
        ctx: &BuilderContext<Node>,
        pool: WorldChainTransactionPool<Node::Provider, S>,
    ) -> eyre::Result<Self::PayloadBuilder> {
        self.build(
            OpEvmConfig::new(ctx.chain_spec(), Default::default()),
            ctx,
            pool,
        )
    }
}
