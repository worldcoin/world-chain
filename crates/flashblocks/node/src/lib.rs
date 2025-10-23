use std::{sync::Arc, time::Duration};

use ed25519_dalek::VerifyingKey;
use flashblocks_builder::{
    executor::FlashblocksStateExecutor, traits::context::OpPayloadBuilderCtxBuilder,
};
use flashblocks_cli::FlashblocksArgs;
use flashblocks_p2p::{net::FlashblocksNetworkBuilder, protocol::handler::FlashblocksHandle};
use flashblocks_primitives::p2p::Authorization;
use flashblocks_rpc::eth::FlashblocksEthApiBuilder;
use op_alloy_consensus::OpTxEnvelope;
use reth::chainspec::EthChainSpec;
use reth_engine_local::LocalPayloadAttributesBuilder;
use reth_node_api::{
    FullNodeComponents, FullNodeTypes, NodeTypes, PayloadAttributesBuilder, PayloadTypes,
};
use reth_node_builder::{
    components::ComponentsBuilder,
    rpc::{BasicEngineValidatorBuilder, RpcAddOns},
    DebugNode, Node, NodeAdapter, NodeComponentsBuilder,
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{
    args::RollupArgs, txpool::OpPooledTx, OpAddOns, OpBuiltPayload, OpConsensusBuilder, OpDAConfig,
    OpEngineTypes, OpEngineValidatorBuilder, OpExecutorBuilder, OpFullNodeTypes, OpNetworkBuilder,
    OpNodeTypes, OpPayloadBuilderAttributes, OpPoolBuilder, OpStorage,
};
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::OpEthApiBuilder;
use reth_provider::{
    ChainSpecProvider, DatabaseProviderFactory, HeaderProvider, StateProviderFactory,
};
use reth_transaction_pool::{PoolTransaction, TransactionPool};

use crate::{
    engine::FlashblocksEngineApiBuilder, payload::FlashblocksPayloadBuilderBuilder,
    payload_service::FlashblocksPayloadServiceBuilder,
};

pub mod engine;
pub mod payload;
pub mod payload_service;

/// Helper trait used for flashblocks reth components
pub trait FlashblocksFullNodeTypes:
    FullNodeTypes<Provider: FlashblocksProvider, Types: FlashblocksNodeTypes>
{
}

impl<T> FlashblocksFullNodeTypes for T where
    T: FullNodeTypes<Provider: FlashblocksProvider, Types: FlashblocksNodeTypes>
{
}

/// Helper trait used for flashblocks reth components
pub trait FlashblocksNodeTypes:
    NodeTypes<
    Payload: PayloadTypes<
        BuiltPayload = OpBuiltPayload,
        PayloadBuilderAttributes = OpPayloadBuilderAttributes<op_alloy_consensus::OpTxEnvelope>,
    >,
    ChainSpec = OpChainSpec,
>
{
}

impl<T> FlashblocksNodeTypes for T where
    T: NodeTypes<
        Payload: PayloadTypes<
            BuiltPayload = OpBuiltPayload,
            PayloadBuilderAttributes = OpPayloadBuilderAttributes<op_alloy_consensus::OpTxEnvelope>,
        >,
        ChainSpec = OpChainSpec,
    >
{
}

/// Helper trait used for flashblocks reth components
pub trait FlashblocksProvider:
    StateProviderFactory
    + ChainSpecProvider<ChainSpec: EthChainSpec + OpHardforks>
    + Clone
    + DatabaseProviderFactory<Provider: HeaderProvider<Header = alloy_consensus::Header>>
{
}

impl<T> FlashblocksProvider for T where
    T: StateProviderFactory
        + ChainSpecProvider<ChainSpec: EthChainSpec + OpHardforks>
        + Clone
        + DatabaseProviderFactory<Provider: HeaderProvider<Header = alloy_consensus::Header>>
{
}

pub trait FlashblocksPool:
    TransactionPool<Transaction: OpPooledTx + PoolTransaction<Consensus = OpTxEnvelope>>
    + Unpin
    + 'static
{
}

/// Type configuration for a regular Optimism node.
#[derive(Debug, Clone)]
pub struct FlashblocksNodeBuilder {
    /// Optimism args
    pub rollup: RollupArgs,

    /// Flashblocks Args
    pub flashblocks: FlashblocksArgs,

    /// Data availability configuration for the OP builder.
    ///
    /// Used to throttle the size of the data availability payloads (configured by the batcher via
    /// the `miner_` api).
    ///
    /// By default no throttling is applied.
    pub da_config: OpDAConfig,
}

impl FlashblocksNodeBuilder {
    pub fn build(self) -> FlashblocksNode {
        let authorizer_vk = self.flashblocks.authorizer_vk.unwrap_or(
            self.flashblocks
                .builder_sk
                .as_ref()
                .expect("flashblocks builder_sk required")
                .verifying_key(),
        );
        let builder_sk = self.flashblocks.builder_sk.clone();
        let flashblocks_handle = FlashblocksHandle::new(authorizer_vk, builder_sk.clone());

        let (pending_block, _) = tokio::sync::watch::channel(None);

        let flashblocks_state = FlashblocksStateExecutor::new(
            flashblocks_handle.clone(),
            self.da_config.clone(),
            pending_block,
        );

        let (to_jobs_generator, _) = tokio::sync::watch::channel(None);

        FlashblocksNode {
            rollup: self.rollup,
            flashblocks: self.flashblocks,
            da_config: self.da_config,
            flashblocks_state,
            flashblocks_handle,
            to_jobs_generator,
            authorizer_vk,
        }
    }
}

/// Type configuration for a regular Optimism node.
#[derive(Debug, Clone)]
pub struct FlashblocksNode {
    /// Optimism args
    pub rollup: RollupArgs,

    /// Flashblocks Args
    pub flashblocks: FlashblocksArgs,

    /// Data availability configuration for the OP builder.
    ///
    /// Used to throttle the size of the data availability payloads (configured by the batcher via
    /// the `miner_` api).
    ///
    /// By default no throttling is applied.
    pub da_config: OpDAConfig,

    pub flashblocks_handle: FlashblocksHandle,
    pub flashblocks_state: FlashblocksStateExecutor,
    pub to_jobs_generator: tokio::sync::watch::Sender<Option<Authorization>>,
    pub authorizer_vk: VerifyingKey,
}

/// A [`ComponentsBuilder`] with its generic arguments set to a stack of Optimism specific builders.
pub type FlashblocksNodeComponentBuilder<Node> = ComponentsBuilder<
    Node,
    OpPoolBuilder,
    FlashblocksPayloadServiceBuilder<FlashblocksPayloadBuilderBuilder<OpPayloadBuilderCtxBuilder>>,
    FlashblocksNetworkBuilder<OpNetworkBuilder>,
    OpExecutorBuilder,
    OpConsensusBuilder,
>;

impl<N> Node<N> for FlashblocksNode
where
    N: FullNodeTypes<Types: OpFullNodeTypes + OpNodeTypes>,

    N: FullNodeTypes,
    N::Provider: StateProviderFactory
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + HeaderProvider<Header = alloy_consensus::Header>
        + Clone
        + DatabaseProviderFactory<Provider: HeaderProvider<Header = alloy_consensus::Header>>,
    N::Types: NodeTypes<
        ChainSpec = OpChainSpec,
        Payload: PayloadTypes<
            BuiltPayload = OpBuiltPayload,
            PayloadBuilderAttributes = OpPayloadBuilderAttributes<op_alloy_consensus::OpTxEnvelope>,
        >,
    >,
{
    type ComponentsBuilder = FlashblocksNodeComponentBuilder<N>;

    type AddOns = OpAddOns<
        NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
        FlashblocksEthApiBuilder,
        OpEngineValidatorBuilder,
        FlashblocksEngineApiBuilder<OpEngineValidatorBuilder>,
        BasicEngineValidatorBuilder<OpEngineValidatorBuilder>,
    >;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        let RollupArgs {
            disable_txpool_gossip,
            discovery_v4,
            ..
        } = self.rollup;

        let op_network_builder = OpNetworkBuilder {
            disable_txpool_gossip,
            disable_discovery_v4: !discovery_v4,
        };

        let fb_network_builder =
            FlashblocksNetworkBuilder::new(op_network_builder, self.flashblocks_handle.clone());

        ComponentsBuilder::default()
            .node_types::<N>()
            .pool(
                OpPoolBuilder::default()
                    .with_enable_tx_conditional(self.rollup.enable_tx_conditional)
                    .with_supervisor(
                        self.rollup.supervisor_http.clone(),
                        self.rollup.supervisor_safety_level,
                    ),
            )
            .executor(OpExecutorBuilder::default())
            .payload(FlashblocksPayloadServiceBuilder::new(
                FlashblocksPayloadBuilderBuilder::new(
                    OpPayloadBuilderCtxBuilder,
                    self.flashblocks_state.clone(),
                    self.da_config.clone(),
                ),
                self.flashblocks_handle.clone(),
                self.flashblocks_state.clone(),
                self.to_jobs_generator.clone().subscribe(),
                Duration::from_millis(self.flashblocks.flashblocks_interval),
                Duration::from_millis(self.flashblocks.recommit_interval),
            ))
            .network(fb_network_builder)
            .consensus(OpConsensusBuilder::default())
    }

    fn add_ons(&self) -> Self::AddOns {
        let pending_block_rx = self.flashblocks_state.pending_block();
        let engine_api_builder = FlashblocksEngineApiBuilder {
            engine_validator_builder: Default::default(),
            flashblocks_handle: Some(self.flashblocks_handle.clone()),
            to_jobs_generator: self.to_jobs_generator.clone(),
            authorizer_vk: self.authorizer_vk,
            pending_block_rx,
        };
        let op_eth_api_builder =
            OpEthApiBuilder::default().with_sequencer(self.rollup.sequencer.clone());

        let flashblocks_eth_api_builder = FlashblocksEthApiBuilder::new(
            op_eth_api_builder,
            self.flashblocks_state.pending_block(),
        );

        let rpc_add_ons = RpcAddOns::new(
            flashblocks_eth_api_builder,
            Default::default(),
            engine_api_builder,
            Default::default(),
            Default::default(),
        );

        OpAddOns::new(
            rpc_add_ons,
            self.da_config.clone(),
            self.rollup.sequencer.clone(),
            Default::default(),
            Default::default(),
            false,
            1_000_000,
        )
    }
}

