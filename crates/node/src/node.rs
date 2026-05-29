use std::fmt::Debug;

use crate::pool::WorldChainPoolBuilder;
use alloy_consensus::{Block, BlockBody, Header};

use reth_chainspec::EthChainSpec;
use reth_codecs::{Compress, Decompress};
use reth_evm::ConfigureEvm;
use reth_node_api::{BuiltPayload, FullNodeTypes, NodeAddOns, NodePrimitives, NodeTypes};
use reth_node_builder::{
    Node, NodeAdapter, NodeComponents, NodeComponentsBuilder, PayloadTypes,
    components::{ComponentsBuilder, NetworkBuilder, PayloadServiceBuilder},
    rpc::{EngineValidatorAddOn, RethRpcAddOns},
};
use reth_optimism_evm::OpNextBlockEnvAttributes;
use reth_optimism_node::{OpStorage, node::OpConsensusBuilder, payload::OpPayloadAttrs};
use reth_primitives_traits::{ReceiptTy, TxTy};
use reth_rpc_eth_api::EthApiTypes;
use reth_transaction_pool::TransactionPool;
use world_chain_cli::WorldChainNodeConfig;
use world_chain_evm::WorldChainExecutorBuilder;

// These imports are only used by the `#[cfg(any(test, feature = "test"))]`-gated `DebugNode`
// implementation below.
#[cfg(any(test, feature = "test"))]
use {
    op_alloy_consensus::OpTxEnvelope,
    reth_node_api::{BlockTy, PayloadAttributesBuilder},
    reth_node_builder::{DebugNode, FullNodeComponents},
    reth_node_core::primitives::EthereumHardforks,
    reth_optimism_forks::OpHardforks,
    reth_optimism_primitives::OpPrimitives,
};

/// Primitive types for a World Chain node implementation.
///
/// This trait parameterizes `NodeTypes` inherited via `WorldChainNode<T>`.
/// Allows `WorldChainNode<T>` to be fully generic over the Engine, Transaction, Block, and Receipt primitives
/// while inheriting a unified testing harness generic over `T: WorldChainNodeContext`.
pub trait WorldChainNodePrimitiveTypes:
    Sized + From<WorldChainNodeConfig> + Clone + Debug + Unpin + Send + Sync + 'static
where
    TxTy<Self::Primitives>: Compress + Decompress,
    ReceiptTy<Self::Primitives>: Compress + Decompress,
{
    /// Primitive block, receipt, and signed transaction types used by the node.
    type Primitives: NodePrimitives<
            BlockHeader = Header,
            Block = Block<TxTy<Self::Primitives>>,
            BlockBody = BlockBody<TxTy<Self::Primitives>>,
        >;

    /// Engine payload types used by the node.
    type Payload: PayloadTypes<
            PayloadAttributes = OpPayloadAttrs,
            BuiltPayload: BuiltPayload<Primitives = Self::Primitives>,
        >;

    /// Chain specification type used by the node.
    type ChainSpec: EthChainSpec<Header = Header> + 'static;
}

/// Context trait for World Chain node implementations.
///
/// This trait defines the configuration context required for setting up a World Chain node,
/// including the EVM configuration, network builder, payload service, and various components
/// and add-ons. Implementors provide the necessary types and builders to construct a fully
/// functional World Chain node.
///
/// The trait is parameterized by `N`, which must be a `FullNodeTypes` with `Types = WorldChainNode<Self>`,
/// ensuring type safety between the context and the node it configures.
pub trait WorldChainNodeContext<N: FullNodeTypes<Types = WorldChainNode<Self>>>:
    WorldChainNodePrimitiveTypes
{
    /// The EVM configuration used for this World Chain node.
    ///
    /// Provides the execution environment configuration, including gas settings,
    /// precompiles, and other EVM-specific parameters for World Chain.
    type Evm: ConfigureEvm<Primitives = Self::Primitives> + 'static;

    /// The transaction pool type used by this node context.
    ///
    /// Production nodes use the existing World Chain pool. Native-AA test
    /// contexts can introduce a pool parameterized over `WorldChainTxEnvelope`
    /// in a stacked change without changing the node type itself.
    type Pool: TransactionPool + Unpin + 'static;

    /// The network builder for establishing P2P connections and protocol handling.
    ///
    /// Configures the networking layer, including peer discovery, message propagation,
    /// and transaction pool synchronization for the World Chain network.
    type Net: NetworkBuilder<N, Self::Pool> + 'static;

    /// Builder for the payload service that handles block building and validation.
    ///
    /// Responsible for constructing execution payloads, managing the transaction pool,
    /// and coordinating with the consensus layer for block production.
    type PayloadServiceBuilder: PayloadServiceBuilder<N, Self::Pool, Self::Evm>;

    /// Builder for the core node components.
    ///
    /// Constructs essential node services including the RPC server, transaction pool,
    /// block executor, and other fundamental components required for node operation.
    type ComponentsBuilder: NodeComponentsBuilder<
            N,
            Components: NodeComponents<
                N,
                Pool = Self::Pool,
                Evm: ConfigureEvm<NextBlockEnvCtx = OpNextBlockEnvAttributes>,
            >,
        >;

    /// Customizable add-on types for extending node functionality.
    ///
    /// Allows for optional extensions such as additional RPC endpoints, custom metrics,
    /// or specialized services that enhance the base World Chain node capabilities.
    type AddOns: NodeAddOns<
            NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
        > + RethRpcAddOns<
            NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
            EthApi: EthApiTypes,
        > + EngineValidatorAddOn<
            NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
        >;

    /// Any peripheral context or extensions required by the node.
    type ExtContext: Debug + 'static;

    /// Creates and returns the components builder for this node context.
    ///
    /// This method consumes the context and produces a builder that will construct
    /// the core node components using the configuration provided by this context.
    fn components(&self) -> Self::ComponentsBuilder;

    /// Returns the add-ons configuration for extending node functionality.
    ///
    /// Provides access to optional extensions and customizations that can be
    /// applied to the World Chain node beyond its core functionality.
    fn add_ons(&self) -> Self::AddOns;

    /// Returns the extension context for the node.
    fn ext_context(&self) -> Self::ExtContext;

    /// Returns the enabled `--kona.*` CLI args, if the in-process Kona consensus node is enabled.
    ///
    /// When this returns [`Some`], the add-ons build a
    /// [`KonaConfig`](world_chain_kona::KonaConfig) from these args during add-on launch (failing
    /// the launch if the rollup config is missing/unreadable/unparsable), assemble a
    /// [`WorldChainKonaEngineClient`](world_chain_kona::WorldChainKonaEngineClient) from reth's
    /// engine handle, and spawn the Kona consensus node in-process. When [`None`], the node runs
    /// without an in-process consensus layer (relying on an external consensus client).
    fn kona_args(&self) -> Option<world_chain_cli::KonaArgs> {
        None
    }
}

