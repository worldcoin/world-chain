pub mod network;
pub mod payload;
pub mod pool;

use reth_basic_payload_builder::{BasicPayloadJobGenerator, BasicPayloadJobGeneratorConfig};
use reth_chainspec::ChainSpec;
use reth_evm::ConfigureEvm;
use reth_node_builder::{
    components::{ComponentsBuilder, PayloadServiceBuilder},
    BuilderContext, FullNodeTypes, Node, NodeTypes, NodeTypesWithEngine, PayloadBuilderConfig,
};
use reth_node_optimism::{
    args::RollupArgs,
    node::{
        OptimismAddOns, OptimismConsensusBuilder, OptimismExecutorBuilder, OptimismNetworkBuilder,
    },
    OptimismEngineTypes, OptimismEvmConfig,
};
use reth_payload_builder::{PayloadBuilderHandle, PayloadBuilderService};
use reth_provider::CanonStateSubscriptions;
use reth_transaction_pool::TransactionPool;

use crate::{
    args::{ExtArgs, PbhBuilderArgs},
    // payload::PBHBuilder,
};

use self::{payload::PBHBuilder, pool::WorldChainPoolBuilder};

#[derive(Debug, Clone)]
pub struct WorldChainBuilder {
    /// Additional Optimism args
    pub args: ExtArgs,
}

impl WorldChainBuilder {
    pub const fn new(args: ExtArgs) -> Self {
        Self { args }
    }

    /// Returns the components for the given [`RollupArgs`].
    pub fn components<Node>(
        args: ExtArgs,
    ) -> ComponentsBuilder<
        Node,
        WorldChainPoolBuilder,
        WorldChainPayloadBuilder,
        OptimismNetworkBuilder,
        OptimismExecutorBuilder,
        OptimismConsensusBuilder,
    >
    where
        Node: FullNodeTypes<
            Types: NodeTypesWithEngine<Engine = OptimismEngineTypes, ChainSpec = ChainSpec>,
        >,
    {
        let PbhBuilderArgs { clear_nullifiers } = args.builder_args;
        let RollupArgs {
            disable_txpool_gossip,
            discovery_v4,
            ..
        } = args.rollup_args;
        ComponentsBuilder::default()
            .node_types::<Node>()
            .pool(WorldChainPoolBuilder { clear_nullifiers })
            .payload(WorldChainPayloadBuilder::new(OptimismEvmConfig::default()))
            .network(OptimismNetworkBuilder {
                disable_txpool_gossip,
                disable_discovery_v4: !discovery_v4,
            })
            .executor(OptimismExecutorBuilder::default())
            .consensus(OptimismConsensusBuilder::default())
    }
}

impl<N> Node<N> for WorldChainBuilder
where
    N: FullNodeTypes<
        Types: NodeTypesWithEngine<Engine = OptimismEngineTypes, ChainSpec = ChainSpec>,
    >,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        WorldChainPoolBuilder,
        WorldChainPayloadBuilder,
        OptimismNetworkBuilder,
        OptimismExecutorBuilder,
        OptimismConsensusBuilder,
    >;

    type AddOns = OptimismAddOns;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        let Self { args } = self;
        Self::components(args.clone())
    }
}

impl NodeTypes for WorldChainBuilder {
    type Primitives = ();
    type ChainSpec = ChainSpec;
}

impl NodeTypesWithEngine for WorldChainBuilder {
    type Engine = OptimismEngineTypes;
}

#[derive(Debug, Default, Clone)]
pub struct WorldChainPayloadBuilder<EVM = OptimismEvmConfig> {
    /// The EVM configuration to use for the payload builder.
    pub evm_config: EVM,
}

impl<EVM> WorldChainPayloadBuilder<EVM> {
    pub const fn new(evm_config: EVM) -> Self {
        Self { evm_config }
    }
}

impl<Node, EVM, Pool> PayloadServiceBuilder<Node, Pool> for WorldChainPayloadBuilder<EVM>
where
    Node: FullNodeTypes<
        Types: NodeTypesWithEngine<Engine = OptimismEngineTypes, ChainSpec = ChainSpec>,
    >,
    Pool: TransactionPool + Unpin + 'static,
    EVM: ConfigureEvm,
{
    async fn spawn_payload_service(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<PayloadBuilderHandle<OptimismEngineTypes>> {
        let payload_builder = PBHBuilder::new(self.evm_config);

        let conf = ctx.payload_builder_config();

        let payload_job_config = BasicPayloadJobGeneratorConfig::default()
            .interval(conf.interval())
            .deadline(conf.deadline())
            .max_payload_tasks(conf.max_payload_tasks())
            // no extradata for OP
            .extradata(Default::default());

        let payload_generator = BasicPayloadJobGenerator::with_builder(
            ctx.provider().clone(),
            pool,
            ctx.task_executor().clone(),
            payload_job_config,
            ctx.chain_spec(),
            payload_builder,
        );
        let (payload_service, payload_builder) =
            PayloadBuilderService::new(payload_generator, ctx.provider().canonical_state_stream());

        ctx.task_executor()
            .spawn_critical("payload builder service", Box::pin(payload_service));

        Ok(payload_builder)
    }
}