impl<N> DebugNode<N> for FlashblocksNode
where
    N: FullNodeComponents,
    N: FullNodeTypes<Types: OpFullNodeTypes + OpNodeTypes>,
    N: FullNodeTypes,
    N::Provider: StateProviderFactory
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + HeaderProvider<Header = alloy_consensus::Header>
        + Clone
        + DatabaseProviderFactory<Provider: HeaderProvider<Header = alloy_consensus::Header>>,
    N::Types: NodeTypes<
        ChainSpec = OpChainSpec,
        Payload: PayloadTypes<
            BuiltPayload = OpBuiltPayload,
            PayloadBuilderAttributes = OpPayloadBuilderAttributes<op_alloy_consensus::OpTxEnvelope>,
        >,
    >,
{
    type RpcBlock = alloy_rpc_types_eth::Block<OpTxEnvelope>;

    fn rpc_to_primitive_block(rpc_block: Self::RpcBlock) -> reth_node_api::BlockTy<Self> {
        rpc_block.into_consensus()
    }

    fn local_payload_attributes_builder(
        chain_spec: &Self::ChainSpec,
    ) -> impl PayloadAttributesBuilder<<Self::Payload as PayloadTypes>::PayloadAttributes> {
        LocalPayloadAttributesBuilder::new(Arc::new(chain_spec.clone()))
    }
}

impl NodeTypes for FlashblocksNode {
    type Primitives = OpPrimitives;
    type ChainSpec = OpChainSpec;
    type Storage = OpStorage;
    type Payload = OpEngineTypes;
}