/// A Generic World Chain node type.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct WorldChainNode<T>(T);

/// A [`ComponentsBuilder`] with its generic arguments set to a stack of World Chain specific builders.
pub type WorldChainNodeComponentBuilder<Node, T> = ComponentsBuilder<
    Node,
    WorldChainPoolBuilder,
    <T as WorldChainNodeContext<Node>>::PayloadServiceBuilder,
    <T as WorldChainNodeContext<Node>>::Net,
    WorldChainExecutorBuilder,
    OpConsensusBuilder,
>;

impl<T> WorldChainNode<T>
where
    T: From<WorldChainNodeConfig> + Clone,
{
    /// Creates a new instance of the World Chain node type.
    pub fn new(config: WorldChainNodeConfig) -> Self {
        Self(config.into())
    }

    /// Returns the components for the given [`WorldChainArgs`].
    pub fn components<Node>(&self) -> T::ComponentsBuilder
    where
        Node: FullNodeTypes<Types = Self>,
        T: WorldChainNodeContext<Node> + From<WorldChainNodeConfig>,
    {
        <T as WorldChainNodeContext<Node>>::components(&self.0)
    }

    pub fn add_ons<Node>(&self) -> T::AddOns
    where
        Node: FullNodeTypes<Types = Self>,
        T: WorldChainNodeContext<Node> + From<WorldChainNodeConfig>,
    {
        <T as WorldChainNodeContext<Node>>::add_ons(&self.0)
    }

    pub fn ext_context<Node>(&self) -> T::ExtContext
    where
        Node: FullNodeTypes<Types = Self>,
        T: WorldChainNodeContext<Node> + From<WorldChainNodeConfig>,
    {
        <T as WorldChainNodeContext<Node>>::ext_context(&self.0)
    }
}

impl<N, T> Node<N> for WorldChainNode<T>
where
    N: FullNodeTypes<Types = Self>,
    T: WorldChainNodeContext<N> + From<WorldChainNodeConfig>,
{
    type ComponentsBuilder = T::ComponentsBuilder;

    type AddOns = T::AddOns;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        Self::components(self)
    }

    fn add_ons(&self) -> Self::AddOns {
        Self::add_ons(self)
    }
}

#[cfg(any(test, feature = "test"))]
impl<N, T> DebugNode<N> for WorldChainNode<T>
where
    N: FullNodeComponents<Types = Self>,
    T: WorldChainNodeContext<N, Primitives = OpPrimitives> + From<WorldChainNodeConfig>,
    T::ChainSpec: Clone + EthereumHardforks + OpHardforks,
    WorldChainNodeComponentBuilder<N, T>: NodeComponentsBuilder<N>,
{
    type RpcBlock = alloy_rpc_types_eth::Block<OpTxEnvelope>;

    fn rpc_to_primitive_block(rpc_block: Self::RpcBlock) -> BlockTy<Self> {
        rpc_block.into_consensus()
    }

    fn local_payload_attributes_builder(
        chain_spec: &Self::ChainSpec,
    ) -> impl PayloadAttributesBuilder<<Self::Payload as PayloadTypes>::PayloadAttributes> {
        use std::sync::Arc;

        use crate::dev::OpLocalPayloadAttributesBuilder;

        OpLocalPayloadAttributesBuilder {
            chain_spec: Arc::new(chain_spec.clone()),
        }
    }
}

impl<T: WorldChainNodePrimitiveTypes> NodeTypes for WorldChainNode<T> {
    type Primitives = T::Primitives;
    type ChainSpec = T::ChainSpec;
    type Storage = OpStorage<TxTy<T::Primitives>>;
    type Payload = T::Payload;
}
