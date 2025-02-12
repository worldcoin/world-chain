use alloy_primitives::Address;
use reth::api::{FullNodeComponents, NodeAddOns};
use reth::builder::components::{
    ComponentsBuilder, PayloadServiceBuilder, PoolBuilder, PoolBuilderConfigOverrides,
};
use reth::builder::rpc::{RethRpcAddOns, RpcHandle};
use reth::builder::{
    BuilderContext, FullNodeTypes, Node, NodeAdapter, NodeComponentsBuilder, NodeTypes,
    NodeTypesWithEngine,
};
use reth::rpc::api::eth::FromEvmError;
use reth::rpc::builder::RethRpcModule;
use reth::transaction_pool::blobstore::DiskFileBlobStore;
use reth::transaction_pool::TransactionValidationTaskExecutor;
use reth_evm::{ConfigureEvm, ConfigureEvmEnv};
use reth_node_api::AddOnsContext;
use reth_node_builder::rpc::EngineValidatorAddOn;
use reth_node_builder::rpc::EngineValidatorBuilder;
use reth_node_builder::rpc::RpcAddOns;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::BasicOpReceiptBuilder;
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::args::RollupArgs;
use reth_optimism_node::engine::OpEngineValidator;
use reth_optimism_node::node::{
    OpConsensusBuilder, OpEngineValidatorBuilder, OpExecutorBuilder, OpNetworkBuilder, OpStorage,
};
use reth_optimism_node::txpool::OpTransactionValidator;
use reth_optimism_node::{OpEngineTypes, OpEvmConfig};
use reth_optimism_payload_builder::builder::OpPayloadTransactions;
use reth_optimism_payload_builder::config::{OpBuilderConfig, OpDAConfig};
use reth_optimism_primitives::{OpBlock, OpPrimitives};
use reth_optimism_rpc::miner::MinerApiExtServer;
use reth_optimism_rpc::miner::OpMinerExtApi;
use reth_optimism_rpc::witness::DebugExecutionWitnessApiServer;
use reth_optimism_rpc::witness::OpDebugWitnessApi;
use reth_optimism_rpc::{OpEthApi, OpEthApiError, SequencerClient};
use reth_provider::{BlockReader, BlockReaderIdExt, CanonStateSubscriptions, StateProviderFactory};
use reth_transaction_pool::BlobStore;
use reth_trie_db::MerklePatriciaTrie;
use revm_primitives::TxEnv;
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

    /// Returns the components for the given [`RollupArgs`].
    pub fn components<Node>(
        &self,
    ) -> ComponentsBuilder<
        Node,
        WorldChainPoolBuilder,
        WorldChainPayloadBuilder,
        OpNetworkBuilder,
        OpExecutorBuilder,
        OpConsensusBuilder,
    >
    where
        Node: FullNodeTypes<
            Types: NodeTypesWithEngine<
                Engine = OpEngineTypes,
                ChainSpec = OpChainSpec,
                Primitives = OpPrimitives,
            >,
        >,
    {
        let WorldChainArgs {
            rollup_args,
            num_pbh_txs,
            verified_blockspace_capacity,
            pbh_entrypoint,
            signature_aggregator,
            world_id,
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
                num_pbh_txs,
                pbh_entrypoint,
                signature_aggregator,
                world_id,
            ))
            .payload(
                WorldChainPayloadBuilder::new(
                    compute_pending_block,
                    verified_blockspace_capacity,
                    pbh_entrypoint,
                    signature_aggregator,
                )
                .with_da_config(self.da_config.clone()),
            )
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
        Types: NodeTypesWithEngine<
            Engine = OpEngineTypes,
            ChainSpec = OpChainSpec,
            Primitives = OpPrimitives,
            Storage = OpStorage,
        >,
    >,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        WorldChainPoolBuilder,
        WorldChainPayloadBuilder,
        OpNetworkBuilder,
        OpExecutorBuilder,
        OpConsensusBuilder,
    >;

    type AddOns = WorldChainAddOns<
        NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
    >;

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

/// Add-ons w.r.t. optimism.
#[derive(Debug)]
pub struct WorldChainAddOns<N: FullNodeComponents> {
    /// Rpc add-ons responsible for launching the RPC servers and instantiating the RPC handlers
    /// and eth-api.
    pub rpc_add_ons: RpcAddOns<N, OpEthApi<N>, OpEngineValidatorBuilder>,
    /// Data availability configuration for the OP builder.
    pub da_config: OpDAConfig,
}

impl<N: FullNodeComponents<Types: NodeTypes<Primitives = OpPrimitives>>> Default
    for WorldChainAddOns<N>
{
    fn default() -> Self {
        Self::builder().build()
    }
}

