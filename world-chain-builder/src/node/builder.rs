use std::{path::Path, sync::Arc};

use eyre::eyre::Result;
use reth::api::{EngineValidator, FullNodeComponents, NodeAddOns};
use reth::builder::rpc::{RethRpcAddOns, RpcAddOns, RpcHandle, RpcHooks};
use reth::builder::AddOnsContext;
use reth::builder::{
    components::ComponentsBuilder, FullNodeTypes, Node, NodeTypes, NodeTypesWithEngine,
};
use reth::builder::{NodeAdapter, NodeComponentsBuilder};
use reth::rpc::eth::FullEthApiServer;
use reth_db::DatabaseEnv;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::engine::OptimismEngineValidator;
use reth_optimism_node::node::{OpPrimitives, OptimismEngineValidatorBuilder};
use reth_optimism_node::{
    args::RollupArgs,
    node::{OptimismConsensusBuilder, OptimismExecutorBuilder, OptimismNetworkBuilder},
    OptimismEngineTypes,
};

use crate::rpc::WorldChainEthApi;
use crate::{
    payload::builder::WorldChainPayloadServiceBuilder, pbh::db::load_world_chain_db,
    pool::builder::WorldChainPoolBuilder,
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
            entry_point,
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
                entry_point,
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
    >;

    type AddOns = WorldChainAddOns<
        NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
    >;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        let Self { args, db } = self;
        Self::components(args.clone(), db.clone())
    }

    fn add_ons(&self) -> Self::AddOns {
        WorldChainAddOns::new(self.args.rollup_args.sequencer_http.clone())
    }
}

impl NodeTypes for WorldChainBuilder {
    type Primitives = OpPrimitives;
    type ChainSpec = OpChainSpec;
}

impl NodeTypesWithEngine for WorldChainBuilder {
    type Engine = OptimismEngineTypes;
}

#[derive(Debug)]
pub struct WorldChainAddOns<N: FullNodeComponents>(
    pub RpcAddOns<N, WorldChainEthApi<N>, OptimismEngineValidatorBuilder>,
);

impl<N: FullNodeComponents> Default for WorldChainAddOns<N> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<N: FullNodeComponents> WorldChainAddOns<N> {
    /// Create a new instance with the given `sequencer_http` URL.
    pub fn new(sequencer_http: Option<String>) -> Self {
        Self(RpcAddOns::new(
            move |ctx| WorldChainEthApi::new(ctx, sequencer_http),
            Default::default(),
        ))
    }
}

impl<N> NodeAddOns<N> for WorldChainAddOns<N>
where
    N: FullNodeComponents<Types: NodeTypes<ChainSpec = OpChainSpec>>,
    OptimismEngineValidator: EngineValidator<<N::Types as NodeTypesWithEngine>::Engine>,
    WorldChainEthApi<N>: FullEthApiServer,
{
    type Handle = RpcHandle<N, WorldChainEthApi<N>>;

    async fn launch_add_ons(self, ctx: AddOnsContext<'_, N>) -> eyre::Result<Self::Handle> {
        self.0.launch_add_ons(ctx).await
    }
}

impl<N> RethRpcAddOns<N> for WorldChainAddOns<N>
where
    N: FullNodeComponents<Types: NodeTypes<ChainSpec = OpChainSpec>>,
    OptimismEngineValidator: EngineValidator<<N::Types as NodeTypesWithEngine>::Engine>,
    WorldChainEthApi<N>: FullEthApiServer,
{
    type EthApi = WorldChainEthApi<N>;

    fn hooks_mut(&mut self) -> &mut RpcHooks<N, Self::EthApi> {
        self.0.hooks_mut()
    }
}
