use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::Arc;

use alloy_primitives::Address;
use alloy_signer_local::PrivateKeySigner;
use op_alloy_consensus::OpTxEnvelope;
use reth::builder::components::{
    ComponentsBuilder, PayloadBuilderBuilder, PoolBuilder, PoolBuilderConfigOverrides,
};
use reth::builder::{
    BuilderContext, FullNodeTypes, Node, NodeAdapter, NodeComponentsBuilder, NodeTypes,
};

use reth::rpc::eth::EthApiTypes;
use reth::transaction_pool::blobstore::DiskFileBlobStore;
use reth::transaction_pool::TransactionValidationTaskExecutor;

use reth_engine_local::LocalPayloadAttributesBuilder;

use reth_evm::ConfigureEvm;
use reth_node_api::{NodeAddOns, PayloadAttributesBuilder};
use reth_node_builder::components::{NetworkBuilder, PayloadServiceBuilder};
use reth_node_builder::rpc::{EngineValidatorAddOn, RethRpcAddOns};
use reth_node_builder::{
    DebugNode, FullNodeComponents, NodeComponents, PayloadTypes, PrimitivesTy, TxTy,
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpNextBlockEnvAttributes;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::node::{OpConsensusBuilder, OpExecutorBuilder};
use reth_optimism_node::txpool::{OpPooledTx, OpTransactionValidator};
use reth_optimism_node::{
    OpBuiltPayload, OpEngineTypes, OpEvmConfig, OpPayloadAttributes, OpPayloadBuilderAttributes,
    OpStorage,
};
use reth_optimism_payload_builder::builder::OpPayloadTransactions;
use reth_optimism_payload_builder::config::{OpBuilderConfig, OpDAConfig};
use reth_optimism_primitives::{OpBlock, OpPrimitives};

use reth_provider::{
    BlockReader, BlockReaderIdExt, CanonStateSubscriptions, ChainSpecProvider, StateProviderFactory,
};

use reth_transaction_pool::{BlobStore, TransactionPool};

use crate::config::WorldChainNodeConfig;
use tracing::{debug, info};
use world_chain_payload::builder::WorldChainPayloadBuilder;
use world_chain_pool::ordering::WorldChainOrdering;
use world_chain_pool::root::WorldChainRootValidator;
use world_chain_pool::tx::{WorldChainPoolTransaction, WorldChainPooledTransaction};
use world_chain_pool::validator::WorldChainTransactionValidator;
use world_chain_pool::WorldChainTransactionPool;

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
pub struct WorldChainNode<T> {
    /// World Chain Args
    pub node_context: T,
    /// Marker type that defines the `Components` and `AddOns` types for this node.
    _marker: PhantomData<T>,
}

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
        Self {
            node_context: config.into(),
            _marker: PhantomData,
        }
    }

    /// Returns the components for the given [`WorldChainArgs`].
    pub fn components<Node>(&self) -> T::ComponentsBuilder
    where
        Node: FullNodeTypes<Types = Self>,
        T: WorldChainNodeContext<Node> + From<WorldChainNodeConfig>,
    {
        <T as WorldChainNodeContext<Node>>::components(&self.node_context)
    }

    pub fn add_ons<Node>(&self) -> T::AddOns
    where
        Node: FullNodeTypes<Types = Self>,
        T: WorldChainNodeContext<Node> + From<WorldChainNodeConfig>,
    {
        <T as WorldChainNodeContext<Node>>::add_ons(&self.node_context)
    }

    pub fn ext_context<Node>(&self) -> T::ExtContext
    where
        Node: FullNodeTypes<Types = Self>,
        T: WorldChainNodeContext<Node> + From<WorldChainNodeConfig>,
    {
        <T as WorldChainNodeContext<Node>>::ext_context(&self.node_context)
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
        LocalPayloadAttributesBuilder::new(Arc::new(chain_spec.clone()))
    }
}

impl<T: Unpin + Send + Clone + Sync + Debug + 'static> NodeTypes for WorldChainNode<T> {
    type Primitives = OpPrimitives;
    type ChainSpec = OpChainSpec;
    type Storage = OpStorage;
    type Payload = OpEngineTypes;
}

/// A basic World Chain transaction pool.
///
/// This contains various settings that can be configured and take precedence over the node's
/// config.
#[derive(Debug, Clone)]
pub struct WorldChainPoolBuilder {
    pub pbh_entrypoint: Address,
    pub pbh_signature_aggregator: Address,
    pub world_id: Address,
    /// Enforced overrides that are applied to the pool config.
    pub pool_config_overrides: PoolBuilderConfigOverrides,
}

impl WorldChainPoolBuilder {
    pub fn new(
        pbh_entrypoint: Address,
        pbh_signature_aggregator: Address,
        world_id: Address,
    ) -> Self {
        Self {
            pbh_entrypoint,
            pbh_signature_aggregator,
            world_id,
            pool_config_overrides: Default::default(),
        }
    }
}

impl WorldChainPoolBuilder {
    /// Sets the [`PoolBuilderConfigOverrides`] on the pool builder.
    pub fn with_pool_config_overrides(
        mut self,
        pool_config_overrides: PoolBuilderConfigOverrides,
    ) -> Self {
        self.pool_config_overrides = pool_config_overrides;
        self
    }
}

impl<Node> PoolBuilder<Node> for WorldChainPoolBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec: OpHardforks, Primitives = OpPrimitives>>,
{
    type Pool = WorldChainTransactionPool<Node::Provider, DiskFileBlobStore>;