impl<N: FullNodeComponents<Types: NodeTypes<Primitives = OpPrimitives>>> WorldChainAddOns<N> {
    /// Build a [`OpAddOns`] using [`OpAddOnsBuilder`].
    pub fn builder() -> WorldChainAddOnsBuilder {
        WorldChainAddOnsBuilder::default()
    }
}

impl<N> NodeAddOns<N> for WorldChainAddOns<N>
where
    N: FullNodeComponents<
        Types: NodeTypesWithEngine<
            ChainSpec = OpChainSpec,
            Primitives = OpPrimitives,
            Storage = OpStorage,
            Engine = OpEngineTypes,
        >,
        Evm: ConfigureEvmEnv<TxEnv = TxEnv>,
    >,
    OpEthApiError: FromEvmError<N::Evm>,
{
    type Handle = RpcHandle<N, OpEthApi<N>>;

    async fn launch_add_ons(
        self,
        ctx: reth_node_api::AddOnsContext<'_, N>,
    ) -> eyre::Result<Self::Handle> {
        let Self {
            rpc_add_ons,
            da_config,
        } = self;

        let builder = reth_optimism_payload_builder::OpPayloadBuilder::new(
            ctx.node.pool().clone(),
            ctx.node.provider().clone(),
            ctx.node.evm_config().clone(),
            BasicOpReceiptBuilder::default(),
        );
        // install additional OP specific rpc methods
        let debug_ext = OpDebugWitnessApi::new(
            ctx.node.provider().clone(),
            Box::new(ctx.node.task_executor().clone()),
            builder,
        );
        let miner_ext = OpMinerExtApi::new(da_config);

        rpc_add_ons
            .launch_add_ons_with(ctx, move |modules, auth_modules| {
                debug!(target: "reth::cli", "Installing debug payload witness rpc endpoint");
                modules.merge_if_module_configured(RethRpcModule::Debug, debug_ext.into_rpc())?;

                // extend the miner namespace if configured in the regular http server
                modules.merge_if_module_configured(
                    RethRpcModule::Miner,
                    miner_ext.clone().into_rpc(),
                )?;

                // install the miner extension in the authenticated if configured
                if modules.module_config().contains_any(&RethRpcModule::Miner) {
                    debug!(target: "reth::cli", "Installing miner DA rpc enddpoint");
                    auth_modules.merge_auth_methods(miner_ext.into_rpc())?;
                }
                Ok(())
            })
            .await
    }
}

impl<N> RethRpcAddOns<N> for WorldChainAddOns<N>
where
    N: FullNodeComponents<
        Types: NodeTypesWithEngine<
            ChainSpec = OpChainSpec,
            Primitives = OpPrimitives,
            Storage = OpStorage,
            Engine = OpEngineTypes,
        >,
        Evm: ConfigureEvm<TxEnv = TxEnv>,
    >,
    OpEthApiError: FromEvmError<N::Evm>,
{
    type EthApi = OpEthApi<N>;

    fn hooks_mut(&mut self) -> &mut reth_node_builder::rpc::RpcHooks<N, Self::EthApi> {
        self.rpc_add_ons.hooks_mut()
    }
}

impl<N> EngineValidatorAddOn<N> for WorldChainAddOns<N>
where
    N: FullNodeComponents<
        Types: NodeTypesWithEngine<
            ChainSpec = OpChainSpec,
            Primitives = OpPrimitives,
            Engine = OpEngineTypes,
        >,
    >,
{
    type Validator = OpEngineValidator;

    async fn engine_validator(&self, ctx: &AddOnsContext<'_, N>) -> eyre::Result<Self::Validator> {
        OpEngineValidatorBuilder::default().build(ctx).await
    }
}

/// A regular optimism evm and executor builder.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct WorldChainAddOnsBuilder {
    /// Sequencer client, configured to forward submitted transactions to sequencer of given OP
    /// network.
    sequencer_client: Option<SequencerClient>,
    /// Data availability configuration for the OP builder.
    da_config: Option<OpDAConfig>,
}

impl WorldChainAddOnsBuilder {
    /// With a [`SequencerClient`].
    pub fn with_sequencer(mut self, sequencer_client: Option<String>) -> Self {
        self.sequencer_client = sequencer_client.map(SequencerClient::new);
        self
    }

    /// Configure the data availability configuration for the OP builder.
    pub fn with_da_config(mut self, da_config: OpDAConfig) -> Self {
        self.da_config = Some(da_config);
        self
    }
}

