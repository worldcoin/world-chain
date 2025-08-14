use std::sync::Arc;

use crate::args::WorldChainArgs;
use crate::flashblocks::rpc::WorldChainEngineApiBuilder;
use crate::flashblocks::service_builder::FlashblocksPayloadServiceBuilder;
use crate::node::WorldChainPoolBuilder;
use flashblocks_p2p::net::FlashblocksNetworkBuilder;
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use payload_builder_builder::FlashblocksPayloadBuilderBuilder;
use reth::builder::components::ComponentsBuilder;
use reth::builder::{FullNodeTypes, Node, NodeAdapter, NodeComponentsBuilder, NodeTypes};
use reth::rpc::compat::RpcTypes;
use reth_engine_local::LocalPayloadAttributesBuilder;
use reth_node_api::{FullNodeComponents, PayloadAttributesBuilder, PayloadTypes};
use reth_node_builder::components::PayloadServiceBuilder;
use reth_node_builder::DebugNode;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::args::RollupArgs;
use reth_optimism_node::node::{
    OpAddOns, OpConsensusBuilder, OpEngineValidatorBuilder, OpExecutorBuilder, OpNetworkBuilder,
};
use reth_optimism_node::{OpAddOnsBuilder, OpEngineTypes, OpEvmConfig};
use reth_optimism_payload_builder::config::OpDAConfig;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::eth::OpEthApiBuilder;
use reth_optimism_storage::OpStorage;
use reth_transaction_pool::blobstore::DiskFileBlobStore;
use reth_trie_db::MerklePatriciaTrie;
use rollup_boost::Authorization;
use world_chain_builder_flashblocks::primitives::FlashblocksState;
use world_chain_builder_pool::tx::WorldChainPooledTransaction;
use world_chain_builder_pool::WorldChainTransactionPool;

mod payload_builder_builder;
pub mod rpc;
pub mod service_builder;

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
    /// The flashblocks handler.
    pub flashblocks_handle: Option<FlashblocksHandle>,
    /// The flashblocks state.
    pub flashblocks_state: FlashblocksState,
    /// A watch channel notifier to the jobs generator.
    pub to_jobs_generator: tokio::sync::watch::Sender<Option<Authorization>>,
}

/// A [`ComponentsBuilder`] with its generic arguements set to a stack of World Chain Flashblocks specific builders.
pub type WorldChainFlashblocksNodeComponentBuilder<Node> = ComponentsBuilder<
    Node,
    WorldChainPoolBuilder,
    FlashblocksPayloadServiceBuilder<FlashblocksPayloadBuilderBuilder>,
    FlashblocksNetworkBuilder<OpNetworkBuilder>,
    OpExecutorBuilder,
    OpConsensusBuilder,
>;

impl WorldChainFlashblocksNode {
    /// Creates a new instance of the World Chain node type.
    pub fn new(
        args: WorldChainArgs,
        flashblocks_state: FlashblocksState,
        flashblocks_handle: Option<FlashblocksHandle>,
        to_jobs_generator: tokio::sync::watch::Sender<Option<Authorization>>,
    ) -> Self {
        Self {
            args,
            da_config: OpDAConfig::default(),
            flashblocks_state,
            flashblocks_handle,
            to_jobs_generator,
        }
    }

