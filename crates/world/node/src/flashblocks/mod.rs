use crate::args::WorldChainArgs;
use crate::node::WorldChainPoolBuilder;
use payload_builder_builder::FlashblocksPayloadBuilderBuilder;
use reth::builder::components::{BasicPayloadServiceBuilder, ComponentsBuilder};
use reth::builder::{FullNodeTypes, Node, NodeAdapter, NodeComponentsBuilder, NodeTypes};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::args::RollupArgs;
use reth_optimism_node::node::{
    OpAddOns, OpConsensusBuilder, OpExecutorBuilder, OpNetworkBuilder, OpStorage,
};
use reth_optimism_node::OpEngineTypes;
use reth_optimism_payload_builder::config::OpDAConfig;
use reth_optimism_primitives::OpPrimitives;
use reth_trie_db::MerklePatriciaTrie;
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
        BasicPayloadServiceBuilder<FlashblocksPayloadBuilderBuilder>,
        OpNetworkBuilder,
        OpExecutorBuilder,
        OpConsensusBuilder,
    >
    where
        Node: FullNodeTypes<
            Types: NodeTypes<
                Payload = OpEngineTypes,
                ChainSpec = OpChainSpec,
                Primitives = OpPrimitives,
            >,
        >,
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

        let flashblock_args = self.args.flashblock_args.as_ref().unwrap();

        // TODO: Connect to a flashblock receiver & WS streaming service
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        ComponentsBuilder::default()
            .node_types::<Node>()
            .pool(WorldChainPoolBuilder::new(
                pbh_entrypoint,
                signature_aggregator,
                world_id,
            ))
            .payload(BasicPayloadServiceBuilder::new(
                FlashblocksPayloadBuilderBuilder::new(
                    compute_pending_block,
                    verified_blockspace_capacity,
                    pbh_entrypoint,
                    signature_aggregator,
                    builder_private_key.clone(),
                    flashblock_args.flashblock_block_time,
                    flashblock_args.flashblock_interval,
                    tx,
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
    type ComponentsBuilder = ComponentsBuilder<
        N,
        WorldChainPoolBuilder,
        BasicPayloadServiceBuilder<FlashblocksPayloadBuilderBuilder>,
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
