use std::sync::Arc;

use reth_chainspec::ChainSpec;
use reth_db::DatabaseEnv;
use reth_node_builder::{
    components::ComponentsBuilder, FullNodeTypes, Node, NodeTypes, NodeTypesWithEngine,
};
use reth_node_optimism::{
    args::RollupArgs,
    node::{OptimismAddOns, OptimismConsensusBuilder},
    OptimismEngineTypes, OptimismEvmConfig,
};

use crate::{
    executer::builder::WcExecutorBuilder,
    network::builder::WcNetworkBuilder,
    payload::builder::WcPayloadServiceBuilder,
    pool::{builder::WcPoolBuilder, provider::DatabaseProviderFactoryRW},
};

use super::args::{ExtArgs, WcBuilderArgs};

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
        WcPoolBuilder,
        WcPayloadServiceBuilder,
        WcNetworkBuilder,
        WcExecutorBuilder,
        OptimismConsensusBuilder,
    >
    where
        Node: FullNodeTypes<
            Types: NodeTypesWithEngine<Engine = OptimismEngineTypes, ChainSpec = ChainSpec>,
            Provider: DatabaseProviderFactoryRW<Arc<DatabaseEnv>>,
        >,
    {
        let WcBuilderArgs {
            clear_nullifiers,
            num_pbh_txs,
        } = args.builder_args;
        let RollupArgs {
            disable_txpool_gossip,
            discovery_v4,
            ..
        } = args.rollup_args;
        ComponentsBuilder::default()
            .node_types::<Node>()
            .pool(WcPoolBuilder {
                clear_nullifiers,
                num_pbh_txs,
            })
            .payload(WcPayloadServiceBuilder::new(OptimismEvmConfig::default()))
            .network(WcNetworkBuilder {
                disable_txpool_gossip,
                disable_discovery_v4: !discovery_v4,
            })
            .executor(WcExecutorBuilder::default())
            .consensus(OptimismConsensusBuilder::default())
    }
}

impl<N> Node<N> for WorldChainBuilder
where
    N: FullNodeTypes<
        Types: NodeTypesWithEngine<Engine = OptimismEngineTypes, ChainSpec = ChainSpec>,
    >,
    <N as FullNodeTypes>::Provider: DatabaseProviderFactoryRW<Arc<DatabaseEnv>>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        WcPoolBuilder,
        WcPayloadServiceBuilder,
        WcNetworkBuilder,
        WcExecutorBuilder,
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
