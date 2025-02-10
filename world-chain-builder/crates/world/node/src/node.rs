use alloy_primitives::Address;
use eyre::eyre::Result;
use reth::api::{ConfigureEvm, HeaderTy, PrimitivesTy, TxTy};
use reth::builder::components::{
    ComponentsBuilder, PayloadServiceBuilder, PoolBuilderConfigOverrides,
};
use reth::builder::{
    BuilderContext, FullNodeTypes, Node, NodeAdapter, NodeComponentsBuilder, NodeTypes,
    NodeTypesWithEngine, PayloadBuilderConfig,
};
use reth::payload::{PayloadBuilderHandle, PayloadBuilderService};
use reth::transaction_pool::TransactionPool;
use reth_basic_payload_builder::{BasicPayloadJobGenerator, BasicPayloadJobGeneratorConfig};
use reth_evm::ConfigureEvmFor;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::BasicOpReceiptBuilder;
use reth_optimism_node::args::RollupArgs;
use reth_optimism_node::node::{
    OpAddOns, OpConsensusBuilder, OpExecutorBuilder, OpNetworkBuilder, OpPayloadBuilder, OpStorage,
};
use reth_optimism_node::{OpEngineTypes, OpEvmConfig};
use reth_optimism_payload_builder::builder::OpPayloadTransactions;
use reth_optimism_payload_builder::config::{OpBuilderConfig, OpDAConfig};
use reth_optimism_primitives::OpPrimitives;
use reth_provider::CanonStateSubscriptions;
use reth_trie_db::MerklePatriciaTrie;
use world_chain_builder_pool::builder::WorldChainPoolBuilder;
use world_chain_builder_pool::tx::WorldChainPoolTransaction;

use super::args::{ExtArgs, WorldChainBuilderArgs};

#[derive(Debug, Clone)]
pub struct WorldChainBuilder {
    /// Additional Optimism args
    pub args: ExtArgs,
    /// Data availability configuration for the OP builder.
    ///
    /// Used to throttle the size of the data availability payloads (configured by the batcher via
    /// the `miner_` api).
    ///
    /// By default no throttling is applied.
    pub da_config: OpDAConfig,
}

impl WorldChainBuilder {
    pub fn new(args: ExtArgs) -> Result<Self> {
        Ok(Self {
            args,
            da_config: OpDAConfig::default(),
        })
    }

    /// Configure the data availability configuration for the OP builder.
    pub fn with_da_config(mut self, da_config: OpDAConfig) -> Self {
        self.da_config = da_config;
        self
    }

    /// Returns the components for the given [`RollupArgs`].
    pub fn components<Node>(
        args: ExtArgs,
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
        let WorldChainBuilderArgs {
            num_pbh_txs,
            verified_blockspace_capacity,
            pbh_entrypoint,
            signature_aggregator,
            world_id,
        } = args.builder_args;

        let RollupArgs {
            disable_txpool_gossip,
            compute_pending_block,
            discovery_v4,
            ..
        } = args.rollup_args;

        ComponentsBuilder::default()
            .node_types::<Node>()
            .pool(WorldChainPoolBuilder::new(
                num_pbh_txs,
                pbh_entrypoint,
                signature_aggregator,
                world_id,
            ))
            .payload(WorldChainPayloadBuilder::new(
                compute_pending_block,
                verified_blockspace_capacity,
                pbh_entrypoint,
                signature_aggregator,
            ))
            .network(OpNetworkBuilder {
                disable_txpool_gossip,
                disable_discovery_v4: !discovery_v4,
            })
            .executor(OpExecutorBuilder::default())
            .consensus(OpConsensusBuilder::default())
    }
}

impl<N> Node<N> for WorldChainBuilder
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

    type AddOns =
        OpAddOns<NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>>;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        let Self { args, .. } = self;
        Self::components(args.clone())
    }

    fn add_ons(&self) -> Self::AddOns {
        let Self { args, da_config } = self;
        Self::AddOns::builder()
            .with_sequencer(args.rollup_args.sequencer_http.clone())
            .with_da_config(da_config.clone())
            .build()
    }
}

impl NodeTypes for WorldChainBuilder {
    type Storage = OpStorage;
    type Primitives = OpPrimitives;
    type ChainSpec = OpChainSpec;
    type StateCommitment = MerklePatriciaTrie;
}

impl NodeTypesWithEngine for WorldChainBuilder {
    type Engine = OpEngineTypes;
}
/// A basic optimism payload service builder
#[derive(Debug, Default, Clone)]
pub struct WorldChainPayloadBuilder<Txs = ()> {
    inner: reth_optimism_node::node::OpPayloadBuilder<Txs>,
    // TODO:
    pub verified_blockspace_capacity: u8,
    pub pbh_entry_point: Address,
    pub pbh_signature_aggregator: Address,
}

impl WorldChainPayloadBuilder {
    /// Create a new instance with the given `compute_pending_block` flag.
    pub fn new(
        compute_pending_block: bool,
        verified_blockspace_capacity: u8,
        pbh_entry_point: Address,
        pbh_signature_aggregator: Address,
    ) -> Self {
        Self {
            inner: OpPayloadBuilder::new(compute_pending_block),
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
        }
    }
}

