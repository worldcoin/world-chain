use std::{path::Path, sync::Arc};

use eyre::eyre::Result;
use reth_db::DatabaseEnv;
use reth::api::{FullNodeComponents, NodeAddOns};
use reth::builder::{
    components::ComponentsBuilder, FullNodeTypes, Node, NodeTypes, NodeTypesWithEngine,
};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::{
    args::RollupArgs,
    node::{
        OptimismAddOns, OptimismConsensusBuilder, OptimismEngineValidatorBuilder,
        OptimismExecutorBuilder, OptimismNetworkBuilder,
    },
    OptimismEngineTypes,
};

use crate::{
    payload::builder::WorldChainPayloadServiceBuilder, pbh::db::load_world_chain_db,
    pool::builder::WorldChainPoolBuilder, rpc::WorldChainEthApi,
};

use super::args::{ExtArgs, WorldChainBuilderArgs};

#[derive(Debug, Clone)]
pub struct WorldChainBuilder {
    /// Additional Optimism args
    pub args: ExtArgs,
    /// The World Chain database
    pub db: Arc<DatabaseEnv>,
}

impl WorldChainBuilder {
    pub fn new(args: ExtArgs, data_dir: &Path) -> Result<Self> {
        let db = load_world_chain_db(data_dir, args.builder_args.clear_nullifiers)?;
        Ok(Self { args, db })
    }

    /// Returns the components for the given [`RollupArgs`].
    pub fn components<Node>(
        args: ExtArgs,
        db: Arc<DatabaseEnv>,
    ) -> ComponentsBuilder<
        Node,
        WorldChainPoolBuilder,
        WorldChainPayloadServiceBuilder,
        OptimismNetworkBuilder,
        OptimismExecutorBuilder,
        OptimismConsensusBuilder,
        OptimismEngineValidatorBuilder,
    >
    where
        Node: FullNodeTypes<
            Types: NodeTypesWithEngine<Engine = OptimismEngineTypes, ChainSpec = OpChainSpec>,
        >,
    {
        let WorldChainBuilderArgs {
            clear_nullifiers,
            num_pbh_txs,
            verified_blockspace_capacity,
        } = args.builder_args;

        let RollupArgs {
            disable_txpool_gossip,
            compute_pending_block,
            discovery_v4,
            ..
        } = args.rollup_args;

        ComponentsBuilder::default()
            .node_types::<Node>()
            .pool(WorldChainPoolBuilder {
                num_pbh_txs,
                clear_nullifiers,
                db: db.clone(),
            })
            .payload(WorldChainPayloadServiceBuilder::new(
                compute_pending_block,
                verified_blockspace_capacity,
                db.clone(),
            ))
            .network(OptimismNetworkBuilder {
                disable_txpool_gossip,
                disable_discovery_v4: !discovery_v4,
            })
            .executor(OptimismExecutorBuilder::default())
            .consensus(OptimismConsensusBuilder::default())
            .engine_validator(OptimismEngineValidatorBuilder::default())
    }
}

impl<N> Node<N> for WorldChainBuilder
where
    N: FullNodeTypes<
        Types: NodeTypesWithEngine<Engine = OptimismEngineTypes, ChainSpec = OpChainSpec>,
    >,
{
    type ComponentsBuilder = ComponentsBuilder<
        N,
        WorldChainPoolBuilder,
        WorldChainPayloadServiceBuilder,
        OptimismNetworkBuilder,
        OptimismExecutorBuilder,
        OptimismConsensusBuilder,
        OptimismEngineValidatorBuilder,
    >;

    type AddOns = WorldChainAddOns;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        let Self { args, db } = self;
        Self::components(args.clone(), db.clone())
    }

    fn add_ons(&self) -> Self::AddOns {
        Self::AddOns {
            inner: OptimismAddOns::new(self.args.rollup_args.sequencer_http.clone()),
        }
    }
}

impl NodeTypes for WorldChainBuilder {
    type Primitives = ();
    type ChainSpec = OpChainSpec;
}

impl NodeTypesWithEngine for WorldChainBuilder {
    type Engine = OptimismEngineTypes;
}

#[derive(Debug, Clone)]
pub struct WorldChainAddOns {
    pub inner: OptimismAddOns,
}

impl<N: FullNodeComponents> NodeAddOns<N> for WorldChainAddOns {
    type EthApi = WorldChainEthApi<N>;
}
