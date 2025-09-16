use flashblocks_builder::traits::context::OpPayloadBuilderCtxBuilder;
use flashblocks_cli::FlashblocksArgs;
use flashblocks_p2p::net::FlashblocksNetworkBuilder;
use flashblocks_provider::InMemoryState;
use flashblocks_rpc::eth::FlashblocksEthApiBuilder;
use op_alloy_consensus::OpTxEnvelope;
use reth::chainspec::EthChainSpec;
use reth_node_api::{FullNodeTypes, NodeTypes, PayloadTypes};
use reth_node_builder::{
    components::{BasicPayloadServiceBuilder, ComponentsBuilder},
    rpc::BasicEngineValidatorBuilder,
    Node, NodeAdapter, NodeComponentsBuilder,
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{
    args::RollupArgs, txpool::OpPooledTx, OpAddOns, OpAddOnsBuilder, OpBuiltPayload,
    OpConsensusBuilder, OpDAConfig, OpEngineTypes, OpEngineValidatorBuilder, OpExecutorBuilder,
    OpFullNodeTypes, OpNetworkBuilder, OpNodeTypes, OpPayloadBuilder, OpPayloadBuilderAttributes,
    OpPoolBuilder, OpStorage,
};
use reth_optimism_primitives::OpPrimitives;
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
    + InMemoryState<Primitives = OpPrimitives>
{
}

impl<T> FlashblocksProvider for T where
    T: StateProviderFactory
        + ChainSpecProvider<ChainSpec: EthChainSpec + OpHardforks>
        + Clone
        + DatabaseProviderFactory<Provider: HeaderProvider<Header = alloy_consensus::Header>>
        + InMemoryState<Primitives = OpPrimitives>
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
            compute_pending_block,
            discovery_v4,
            ..
        } = self.rollup;
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
            .payload(BasicPayloadServiceBuilder::new(
                OpPayloadBuilder::new(compute_pending_block).with_da_config(self.da_config.clone()),
            ))
            .network(OpNetworkBuilder::new(disable_txpool_gossip, !discovery_v4))
            .consensus(OpConsensusBuilder::default());
        todo!()
    }

    fn add_ons(&self) -> Self::AddOns {
        let op_add_ons = OpAddOnsBuilder::default()
            .with_sequencer(self.rollup.sequencer.clone())
            .with_sequencer_headers(self.rollup.sequencer_headers.clone())
            .with_da_config(self.da_config.clone())
            .with_enable_tx_conditional(self.rollup.enable_tx_conditional)
            .with_min_suggested_priority_fee(self.rollup.min_suggested_priority_fee)
            .with_historical_rpc(self.rollup.historical_rpc.clone())
            .with_flashblocks(self.rollup.flashblocks_url.clone())
            .build();
        todo!()
    }
}

impl NodeTypes for FlashblocksNode {
    type Primitives = OpPrimitives;
    type ChainSpec = OpChainSpec;
    type Storage = OpStorage;
    type Payload = OpEngineTypes;
}