    /// Returns the [`WorldChainEngineApiBuilder`] for the World Chain node.
    pub fn engine_api_builder(&self) -> WorldChainEngineApiBuilder<OpEngineValidatorBuilder> {
        let Self {
            flashblocks_handle,
            flashblocks_state,
            to_jobs_generator,
            ..
        } = self;

        WorldChainEngineApiBuilder {
            engine_validator_builder: OpEngineValidatorBuilder::default(),
            flashblocks_handle: flashblocks_handle.clone(),
            flashblocks_state: Some(flashblocks_state.clone()),
            to_jobs_generator: Some(to_jobs_generator.clone()),
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
        Node: FullNodeTypes<Types = Self>,
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

        let op_network_builder = OpNetworkBuilder {
            disable_txpool_gossip,
            disable_discovery_v4: !discovery_v4,
        };
        let flashblocks_handle = self
            .flashblocks_handle
            .clone()
            .expect("Flashblocks handle is required");

        let fb_network_builder =
            FlashblocksNetworkBuilder::new(op_network_builder, flashblocks_handle.clone());

        ComponentsBuilder::default()
            .node_types::<Node>()
            .pool(WorldChainPoolBuilder::new(
                pbh_entrypoint,
                signature_aggregator,
                world_id,
            ))
            .executor(OpExecutorBuilder::default())
            .payload(FlashblocksPayloadServiceBuilder::new(
                FlashblocksPayloadBuilderBuilder::new(
                    compute_pending_block,
                    verified_blockspace_capacity,
                    pbh_entrypoint,
                    signature_aggregator,
                    builder_private_key.clone(),
                )
                .with_da_config(self.da_config.clone()),
                flashblocks_handle,
                self.flashblocks_state.clone(),
                self.to_jobs_generator.clone().subscribe(),
                flashblocks_args.flashblocks_authorization_enabled,
                flashblocks_args.flashblocks_builder_sk.clone(),
                flashblocks_args
                    .flashblocks_authorizer_sk
                    .as_ref()
                    .unwrap()
                    .clone(),
            ))
            .network(fb_network_builder)
            .executor(OpExecutorBuilder::default())
            .consensus(OpConsensusBuilder::default())
    }

    /// Returns [`OpAddOnsBuilder`] with configured arguments.
    pub fn add_ons_builder<NetworkT: RpcTypes>(&self) -> OpAddOnsBuilder<NetworkT> {
        reth_optimism_node::OpAddOnsBuilder::default()
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
    N: FullNodeTypes<Types = Self>,
    FlashblocksPayloadServiceBuilder<FlashblocksPayloadBuilderBuilder>: PayloadServiceBuilder<
        N,
        WorldChainTransactionPool<
            <N as FullNodeTypes>::Provider,
            DiskFileBlobStore,
            WorldChainPooledTransaction,
        >,
        OpEvmConfig<<<N as FullNodeTypes>::Types as NodeTypes>::ChainSpec>,
    >,
{
    type ComponentsBuilder = WorldChainFlashblocksNodeComponentBuilder<N>;

    type AddOns = OpAddOns<
        NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
        OpEthApiBuilder,
        OpEngineValidatorBuilder,
        WorldChainEngineApiBuilder<OpEngineValidatorBuilder>,
    >;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        Self::components(self)
    }

    fn add_ons(&self) -> Self::AddOns {
        self.add_ons_builder()
            .build::<_, OpEngineValidatorBuilder, WorldChainEngineApiBuilder<OpEngineValidatorBuilder>>()
            .with_engine_api(self.engine_api_builder())
    }
}

impl<N> DebugNode<N> for WorldChainFlashblocksNode
where
    N: FullNodeComponents<Types = Self>,
    FlashblocksPayloadServiceBuilder<FlashblocksPayloadBuilderBuilder>: PayloadServiceBuilder<
        N,
        WorldChainTransactionPool<
            <N as FullNodeTypes>::Provider,
            DiskFileBlobStore,
            WorldChainPooledTransaction,
        >,
        OpEvmConfig<<<N as FullNodeTypes>::Types as NodeTypes>::ChainSpec>,
    >,
{
    type RpcBlock = alloy_rpc_types_eth::Block<op_alloy_consensus::OpTxEnvelope>;

    fn rpc_to_primitive_block(rpc_block: Self::RpcBlock) -> reth_node_api::BlockTy<Self> {
        rpc_block.into_consensus()
    }

    fn local_payload_attributes_builder(
        chain_spec: &Self::ChainSpec,
    ) -> impl PayloadAttributesBuilder<<Self::Payload as PayloadTypes>::PayloadAttributes> {
        LocalPayloadAttributesBuilder::new(Arc::new(chain_spec.clone()))
    }
}

impl NodeTypes for WorldChainFlashblocksNode {
    type Primitives = OpPrimitives;
    type ChainSpec = OpChainSpec;
    type StateCommitment = MerklePatriciaTrie;
    type Storage = OpStorage;
    type Payload = OpEngineTypes;
}
