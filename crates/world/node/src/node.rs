use alloy_primitives::Address;
use op_alloy_consensus::OpTxEnvelope;
use reth::builder::components::{
    BasicPayloadServiceBuilder, ComponentsBuilder, PayloadBuilderBuilder, PoolBuilder,
    PoolBuilderConfigOverrides,
};
use reth::builder::{
    BuilderContext, FullNodeTypes, Node, NodeAdapter, NodeComponentsBuilder, NodeTypes,
};

use reth::transaction_pool::blobstore::DiskFileBlobStore;
use reth::transaction_pool::TransactionValidationTaskExecutor;

use reth_node_builder::components::PayloadServiceBuilder;
use reth_node_builder::{DebugNode, FullNodeComponents, PayloadTypes, PrimitivesTy, TxTy};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::args::RollupArgs;
use reth_optimism_node::node::{
    OpAddOns, OpConsensusBuilder, OpEngineValidatorBuilder, OpExecutorBuilder, OpNetworkBuilder,
    OpNodeTypes,
};
use reth_optimism_node::txpool::OpTransactionValidator;
use reth_optimism_node::{
    OpBuiltPayload, OpEngineApiBuilder, OpEngineTypes, OpEvmConfig, OpPayloadAttributes,
    OpPayloadBuilderAttributes, OpStorage,
};
use reth_optimism_payload_builder::builder::OpPayloadTransactions;
use reth_optimism_payload_builder::config::{OpBuilderConfig, OpDAConfig};
use reth_optimism_primitives::{OpBlock, OpPrimitives};

use reth_optimism_rpc::eth::OpEthApiBuilder;
use reth_provider::{
    BlockReader, BlockReaderIdExt, CanonStateSubscriptions, ChainSpecProvider, StateProviderFactory,
};
use reth_transaction_pool::BlobStore;
use reth_trie_db::MerklePatriciaTrie;
use tracing::{debug, info};
use world_chain_builder_pool::ordering::WorldChainOrdering;
use world_chain_builder_pool::root::WorldChainRootValidator;
use world_chain_builder_pool::tx::WorldChainPooledTransaction;
use world_chain_builder_pool::validator::WorldChainTransactionValidator;
use world_chain_builder_pool::WorldChainTransactionPool;

use crate::args::WorldChainArgs;

/// Type configuration for a regular World Chain node.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct WorldChainNode {
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

/// A [`ComponentsBuilder`] with its generic arguments set to a stack of World Chain specific builders.
pub type WorldChainNodeComponentBuilder<Node, Payload = WorldChainPayloadBuilder> =
    ComponentsBuilder<
        Node,
        WorldChainPoolBuilder,
        BasicPayloadServiceBuilder<Payload>,
        OpNetworkBuilder,
        OpExecutorBuilder,
        OpConsensusBuilder,
    >;

impl WorldChainNode {
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

    /// Returns the components for the given [`WorldChainArgs`].
    pub fn components<Node>(&self) -> WorldChainNodeComponentBuilder<Node>
    where
        Node: FullNodeTypes<Types: OpNodeTypes>,
        BasicPayloadServiceBuilder<WorldChainPayloadBuilder>: PayloadServiceBuilder<
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
            .executor(OpExecutorBuilder::default())
            .payload(BasicPayloadServiceBuilder::new(
                WorldChainPayloadBuilder::new(
                    compute_pending_block,
                    verified_blockspace_capacity,
                    pbh_entrypoint,
                    signature_aggregator,
                    builder_private_key,
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

impl<N> Node<N> for WorldChainNode
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
    type ComponentsBuilder = WorldChainNodeComponentBuilder<N>;

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

impl<N> DebugNode<N> for WorldChainNode
where
    N: FullNodeComponents<Types = Self>,
{
    type RpcBlock = alloy_rpc_types_eth::Block<OpTxEnvelope>;

    fn rpc_to_primitive_block(rpc_block: Self::RpcBlock) -> reth_node_api::BlockTy<Self> {
        rpc_block.into_consensus()
    }
}

impl NodeTypes for WorldChainNode {
    type Primitives = OpPrimitives;
    type ChainSpec = OpChainSpec;
    type StateCommitment = MerklePatriciaTrie;
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
                let op_tx_validator = OpTransactionValidator::new(validator.clone())
                    // In --dev mode we can't require gas fees because we're unable to decode the L1
                    // block info
                    .require_l1_data_gas_fee(!ctx.config().dev.dev);
                let root_validator =
                    WorldChainRootValidator::new(validator.client().clone(), world_id)
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
#[derive(Debug, Default, Clone)]
pub struct WorldChainPayloadBuilder<Txs = ()> {
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
    pub builder_private_key: String,
}

impl WorldChainPayloadBuilder {
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

impl<Txs> WorldChainPayloadBuilder<Txs> {
    /// Configures the type responsible for yielding the transactions that should be included in the
    /// payload.
    pub fn with_transactions<T>(self, best_transactions: T) -> WorldChainPayloadBuilder<T> {
        let Self {
            compute_pending_block,
            da_config,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            builder_private_key,
            ..
        } = self;

        WorldChainPayloadBuilder {
            compute_pending_block,
            da_config,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            best_transactions,
            builder_private_key,
        }
    }

    /// A helper method to initialize [`WorldChainPayloadBuilder`] with the
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
        Node: FullNodeTypes<
            Provider: ChainSpecProvider<ChainSpec: OpHardforks>,
            Types: NodeTypes<
                Primitives = OpPrimitives,
                Payload: PayloadTypes<
                    BuiltPayload = OpBuiltPayload<PrimitivesTy<Node::Types>>,
                    PayloadAttributes = OpPayloadAttributes,
                    PayloadBuilderAttributes = OpPayloadBuilderAttributes<TxTy<Node::Types>>,
                >,
            >,
        >,
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

impl<Node, S, Txs>
    PayloadBuilderBuilder<Node, WorldChainTransactionPool<Node::Provider, S>, OpEvmConfig>
    for WorldChainPayloadBuilder<Txs>
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
    type PayloadBuilder =
        world_chain_builder_payload::builder::WorldChainPayloadBuilder<Node::Provider, S, Txs>;

    async fn build_payload_builder(
        self,
        ctx: &BuilderContext<Node>,
        pool: WorldChainTransactionPool<Node::Provider, S>,
        evm_config: OpEvmConfig,
    ) -> eyre::Result<Self::PayloadBuilder> {
        self.build(evm_config, ctx, pool)
    }
}
