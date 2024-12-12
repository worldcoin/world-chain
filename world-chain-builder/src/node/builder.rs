use std::{path::Path, sync::Arc};

use eyre::eyre::Result;
use reth::builder::components::{ConsensusBuilder, ExecutorBuilder, NetworkBuilder, PoolBuilder};
use reth::builder::{
    components::ComponentsBuilder, FullNodeTypes, Node, NodeTypes, NodeTypesWithEngine,
};
use reth::builder::{NodeAdapter, NodeComponentsBuilder};
use reth::transaction_pool::blobstore::DiskFileBlobStore;
use reth::transaction_pool::{Pool, TransactionValidationTaskExecutor};
use reth_db::DatabaseEnv;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::args::RollupArgs;
use reth_optimism_node::node::{
    OpAddOns, OpConsensusBuilder, OpExecutorBuilder, OpNetworkBuilder, OpStorage,
};
use reth_optimism_node::OpEngineTypes;
use reth_optimism_payload_builder::config::OpDAConfig;
use reth_optimism_primitives::OpPrimitives;
use reth_trie_db::MerklePatriciaTrie;

use crate::pool::ordering::WorldChainOrdering;
use crate::pool::tx::WorldChainPooledTransaction;
use crate::pool::validator::WorldChainTransactionValidator;
use crate::{
    payload::builder::WorldChainPayloadServiceBuilder, pbh::db::load_world_chain_db,
    pool::builder::WorldChainPoolBuilder,
};

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
    /// The World Chain database
    pub db: Arc<DatabaseEnv>,
}

impl WorldChainBuilder {
    pub fn new(args: ExtArgs, data_dir: &Path) -> Result<Self> {
        let db = load_world_chain_db(data_dir, args.builder_args.clear_nullifiers)?;
        Ok(Self {
            args,
            db,
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
        db: Arc<DatabaseEnv>,
    ) -> ComponentsBuilder<
        Node,
        WorldChainPoolBuilder,
        WorldChainPayloadServiceBuilder,
        OpNetworkBuilder,
        OpExecutorBuilder,
        OpConsensusBuilder,
    >
    where
        Node: FullNodeTypes<
            Types: NodeTypesWithEngine<Engine = OpEngineTypes, ChainSpec = OpChainSpec>,
        >,
        OpNetworkBuilder: NetworkBuilder<
            Node,
            Pool<
                TransactionValidationTaskExecutor<
                    WorldChainTransactionValidator<
                        <Node as FullNodeTypes>::Provider,
                        WorldChainPooledTransaction,
                    >,
                >,
                WorldChainOrdering<WorldChainPooledTransaction>,
                DiskFileBlobStore,
            >,
        >,
        OpExecutorBuilder: ExecutorBuilder<Node>,
        OpConsensusBuilder: ConsensusBuilder<Node>,
        WorldChainPoolBuilder: PoolBuilder<Node>,
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
        WorldChainPayloadServiceBuilder,
        OpNetworkBuilder,
        OpExecutorBuilder,
        OpConsensusBuilder,
    >;

    type AddOns =
        OpAddOns<NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>>;

    fn components_builder(&self) -> Self::ComponentsBuilder {
        let Self { args, db, .. } = self;
        Self::components(args.clone(), db.clone())
    }

    fn add_ons(&self) -> Self::AddOns {
        let Self {
            args,
            db,
            da_config,
        } = self;
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
