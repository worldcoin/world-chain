use std::sync::Arc;

use args::{ExtArgs, WorldChainBuilderArgs};
use reth::{
    api::{FullNodeTypes, NodeTypesWithEngine},
    builder::{components::ComponentsBuilder, Node},
};
use reth_db::DatabaseEnv;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::{
    args::RollupArgs,
    node::{OpConsensusBuilder, OpExecutorBuilder, OpNetworkBuilder, OpStorage},
    OpEngineTypes, OpNode,
};
use reth_optimism_payload_builder::config::OpDAConfig;
use reth_optimism_primitives::OpPrimitives;

use crate::{payload::builder::WorldChainPayloadBuilder, pool::builder::WorldChainPoolBuilder};

pub mod args;
pub mod builder;

#[cfg(test)]
pub mod test_utils;

/// Type configuration for World Chain node
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct WorldChainNode {
    /// Additional Optimism args
    pub args: RollupArgs,
    /// Data availability configuration for the OP builder.
    ///
    /// Used to throttle the size of the data availability payloads (configured by the batcher via
    /// the `miner_` api).
    ///
    /// By default no throttling is applied.
    pub da_config: OpDAConfig,
}

impl WorldChainNode {
    /// Creates a new instance of the Optimism node type.
    pub fn new(args: RollupArgs) -> Self {
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
        args: RollupArgs,
        ext_args: ExtArgs,
        db: Arc<DatabaseEnv>,
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
            clear_nullifiers,
            num_pbh_txs,
            verified_blockspace_capacity,
        } = ext_args.builder_args;

        let RollupArgs {
            disable_txpool_gossip,
            compute_pending_block,
            discovery_v4,
            ..
        } = args;
        ComponentsBuilder::default()
            .node_types::<Node>()
            .pool(WorldChainPoolBuilder {
                num_pbh_txs,
                clear_nullifiers,
                db: db.clone(),
            })
            .payload(WorldChainPayloadBuilder::new(
                compute_pending_block,
                verified_blockspace_capacity,
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
        OpPoolBuilder,
        OpPayloadBuilder,
        OpNetworkBuilder,
        OpExecutorBuilder,
        OpConsensusBuilder,
    >;

    type AddOns =
        OpAddOns<NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>>;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        Self::components(self.args.clone())
    }

    fn add_ons(&self) -> Self::AddOns {
        Self::AddOns::builder()
            .with_sequencer(self.args.sequencer_http.clone())
            .with_da_config(self.da_config.clone())
            .build()
    }
}

impl NodeTypes for OpNode {
    type Primitives = OpPrimitives;
    type ChainSpec = OpChainSpec;
    type StateCommitment = MerklePatriciaTrie;
    type Storage = OpStorage;
}

impl NodeTypesWithEngine for OpNode {
    type Engine = OpEngineTypes;
}

/// Add-ons w.r.t. optimism.
#[derive(Debug)]
pub struct OpAddOns<N: FullNodeComponents> {
    /// Rpc add-ons responsible for launching the RPC servers and instantiating the RPC handlers
    /// and eth-api.
    pub rpc_add_ons: RpcAddOns<N, OpEthApi<N>, OpEngineValidatorBuilder>,
    /// Data availability configuration for the OP builder.
    pub da_config: OpDAConfig,
}

impl<N: FullNodeComponents<Types: NodeTypes<Primitives = OpPrimitives>>> Default for OpAddOns<N> {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl<N: FullNodeComponents<Types: NodeTypes<Primitives = OpPrimitives>>> OpAddOns<N> {
    /// Build a [`OpAddOns`] using [`OpAddOnsBuilder`].
    pub fn builder() -> OpAddOnsBuilder {
        OpAddOnsBuilder::default()
    }
}
