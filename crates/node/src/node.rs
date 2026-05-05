use std::{fmt::Debug, sync::Arc};

use crate::pool::WorldChainPoolBuilder;
use alloy_eips::eip1559::BaseFeeParams;
use op_alloy_consensus::OpTxEnvelope;
use op_alloy_rpc_types_engine::OpPayloadAttributes;
use reth_evm::ConfigureEvm;
use reth_node_api::{FullNodeTypes, NodeAddOns, NodeTypes, PayloadAttributesBuilder};
use reth_node_builder::{
    DebugNode, FullNodeComponents, Node, NodeAdapter, NodeComponents, NodeComponentsBuilder,
    PayloadTypes, PrimitivesTy,
    components::{ComponentsBuilder, NetworkBuilder, PayloadServiceBuilder},
    rpc::{EngineValidatorAddOn, RethRpcAddOns},
};
use reth_node_core::primitives::EthereumHardforks;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpNextBlockEnvAttributes;
use reth_optimism_node::{
    OpEngineTypes, OpStorage,
    node::{OpConsensusBuilder, OpExecutorBuilder},
    payload::OpPayloadAttrs,
    txpool::OpPooledTx,
};
use reth_optimism_primitives::OpPrimitives;
use reth_primitives_traits::SealedHeader;
use reth_rpc_eth_api::EthApiTypes;
use reth_transaction_pool::{TransactionPool, blobstore::DiskFileBlobStore};
use world_chain_cli::WorldChainNodeConfig;
use world_chain_pool::{WorldChainTransactionPool, tx::WorldChainPoolTransaction};

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
    Sized + From<WorldChainNodeConfig> + Clone + Debug + Unpin + Send + Sync + 'static
{
    /// The EVM configuration used for this World Chain node.
    ///
    /// Provides the execution environment configuration, including gas settings,
    /// precompiles, and other EVM-specific parameters for World Chain.
    type Evm: ConfigureEvm<Primitives = PrimitivesTy<N::Types>> + 'static;

    /// The network builder for establishing P2P connections and protocol handling.
    ///
    /// Configures the networking layer, including peer discovery, message propagation,
    /// and transaction pool synchronization for the World Chain network.
    type Net: NetworkBuilder<N, WorldChainTransactionPool<N::Provider, DiskFileBlobStore>> + 'static;

    /// Builder for the payload service that handles block building and validation.
    ///
    /// Responsible for constructing execution payloads, managing the transaction pool,
    /// and coordinating with the consensus layer for block production.
    type PayloadServiceBuilder: PayloadServiceBuilder<
            N,
            WorldChainTransactionPool<N::Provider, DiskFileBlobStore>,
            Self::Evm,
        >;

    /// Builder for the core node components.
    ///
    /// Constructs essential node services including the RPC server, transaction pool,
    /// block executor, and other fundamental components required for node operation.
    type ComponentsBuilder: NodeComponentsBuilder<
            N,
            Components: NodeComponents<
                N,
                Pool: TransactionPool<Transaction: WorldChainPoolTransaction + OpPooledTx>,
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
    OpExecutorBuilder,
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

impl<N, T> DebugNode<N> for WorldChainNode<T>
where
    N: FullNodeComponents<Types = Self>,
    T: WorldChainNodeContext<N> + From<WorldChainNodeConfig>,
    WorldChainNodeComponentBuilder<N, T>: NodeComponentsBuilder<N>,
{
    type RpcBlock = alloy_rpc_types_eth::Block<OpTxEnvelope>;

    fn rpc_to_primitive_block(rpc_block: Self::RpcBlock) -> reth_node_api::BlockTy<Self> {
        rpc_block.into_consensus()
    }

    fn local_payload_attributes_builder(
        chain_spec: &Self::ChainSpec,
    ) -> impl PayloadAttributesBuilder<<Self::Payload as PayloadTypes>::PayloadAttributes> {
        OpLocalPayloadAttributesBuilder {
            chain_spec: Arc::new(chain_spec.clone()),
        }
    }
}

impl<T: Unpin + Send + Clone + Sync + Debug + 'static> NodeTypes for WorldChainNode<T> {
    type Primitives = OpPrimitives;
    type ChainSpec = OpChainSpec;
    type Storage = OpStorage;
    type Payload = OpEngineTypes;
}

/// Builds [`OpPayloadAttrs`] for local/dev-mode payload generation.
///
/// Mirrors `reth_optimism_node::node::OpLocalPayloadAttributesBuilder`, which is
/// not re-exported by upstream. Used by [`DebugNode`] when running in dev mode.
struct OpLocalPayloadAttributesBuilder {
    chain_spec: Arc<OpChainSpec>,
}

impl PayloadAttributesBuilder<OpPayloadAttrs> for OpLocalPayloadAttributesBuilder {
    fn build(&self, parent: &SealedHeader<alloy_consensus::Header>) -> OpPayloadAttrs {
        use alloy_consensus::BlockHeader;
        use alloy_primitives::{Address, B64};

        let timestamp = std::cmp::max(
            parent.timestamp().saturating_add(1),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );

        let eth_attrs = alloy_rpc_types_engine::PayloadAttributes {
            timestamp,
            prev_randao: alloy_primitives::B256::random(),
            suggested_fee_recipient: Address::random(),
            withdrawals: self
                .chain_spec
                .is_shanghai_active_at_timestamp(timestamp)
                .then(Default::default),
            parent_beacon_block_root: self
                .chain_spec
                .is_cancun_active_at_timestamp(timestamp)
                .then(alloy_primitives::B256::random),
            slot_number: self
                .chain_spec
                .is_amsterdam_active_at_timestamp(timestamp)
                .then_some(0),
        };

        // Dummy system transaction for dev mode.
        // OP Mainnet transaction at index 0 in block 124665056.
        const TX_SET_L1_BLOCK: [u8; 251] = alloy_primitives::hex!(
            "7ef8f8a0683079df94aa5b9cf86687d739a60a9b4f0835e520ec4d664e2e415dca17a6df94deaddeaddeaddeaddeaddeaddeaddeaddead00019442000000000000000000000000000000000000158080830f424080b8a4440a5e200000146b000f79c500000000000000040000000066d052e700000000013ad8a3000000000000000000000000000000000000000000000000000000003ef1278700000000000000000000000000000000000000000000000000000000000000012fdf87b89884a61e74b322bbcf60386f543bfae7827725efaaf0ab1de2294a590000000000000000000000006887246668a3b87f54deb3b94ba47a6f63f32985"
        );

        let default_params = BaseFeeParams::optimism();
        let denominator = std::env::var("OP_DEV_EIP1559_DENOMINATOR")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(default_params.max_change_denominator as u32);
        let elasticity = std::env::var("OP_DEV_EIP1559_ELASTICITY")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(default_params.elasticity_multiplier as u32);
        let gas_limit = std::env::var("OP_DEV_GAS_LIMIT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok());

        let mut eip1559_bytes = [0u8; 8];
        eip1559_bytes[0..4].copy_from_slice(&denominator.to_be_bytes());
        eip1559_bytes[4..8].copy_from_slice(&elasticity.to_be_bytes());

        OpPayloadAttrs(OpPayloadAttributes {
            payload_attributes: eth_attrs,
            transactions: Some(vec![TX_SET_L1_BLOCK.into()]),
            no_tx_pool: None,
            gas_limit,
            eip_1559_params: Some(B64::from(eip1559_bytes)),
            min_base_fee: Some(0),
        })
    }
}
