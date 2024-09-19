use std::{path::Path, sync::Arc};

use reth_chainspec::ChainSpec;
use reth_db::{create_db, mdbx::DatabaseArguments, DatabaseEnv};
use reth_node_api::{FullNodeComponents, NodeAddOns};
use reth_node_builder::{
    components::ComponentsBuilder, FullNodeTypes, Node, NodeTypes, NodeTypesWithEngine,
};
use reth_node_optimism::{
    args::RollupArgs,
    node::{OptimismConsensusBuilder, OptimismExecutorBuilder, OptimismNetworkBuilder},
    OptimismEngineTypes, OptimismEvmConfig,
};
use tracing::info;

use crate::{
    payload::builder::WorldChainPayloadServiceBuilder, pool::builder::WorldChainPoolBuilder,
    rpc::WorldChainEthApi,
};

use super::args::{ExtArgs, WorldChainBuilderArgs};

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
        WorldChainPayloadServiceBuilder,
        OptimismNetworkBuilder,
        OptimismExecutorBuilder,
        OptimismConsensusBuilder,
    >
    where
        Node: FullNodeTypes<
            Types: NodeTypesWithEngine<Engine = OptimismEngineTypes, ChainSpec = ChainSpec>,
        >,
    {
        let WorldChainBuilderArgs {
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
            .pool(WorldChainPoolBuilder {
                clear_nullifiers,
                num_pbh_txs,
            })
            .payload(WorldChainPayloadServiceBuilder::default())
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
        WorldChainPayloadServiceBuilder,
        OptimismNetworkBuilder,
        OptimismExecutorBuilder,
        OptimismConsensusBuilder,
    >;

    type AddOns = WorldChainAddOns;

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

/// Add-ons w.r.t. optimism.
#[derive(Debug, Clone)]
pub struct WorldChainAddOns;

impl<N: FullNodeComponents> NodeAddOns<N> for WorldChainAddOns {
    type EthApi = WorldChainEthApi<N>;
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
