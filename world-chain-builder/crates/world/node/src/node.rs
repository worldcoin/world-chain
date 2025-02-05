use eyre::eyre::Result;
use reth::api::{ConfigureEvm, HeaderTy, TxTy};
use reth::builder::components::{ComponentsBuilder, PayloadServiceBuilder};
use reth::builder::{
    BuilderContext, FullNodeTypes, Node, NodeAdapter, NodeComponentsBuilder, NodeTypes,
    NodeTypesWithEngine, PayloadBuilderConfig,
};
use reth::payload::{PayloadBuilderHandle, PayloadBuilderService};
use reth::transaction_pool::TransactionPool;
use reth_basic_payload_builder::{BasicPayloadJobGenerator, BasicPayloadJobGeneratorConfig};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::args::RollupArgs;
use reth_optimism_node::node::{
    OpAddOns, OpConsensusBuilder, OpExecutorBuilder, OpNetworkBuilder, OpStorage,
};
use reth_optimism_node::{OpEngineTypes, OpEvmConfig};
use reth_optimism_payload_builder::builder::OpPayloadTransactions;
use reth_optimism_payload_builder::config::OpDAConfig;
use reth_optimism_primitives::OpPrimitives;
use reth_provider::CanonStateSubscriptions;
use reth_trie_db::MerklePatriciaTrie;
use revm_primitives::Address;
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
    // TODO:
    pub verified_blockspace_capacity: u8,
    pub pbh_entry_point: Address,
}

impl WorldChainPayloadBuilder {
    /// Create a new instance with the given `compute_pending_block` flag.
    pub const fn new(
        compute_pending_block: bool,
        verified_blockspace_capacity: u8,
        pbh_entry_point: Address,
    ) -> Self {
        Self {
            compute_pending_block,
            verified_blockspace_capacity,
            pbh_entry_point,
            best_transactions: (),
        }
    }
}

impl<Txs> WorldChainPayloadBuilder<Txs>
where
    Txs: OpPayloadTransactions,
{
    /// Configures the type responsible for yielding the transactions that should be included in the
    /// payload.
    pub fn with_transactions<T: OpPayloadTransactions>(
        self,
        best_transactions: T,
    ) -> WorldChainPayloadBuilder<T> {
        let Self {
            compute_pending_block,
            verified_blockspace_capacity,
            pbh_entry_point,
            ..
        } = self;

        WorldChainPayloadBuilder {
            compute_pending_block,
            verified_blockspace_capacity,
            pbh_entry_point,
            best_transactions,
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
        let payload_builder = world_chain_builder_payload::builder::WorldChainPayloadBuilder::new(
            evm_config,
            self.verified_blockspace_capacity,
            self.pbh_entry_point,
        )
        .with_transactions(self.best_transactions)
        .set_compute_pending_block(self.compute_pending_block);

        let conf = ctx.payload_builder_config();

        let payload_job_config = BasicPayloadJobGeneratorConfig::default()
            .interval(conf.interval())
            .deadline(conf.deadline())
            .max_payload_tasks(conf.max_payload_tasks());

        let payload_generator = BasicPayloadJobGenerator::with_builder(
            ctx.provider().clone(),
            pool,
            ctx.task_executor().clone(),
            payload_job_config,
            payload_builder,
        );
        let (payload_service, payload_builder) =
            PayloadBuilderService::new(payload_generator, ctx.provider().canonical_state_stream());

        ctx.task_executor()
            .spawn_critical("payload builder service", Box::pin(payload_service));

        Ok(payload_builder)
    }
}

impl<N, Pool, Txs> PayloadServiceBuilder<N, Pool> for WorldChainPayloadBuilder<Txs>
where
    N: FullNodeTypes<
        Types: NodeTypesWithEngine<
            Engine = OpEngineTypes,
            ChainSpec = OpChainSpec,
            Primitives = OpPrimitives,
        >,
    >,
    Pool: TransactionPool<Transaction: WorldChainPoolTransaction<Consensus = TxTy<N::Types>>>
        + Unpin
        + 'static,
    Txs: OpPayloadTransactions,
{
    async fn spawn_payload_service(
        self,
        ctx: &BuilderContext<N>,
        pool: Pool,
    ) -> eyre::Result<PayloadBuilderHandle<OpEngineTypes>> {
        self.spawn(OpEvmConfig::new(ctx.chain_spec()), ctx, pool)
    }
}
