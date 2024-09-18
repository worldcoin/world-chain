use std::{path::Path, sync::Arc};

use reth_chainspec::ChainSpec;
use reth_db::{create_db, mdbx::DatabaseArguments, DatabaseEnv};
use reth_node_builder::{
    components::ComponentsBuilder, FullNodeTypes, Node, NodeTypes, NodeTypesWithEngine,
};
use reth_node_optimism::{
    args::RollupArgs,
    node::{
        OptimismAddOns, OptimismConsensusBuilder, OptimismExecutorBuilder, OptimismNetworkBuilder,
    },
    OptimismEngineTypes, OptimismEvmConfig,
};
use tracing::info;

use crate::{
    executor::builder::WorldCoinExecutorBuilder,
    network::builder::WorldCoinNetworkBuilder,
    payload::builder::WorldChainPayloadServiceBuilder,
    pool::{builder::WorldCoinPoolBuilder, provider::DatabaseProviderFactoryRW},
};

use super::args::{ExtArgs, WorldCoinBuilderArgs};

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
        WorldCoinPoolBuilder,
        WorldChainPayloadServiceBuilder,
        OptimismNetworkBuilder,
        OptimismExecutorBuilder,
        OptimismConsensusBuilder,
    >
    where
        Node: FullNodeTypes<
            Types: NodeTypesWithEngine<Engine = OptimismEngineTypes, ChainSpec = ChainSpec>,
            Provider: DatabaseProviderFactoryRW<Arc<DatabaseEnv>>,
        >,
    {
        let WorldCoinBuilderArgs {
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
            .pool(WorldCoinPoolBuilder {
                clear_nullifiers,
                num_pbh_txs,
            })
            .payload(WorldChainPayloadServiceBuilder::new(
                OptimismEvmConfig::default(),
            ))
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
    <N as FullNodeTypes>::Provider: DatabaseProviderFactoryRW<Arc<DatabaseEnv>>,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        WorldCoinPoolBuilder,
        WorldChainPayloadServiceBuilder,
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

pub fn load_world_chain_db(
    data_dir: &Path,
    clear_nullifiers: bool,
) -> Result<Arc<reth_db::DatabaseEnv>, eyre::eyre::Error> {
    let path = data_dir.join("world-chain");
    if clear_nullifiers {
        info!(?path, "Clearing semaphore-nullifiers database");
        // delete the directory
        std::fs::remove_dir_all(&path)?;
    }
    info!(?path, "Opening semaphore-nullifiers database");
    Ok(Arc::new(create_db(path, DatabaseArguments::default())?))
}