impl<Txs> WorldChainPayloadBuilder<Txs> {
    /// A helper method to initialize [`reth_optimism_payload_builder::OpPayloadBuilder`] with the
    /// given EVM config.
    #[expect(clippy::type_complexity)]
    pub fn build<Node, Evm, Pool>(
        &self,
        evm_config: Evm,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<
        world_chain_builder_payload::builder::WorldChainPayloadBuilder<
            Pool,
            Node::Provider,
            Evm,
            PrimitivesTy<Node::Types>,
            Txs,
        >,
    >
    where
        Node: FullNodeTypes<
            Types: NodeTypesWithEngine<
                Engine = OpEngineTypes,
                ChainSpec = OpChainSpec,
                Primitives = OpPrimitives,
            >,
        >,
        Pool: TransactionPool<Transaction: WorldChainPoolTransaction<Consensus = TxTy<Node::Types>>>
            + Unpin
            + 'static,
        Evm: ConfigureEvmFor<PrimitivesTy<Node::Types>>,
        Txs: OpPayloadTransactions<Pool::Transaction>,
    {
        let payload_builder = world_chain_builder_payload::builder::WorldChainPayloadBuilder::new(
            pool,
            ctx.provider().clone(),
            evm_config,
            BasicOpReceiptBuilder::default(),
            self.inner.compute_pending_block,
            OpBuilderConfig {
                da_config: self.inner.da_config.clone(),
            },
        )
        .with_transactions(self.inner.best_transactions.clone());

        Ok(payload_builder)
    }
}

impl<Txs> WorldChainPayloadBuilder<Txs> {
    /// Configures the type responsible for yielding the transactions that should be included in the
    /// payload.
    pub fn with_transactions<T>(self, best_transactions: T) -> WorldChainPayloadBuilder<T> {
        let Self {
            compute_pending_block,
            verified_blockspace_capacity,
            pbh_entry_point,
            pbh_signature_aggregator,
            ..
        } = self;

        WorldChainPayloadBuilder {
            compute_pending_block,
            verified_blockspace_capacity,
            pbh_entry_point,
            best_transactions,
            pbh_signature_aggregator,
        }
    }

    /// A helper method to initialize [`PayloadBuilderService`] with the given EVM config.
    pub fn spawn<Node, Evm, Pool>(
        self,
        evm_config: Evm,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<PayloadBuilderHandle<OpEngineTypes>>
    where
        Node: FullNodeTypes<
            Types: NodeTypesWithEngine<
                Engine = OpEngineTypes,
                ChainSpec = OpChainSpec,
                Primitives = OpPrimitives,
            >,
        >,
        Pool: TransactionPool<Transaction: WorldChainPoolTransaction<Consensus = TxTy<Node::Types>>>
            + Unpin
            + 'static,
        Evm: ConfigureEvm<Header = HeaderTy<Node::Types>, Transaction = TxTy<Node::Types>>,
    {
        todo!("TODO:")
        // let payload_builder = world_chain_builder_payload::builder::WorldChainPayloadBuilder::new(
        //     evm_config,
        //     self.verified_blockspace_capacity,
        //     self.pbh_entry_point,
        //     self.pbh_signature_aggregator,
        // )
        // .with_transactions(self.best_transactions)
        // .set_compute_pending_block(self.compute_pending_block);

        // let conf = ctx.payload_builder_config();

        // let payload_job_config = BasicPayloadJobGeneratorConfig::default()
        //     .interval(conf.interval())
        //     .deadline(conf.deadline())
        //     .max_payload_tasks(conf.max_payload_tasks());

        // let payload_generator = BasicPayloadJobGenerator::with_builder(
        //     ctx.provider().clone(),
        //     pool,
        //     ctx.task_executor().clone(),
        //     payload_job_config,
        //     payload_builder,
        // );
        // let (payload_service, payload_builder) =
        //     PayloadBuilderService::new(payload_generator, ctx.provider().canonical_state_stream());

        // ctx.task_executor()
        //     .spawn_critical("payload builder service", Box::pin(payload_service));

        // Ok(payload_builder)
    }
}

impl<Node, Pool, Txs> PayloadServiceBuilder<Node, Pool> for WorldChainPayloadBuilder<Txs>
where
    Node: FullNodeTypes<
        Types: NodeTypesWithEngine<
            Engine = OpEngineTypes,
            ChainSpec = OpChainSpec,
            Primitives = OpPrimitives,
        >,
    >,
    Pool: TransactionPool<Transaction: WorldChainPoolTransaction<Consensus = TxTy<Node::Types>>>
        + Unpin
        + 'static,
    Txs: OpPayloadTransactions<Pool::Transaction>,
{
    type PayloadBuilder = reth_optimism_payload_builder::OpPayloadBuilder<
        Pool,
        Node::Provider,
        OpEvmConfig,
        PrimitivesTy<Node::Types>,
        Txs,
    >;

    async fn build_payload_builder(
        &self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<Self::PayloadBuilder> {
        self.build(OpEvmConfig::new(ctx.chain_spec()), ctx, pool)
    }
}
