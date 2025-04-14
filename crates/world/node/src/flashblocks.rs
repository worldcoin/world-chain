use alloy_primitives::Address;
use reth::builder::components::{
    BasicPayloadServiceBuilder, ComponentsBuilder, PayloadBuilderBuilder,
};
use reth::builder::{
    BuilderContext, FullNodeTypes, Node, NodeAdapter, NodeComponentsBuilder, NodeTypes,
};

use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::args::RollupArgs;
use reth_optimism_node::node::{
    OpAddOns, OpConsensusBuilder, OpExecutorBuilder, OpNetworkBuilder, OpStorage,
};
use reth_optimism_node::{OpEngineTypes, OpEvmConfig};
use reth_optimism_payload_builder::builder::OpPayloadTransactions;
use reth_optimism_payload_builder::config::{OpBuilderConfig, OpDAConfig};
use reth_optimism_primitives::{OpBlock, OpPrimitives};

use reth_provider::{BlockReader, BlockReaderIdExt, StateProviderFactory};
use reth_transaction_pool::BlobStore;
use reth_trie_db::MerklePatriciaTrie;
use world_chain_builder_pool::tx::WorldChainPooledTransaction;
use world_chain_builder_pool::WorldChainTransactionPool;

use crate::args::WorldChainArgs;
use crate::node::WorldChainPoolBuilder;

/// Type configuration for a regular World Chain node.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct WorldChainFlashblocksNode {
    /// Additional World Chain args
    pub args: WorldChainArgs,
    /// Data availability configuration for the OP builder.
    ///
    /// Used to throttle the size of the data availability payloads (configured by the batcher via
    /// the `miner_` api).
    ///
    /// By default no throttling is applied.
    pub da_config: OpDAConfig,
}

impl WorldChainFlashblocksNode {
    /// Creates a new instance of the World Chain node type.
    pub fn new(args: WorldChainArgs) -> Self {
        Self {
            args,
            da_config: OpDAConfig::default(),
        }
    }

    /// Configure the data availability configuration for the OP builder.
    pub fn with_da_config(mut self, da_config: OpDAConfig) -> Self {
        self.da_config = da_config;
        self
    }

    /// Returns the components for the given [`RollupArgs`].
    pub fn components<Node>(
        &self,
    ) -> ComponentsBuilder<
        Node,
        WorldChainPoolBuilder,
        BasicPayloadServiceBuilder<FlashblocksPayloadBuilder>,
        OpNetworkBuilder,
        OpExecutorBuilder,
        OpConsensusBuilder,
    >
    where
        Node: FullNodeTypes<Types: NodeTypes<ChainSpec = OpChainSpec, Primitives = OpPrimitives>>,
    {
        let WorldChainArgs {
            rollup_args,
            verified_blockspace_capacity,
            pbh_entrypoint,
            signature_aggregator,
            world_id,
            builder_private_key,
            flashblock_args: _,
        } = self.args.clone();

        let RollupArgs {
            disable_txpool_gossip,
            compute_pending_block,
            discovery_v4,
            ..
        } = rollup_args;

        ComponentsBuilder::default()
            .node_types::<Node>()
            .pool(WorldChainPoolBuilder::new(
                pbh_entrypoint,
                signature_aggregator,
                world_id,
            ))
            .payload(BasicPayloadServiceBuilder::new(
                FlashblocksPayloadBuilder::new(
                    compute_pending_block,
                    verified_blockspace_capacity,
                    pbh_entrypoint,
                    signature_aggregator,
                    builder_private_key.clone(),
                )
                .with_da_config(self.da_config.clone()),
            ))
            .network(OpNetworkBuilder {
                disable_txpool_gossip,
                disable_discovery_v4: !discovery_v4,
            })
            .executor(OpExecutorBuilder::default())
            .consensus(OpConsensusBuilder::default())
    }
}

impl<N> Node<N> for WorldChainFlashblocksNode
where
    N: FullNodeTypes<
        Types: NodeTypes<ChainSpec = OpChainSpec, Primitives = OpPrimitives, Storage = OpStorage>,
    >,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        WorldChainPoolBuilder,
        BasicPayloadServiceBuilder<FlashblocksPayloadBuilder>,
        OpNetworkBuilder,
        OpExecutorBuilder,
        OpConsensusBuilder,
    >;

    type AddOns =
        OpAddOns<NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>>;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        Self::components(self)
    }

    fn add_ons(&self) -> Self::AddOns {
        Self::AddOns::builder()
            .with_sequencer(self.args.rollup_args.sequencer_http.clone())
            .with_da_config(self.da_config.clone())
            .build()
    }
}

impl NodeTypes for WorldChainFlashblocksNode {
    type Primitives = OpPrimitives;
    type ChainSpec = OpChainSpec;
    type StateCommitment = MerklePatriciaTrie;
    type Storage = OpStorage;
    type Payload = OpEngineTypes;
}

// TODO: NOTE: update this struct
/// A basic World Chain payload service builder
#[derive(Debug, Default, Clone)]
pub struct FlashblocksPayloadBuilder<Txs = ()> {
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

impl FlashblocksPayloadBuilder {
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

impl<Txs> FlashblocksPayloadBuilder<Txs> {
    /// Configures the type responsible for yielding the transactions that should be included in the
    /// payload.
    pub fn with_transactions<T>(self, best_transactions: T) -> FlashblocksPayloadBuilder<T> {
        let Self {
            compute_pending_block,
            da_config,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            builder_private_key,
            ..
        } = self;

        FlashblocksPayloadBuilder {
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
    ) -> eyre::Result<
        world_chain_builder_payload::builder::WorldChainPayloadBuilder<Node::Provider, S, Txs>,
    >
    where
        Node: FullNodeTypes<Types: NodeTypes<ChainSpec = OpChainSpec, Primitives = OpPrimitives>>,
        S: BlobStore + Clone,
        Txs: OpPayloadTransactions<WorldChainPooledTransaction>,
    {
        let payload_builder =
            world_chain_builder_payload::builder::WorldChainPayloadBuilder::with_builder_config(
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
    for FlashblocksPayloadBuilder<Txs>
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = OpChainSpec, Primitives = OpPrimitives>>,
    Node::Provider: StateProviderFactory + BlockReaderIdExt + BlockReader<Block = OpBlock>,
    S: BlobStore + Clone,
    Txs: OpPayloadTransactions<WorldChainPooledTransaction>,
{
    // TODO: NOTE: update this toflashblocks payload builder
    type PayloadBuilder =
        world_chain_builder_payload::builder::WorldChainPayloadBuilder<Node::Provider, S, Txs>;

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
