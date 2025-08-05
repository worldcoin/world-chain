use crate::args::WorldChainArgs;
use crate::flashblocks::rpc::WorldChainEngineApiBuilder;
use crate::flashblocks::service_builder::FlashblocksPayloadServiceBuilder;
use crate::node::WorldChainPoolBuilder;
use flashblocks::rpc::engine::FlashblocksState;
use flashblocks_p2p::net::FlashblocksNetworkBuilder;
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use payload_builder_builder::FlashblocksPayloadBuilderBuilder;
use reth::builder::components::ComponentsBuilder;
use reth::builder::{FullNodeTypes, Node, NodeAdapter, NodeComponentsBuilder, NodeTypes};
use reth::rpc::compat::RpcTypes;
use reth_node_api::FullNodeComponents;
use reth_node_builder::components::PayloadServiceBuilder;
use reth_node_builder::rpc::{EthApiBuilder, Identity};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::args::RollupArgs;
use reth_optimism_node::node::{
    OpAddOns, OpConsensusBuilder, OpEngineValidatorBuilder, OpExecutorBuilder, OpNetworkBuilder,
    OpNodeTypes,
};
use reth_optimism_node::{OpAddOnsBuilder, OpEngineApiBuilder, OpEngineTypes, OpEvmConfig};
use reth_optimism_payload_builder::config::OpDAConfig;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::eth::OpEthApiBuilder;
use reth_optimism_storage::OpStorage;
use reth_transaction_pool::blobstore::DiskFileBlobStore;
use reth_trie_db::MerklePatriciaTrie;

use world_chain_builder_pool::tx::WorldChainPooledTransaction;
use world_chain_builder_pool::WorldChainTransactionPool;

mod payload_builder_builder;
mod rpc;
pub mod service_builder;

/// Type configuration for a regular World Chain node.
#[derive(Debug, Clone)]
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
    /// The flashblocks handler.
    pub flashblocks_handle: FlashblocksHandle,
}

/// A [`ComponentsBuilder`] with its generic arguements set to a stack of World Chain Flashblocks specific builders.
pub type WorldChainFlashblocksNodeComponentBuilder<
    Node,
    Payload = FlashblocksPayloadBuilderBuilder,
> = ComponentsBuilder<
    Node,
    WorldChainPoolBuilder,
    FlashblocksPayloadServiceBuilder<Payload>,
    FlashblocksNetworkBuilder<OpNetworkBuilder>,
    OpExecutorBuilder,
    OpConsensusBuilder,
>;

impl WorldChainFlashblocksNode {
    /// Creates a new instance of the World Chain node type.
    pub fn new(args: WorldChainArgs, flashblocks_handle: FlashblocksHandle) -> Self {
        Self {
            args,
            da_config: OpDAConfig::default(),
            flashblocks_handle,
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
        FlashblocksPayloadServiceBuilder<FlashblocksPayloadBuilderBuilder>: PayloadServiceBuilder<
            Node,
            WorldChainTransactionPool<
                <Node as FullNodeTypes>::Provider,
                DiskFileBlobStore,
                WorldChainPooledTransaction,
            >,
            OpEvmConfig<<<Node as FullNodeTypes>::Types as NodeTypes>::ChainSpec>,
        >,
    {
        let Self {
            args:
                WorldChainArgs {
                    rollup_args,
                    verified_blockspace_capacity,
                    pbh_entrypoint,
                    signature_aggregator,
                    world_id,
                    builder_private_key,
                    flashblocks_args: _,
                },
            flashblocks_handle,
            ..
        } = self;

        let RollupArgs {
            disable_txpool_gossip,
            compute_pending_block,
            discovery_v4,
            ..
        } = rollup_args;

        let flashblocks_args = self.args.flashblocks_args.as_ref().unwrap();
        let op_network_builder = OpNetworkBuilder {
            disable_txpool_gossip: *disable_txpool_gossip,
            disable_discovery_v4: !discovery_v4,
        };

        let authorizer_vk = flashblocks_args
            .flashblocks_authorizor_vk
            .unwrap_or(flashblocks_args.flashblocks_builder_sk.verifying_key());

        let fb_network_builder =
            FlashblocksNetworkBuilder::new(op_network_builder, flashblocks_handle.clone());

        let flashblocks_state = FlashblocksState::default();

        ComponentsBuilder::default()
            .node_types::<Node>()
            .pool(WorldChainPoolBuilder::new(
                *pbh_entrypoint,
                *signature_aggregator,
                *world_id,
            ))
            .executor(OpExecutorBuilder::default())
            .payload(FlashblocksPayloadServiceBuilder::new(
                FlashblocksPayloadBuilderBuilder::new(
                    *compute_pending_block,
                    *verified_blockspace_capacity,
                    *pbh_entrypoint,
                    *signature_aggregator,
                    builder_private_key.clone(),
                    flashblocks_args.flashblock_block_time,
                    flashblocks_args.flashblock_interval,
                    (
                        flashblocks_args.flashblock_host,
                        flashblocks_args.flashblock_port,
                    ),
                    flashblocks_handle.flashblocks_tx(),
                    authorizer_vk,
                    flashblocks_args.flashblocks_builder_sk.clone(),
                    flashblocks_handle.clone(),
                    flashblocks_state,
                )
                .with_da_config(self.da_config.clone()),
                authorizer_vk,
                flashblocks_args.flashblocks_builder_sk.clone(),
                flashblocks_handle.clone(),
            ))
            .network(fb_network_builder)
            .consensus(OpConsensusBuilder::default())
    }

    /// Returns [``] with configured arguments.
    pub fn add_ons_builder<NetworkT: RpcTypes>(&self) -> OpAddOnsBuilder<NetworkT> {
        // TODO: Update this.
        OpAddOnsBuilder::default()
            .with_sequencer(self.args.rollup_args.sequencer.clone())
            .with_sequencer_headers(self.args.rollup_args.sequencer_headers.clone())
            .with_da_config(self.da_config.clone())
            .with_enable_tx_conditional(self.args.rollup_args.enable_tx_conditional)
            .with_min_suggested_priority_fee(self.args.rollup_args.min_suggested_priority_fee)
            .with_historical_rpc(self.args.rollup_args.historical_rpc.clone())
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
        self.add_ons_builder().build()
    }
}

impl NodeTypes for WorldChainFlashblocksNode {
    type Primitives = OpPrimitives;
    type ChainSpec = OpChainSpec;
    type StateCommitment = MerklePatriciaTrie;
    type Storage = OpStorage;
    type Payload = OpEngineTypes;
}

/// Add-ons w.r.t World Chain
#[derive(Debug)]
pub struct WorldChainAddOns<
    N: FullNodeComponents,
    EthB: EthApiBuilder<N>,
    EV,
    EB,
    RpcMiddleware = Identity,
> {
    inner: OpAddOns<N, EthB, EV, EB, RpcMiddleware>,
}

impl<N: FullNodeComponents<Types: NodeTypes>> Default
    for WorldChainAddOns<
        N,
        OpEthApiBuilder,
        OpEngineValidatorBuilder,
        WorldChainEngineApiBuilder<OpEngineValidatorBuilder>,
        Identity,
    >
where
    OpEthApiBuilder: EthApiBuilder<N>,
{
    fn default() -> Self {
        todo!()
    }
}
