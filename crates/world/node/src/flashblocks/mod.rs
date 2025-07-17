use crate::args::WorldChainArgs;
use crate::node::WorldChainPoolBuilder;
use payload_builder_builder::FlashblocksPayloadBuilderBuilder;
use reth::builder::components::{BasicPayloadServiceBuilder, ComponentsBuilder};
use reth::builder::{FullNodeTypes, Node, NodeAdapter, NodeComponentsBuilder, NodeTypes};
use reth_node_builder::components::PayloadServiceBuilder;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::args::RollupArgs;
use reth_optimism_node::node::{
    OpAddOns, OpConsensusBuilder, OpEngineValidatorBuilder, OpExecutorBuilder, OpNetworkBuilder,
    OpNodeTypes,
};
use reth_optimism_node::{OpEngineApiBuilder, OpEngineTypes, OpEvmConfig};
use reth_optimism_payload_builder::config::OpDAConfig;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::eth::OpEthApiBuilder;
use reth_optimism_storage::OpStorage;
use reth_transaction_pool::blobstore::DiskFileBlobStore;
use reth_trie_db::MerklePatriciaTrie;
use tokio::sync::mpsc;
use world_chain_builder_pool::tx::WorldChainPooledTransaction;
use world_chain_builder_pool::WorldChainTransactionPool;
mod payload_builder_builder;

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

/// A [`ComponentsBuilder`] with its generic arguements set to a stack of World Chain Flashblocks specific builders.
pub type WorldChainFlashblocksNodeComponentBuilder<
    Node,
    Payload = FlashblocksPayloadBuilderBuilder,
> = ComponentsBuilder<
    Node,
    WorldChainPoolBuilder,
    BasicPayloadServiceBuilder<Payload>,
    OpNetworkBuilder,
    OpExecutorBuilder,
    OpConsensusBuilder,
>;

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
    pub fn components<Node>(&self) -> WorldChainFlashblocksNodeComponentBuilder<Node>
    where
        Node: FullNodeTypes<Types: OpNodeTypes>,
        BasicPayloadServiceBuilder<FlashblocksPayloadBuilderBuilder>: PayloadServiceBuilder<
            Node,
            WorldChainTransactionPool<
                <Node as FullNodeTypes>::Provider,
                DiskFileBlobStore,
                WorldChainPooledTransaction,
            >,
            OpEvmConfig<<<Node as FullNodeTypes>::Types as NodeTypes>::ChainSpec>,
        >,
    {
        let (flashblocks_publish_tx, flashblocks_publish_rx) = mpsc::unbounded_channel();
        // inbound flashblocks, for use with the node overlay
        // let (flashblocks_tx, flashblocks_rx) = tokio::sync::mpsc::channel(100);

        let WorldChainArgs {
            rollup_args,
            verified_blockspace_capacity,
            pbh_entrypoint,
            signature_aggregator,
            world_id,
            builder_private_key,
            flashblocks_args: _,
        } = self.args.clone();

        let RollupArgs {
            disable_txpool_gossip,
            compute_pending_block,
            discovery_v4,
            ..
        } = rollup_args;

        let flashblocks_args = self.args.flashblocks_args.as_ref().unwrap();

        ComponentsBuilder::default()
            .node_types::<Node>()
            .pool(WorldChainPoolBuilder::new(
                pbh_entrypoint,
                signature_aggregator,
                world_id,
            ))
            .executor(OpExecutorBuilder::default())
            .payload(BasicPayloadServiceBuilder::new(
                FlashblocksPayloadBuilderBuilder::new(
                    compute_pending_block,
                    verified_blockspace_capacity,
                    pbh_entrypoint,
                    signature_aggregator,
                    builder_private_key.clone(),
                    flashblocks_args.flashblock_block_time,
                    flashblocks_args.flashblock_interval,
                    (
                        flashblocks_args.flashblock_host,
                        flashblocks_args.flashblock_port,
                    ),
                    flashblocks_publish_tx,
                    flashblocks_args.flashblocks_authorizor_vk,
                    flashblocks_args.flashblocks_builder_sk.clone(),
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
        Types: NodeTypes<
            Payload = OpEngineTypes,
            ChainSpec = OpChainSpec,
            Primitives = OpPrimitives,
            Storage = OpStorage,
        >,
    >,
{
    type ComponentsBuilder = WorldChainFlashblocksNodeComponentBuilder<N>;

    type AddOns = OpAddOns<
        NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
        OpEthApiBuilder,
        OpEngineValidatorBuilder,
        OpEngineApiBuilder<OpEngineValidatorBuilder>,
    >;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        Self::components(self)
    }

    fn add_ons(&self) -> Self::AddOns {
        Self::AddOns::builder()
            .with_sequencer(self.args.rollup_args.sequencer.clone())
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