    async fn build_pool(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Pool> {
        let Self {
            pbh_entrypoint,
            pbh_signature_aggregator,
            world_id,
            pool_config_overrides,
            ..
        } = self;

        let data_dir = ctx.config().datadir();
        let blob_store = DiskFileBlobStore::open(data_dir.blobstore(), Default::default())?;

        let validator = TransactionValidationTaskExecutor::eth_builder(ctx.provider().clone())
            .no_eip4844()
            .with_head_timestamp(ctx.head().timestamp)
            .kzg_settings(ctx.kzg_settings()?)
            .with_additional_tasks(
                pool_config_overrides
                    .additional_validation_tasks
                    .unwrap_or_else(|| ctx.config().txpool.additional_validation_tasks),
            )
            .build_with_tasks(ctx.task_executor().clone(), blob_store.clone())
            .map(|validator| {
                let client = validator.client().clone();
                let op_tx_validator = OpTransactionValidator::new(validator)
                    // In --dev mode we can't require gas fees because we're unable to decode the L1
                    // block info
                    .require_l1_data_gas_fee(!ctx.config().dev.dev);
                let root_validator = WorldChainRootValidator::new(client, world_id)
                    .expect("failed to initialize root validator");

                WorldChainTransactionValidator::new(
                    op_tx_validator,
                    root_validator,
                    pbh_entrypoint,
                    pbh_signature_aggregator,
                )
                .expect("failed to create world chain validator")
            });

        let transaction_pool = reth_transaction_pool::Pool::new(
            validator,
            WorldChainOrdering::default(),
            blob_store,
            pool_config_overrides.apply(ctx.pool_config()),
        );
        info!(target: "reth::cli", "Transaction pool initialized");
        let transactions_path = data_dir.txpool_transactions();

        // spawn txpool maintenance task
        {
            let pool = transaction_pool.clone();
            let chain_events = ctx.provider().canonical_state_stream();
            let client = ctx.provider().clone();
            let transactions_backup_config =
                    reth_transaction_pool::maintain::LocalTransactionBackupConfig::with_local_txs_backup(transactions_path);

            ctx.task_executor()
                .spawn_critical_with_graceful_shutdown_signal(
                    "local transactions backup task",
                    |shutdown| {
                        reth_transaction_pool::maintain::backup_local_transactions_task(
                            shutdown,
                            pool.clone(),
                            transactions_backup_config,
                        )
                    },
                );

            // spawn the maintenance task
            ctx.task_executor().spawn_critical(
                "txpool maintenance task",
                reth_transaction_pool::maintain::maintain_transaction_pool_future(
                    client,
                    pool.clone(),
                    chain_events,
                    ctx.task_executor().clone(),
                    reth_transaction_pool::maintain::MaintainPoolConfig {
                        max_tx_lifetime: pool.config().max_queued_lifetime,
                        no_local_exemptions: transaction_pool
                            .config()
                            .local_transactions_config
                            .no_exemptions,
                        ..Default::default()
                    },
                ),
            );
            debug!(target: "reth::cli", "Spawned txpool maintenance task");
        }

        Ok(transaction_pool)
    }
}

/// A basic World Chain payload service builder
#[derive(Debug, Clone)]
pub struct WorldChainPayloadBuilderBuilder<Txs = ()> {
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
    pub builder_private_key: PrivateKeySigner,
}

impl WorldChainPayloadBuilderBuilder {
    /// Create a new instance with the given `compute_pending_block` flag and data availability
    /// config.
    pub fn new(
        compute_pending_block: bool,
        verified_blockspace_capacity: u8,
        pbh_entry_point: Address,
        pbh_signature_aggregator: Address,
        builder_private_key: PrivateKeySigner,
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

impl<Txs> WorldChainPayloadBuilderBuilder<Txs> {
    /// Configures the type responsible for yielding the transactions that should be included in the
    /// payload.
    pub fn with_transactions<T>(self, best_transactions: T) -> WorldChainPayloadBuilderBuilder<T> {
        let Self {
            compute_pending_block,
            da_config,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            builder_private_key,
            ..
        } = self;

        WorldChainPayloadBuilderBuilder {
            compute_pending_block,
            da_config,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            best_transactions,
            builder_private_key,
        }
    }
}

impl<Node, S, Txs>
    PayloadBuilderBuilder<Node, WorldChainTransactionPool<Node::Provider, S>, OpEvmConfig>
    for WorldChainPayloadBuilderBuilder<Txs>
where
    Node: FullNodeTypes<
        Provider: ChainSpecProvider<ChainSpec = OpChainSpec>,
        Types: NodeTypes<
            Primitives = OpPrimitives,
            Payload: PayloadTypes<
                BuiltPayload = OpBuiltPayload<PrimitivesTy<Node::Types>>,
                PayloadAttributes = OpPayloadAttributes,
                PayloadBuilderAttributes = OpPayloadBuilderAttributes<TxTy<Node::Types>>,
            >,
        >,
    >,
    Node::Provider: StateProviderFactory + BlockReaderIdExt + BlockReader<Block = OpBlock>,
    S: BlobStore + Clone,
    Txs: OpPayloadTransactions<WorldChainPooledTransaction>,
{
    type PayloadBuilder = WorldChainPayloadBuilder<Node::Provider, S, Txs>;

    async fn build_payload_builder(
        self,
        ctx: &BuilderContext<Node>,
        pool: WorldChainTransactionPool<Node::Provider, S>,
        evm_config: OpEvmConfig,
    ) -> eyre::Result<Self::PayloadBuilder> {
        Ok(WorldChainPayloadBuilder::with_builder_config(
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
        .with_transactions(self.best_transactions.clone()))
    }
}