impl WorldChainAddOnsBuilder {
    /// Builds an instance of [`OpAddOns`].
    pub fn build<N>(self) -> WorldChainAddOns<N>
    where
        N: FullNodeComponents<Types: NodeTypes<Primitives = OpPrimitives>>,
    {
        let Self {
            sequencer_client,
            da_config,
        } = self;

        WorldChainAddOns {
            rpc_add_ons: RpcAddOns::new(
                move |ctx| {
                    OpEthApi::<N>::builder()
                        .with_sequencer(sequencer_client)
                        .build(ctx)
                },
                Default::default(),
            ),
            da_config: da_config.unwrap_or_default(),
        }
    }
}

impl NodeTypes for WorldChainNode {
    type Primitives = OpPrimitives;
    type ChainSpec = OpChainSpec;
    type StateCommitment = MerklePatriciaTrie;
    type Storage = OpStorage;
}

impl NodeTypesWithEngine for WorldChainNode {
    type Engine = OpEngineTypes;
}
/// A basic World Chain transaction pool.
///
/// This contains various settings that can be configured and take precedence over the node's
/// config.
#[derive(Debug, Clone)]
pub struct WorldChainPoolBuilder {
    pub num_pbh_txs: u8,
    pub pbh_entrypoint: Address,
    pub pbh_signature_aggregator: Address,
    pub world_id: Address,
    /// Enforced overrides that are applied to the pool config.
    pub pool_config_overrides: PoolBuilderConfigOverrides,
}

impl WorldChainPoolBuilder {
    pub fn new(
        num_pbh_txs: u8,
        pbh_entrypoint: Address,
        pbh_signature_aggregator: Address,
        world_id: Address,
    ) -> Self {
        Self {
            num_pbh_txs,
            pbh_entrypoint,
            pbh_signature_aggregator,
            world_id,
            pool_config_overrides: Default::default(),
        }
    }
}

impl<Node> PoolBuilder<Node> for WorldChainPoolBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec: OpHardforks, Primitives = OpPrimitives>>,
{
    type Pool = WorldChainTransactionPool<Node::Provider, DiskFileBlobStore>;

    async fn build_pool(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Pool> {
        let Self {
            num_pbh_txs,
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
                    num_pbh_txs,
                    pbh_entrypoint,
                    pbh_signature_aggregator,
                )
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
                    pool,
                    chain_events,
                    ctx.task_executor().clone(),
                    Default::default(),
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
}

impl WorldChainPayloadBuilder {
    /// Create a new instance with the given `compute_pending_block` flag and data availability
    /// config.
    pub fn new(
        compute_pending_block: bool,
        verified_blockspace_capacity: u8,
        pbh_entry_point: Address,
        pbh_signature_aggregator: Address,
    ) -> Self {
        Self {
            compute_pending_block,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            best_transactions: (),
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
            ..
        } = self;

        WorldChainPayloadBuilder {
            compute_pending_block,
            da_config,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            best_transactions,
        }
    }

    /// A helper method to initialize [`reth_optimism_payload_builder::OpPayloadBuilder`] with the
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
            Types: NodeTypesWithEngine<
                Engine = OpEngineTypes,
                ChainSpec = OpChainSpec,
                Primitives = OpPrimitives,
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
                BasicOpReceiptBuilder::default(),
                OpBuilderConfig {
                    da_config: self.da_config.clone(),
                },
                self.compute_pending_block,
                self.verified_blockspace_capacity,
                self.pbh_entry_point,
                self.pbh_signature_aggregator,
            )
            .with_transactions(self.best_transactions.clone());

        Ok(payload_builder)
    }
}

impl<Node, S, Txs> PayloadServiceBuilder<Node, WorldChainTransactionPool<Node::Provider, S>>
    for WorldChainPayloadBuilder<Txs>
where
    Node: FullNodeTypes<
        Types: NodeTypesWithEngine<
            Engine = OpEngineTypes,
            ChainSpec = OpChainSpec,
            Primitives = OpPrimitives,
        >,
    >,
    Node::Provider: StateProviderFactory + BlockReaderIdExt + BlockReader<Block = OpBlock>,
    S: BlobStore + Clone,
    Txs: OpPayloadTransactions<WorldChainPooledTransaction>,
{
    type PayloadBuilder =
        world_chain_builder_payload::builder::WorldChainPayloadBuilder<Node::Provider, S, Txs>;

    async fn build_payload_builder(
        &self,
        ctx: &BuilderContext<Node>,
        pool: WorldChainTransactionPool<Node::Provider, S>,
    ) -> eyre::Result<Self::PayloadBuilder> {
        self.build(OpEvmConfig::new(ctx.chain_spec()), ctx, pool)
    }
}
