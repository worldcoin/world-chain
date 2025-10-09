use alloy_genesis::{Genesis, GenesisAccount};
use eyre::eyre::{eyre, Ok};
use reth::api::TreeConfig;
use reth::args::PayloadBuilderArgs;
use reth::builder::{EngineNodeLauncher, Node, NodeBuilder, NodeConfig, NodeHandle};
use reth::chainspec::{Chain, ChainSpecBuilder, NamedChain};
use reth::network::PeersHandleProvider;
use reth::tasks::TaskManager;
use reth_e2e_test_utils::testsuite::{Environment, NodeClient};
use reth_e2e_test_utils::{Adapter, NodeHelperType, TmpDB};
use reth_node_api::{
    FullNodeTypesAdapter, NodeAddOns, NodeTypes, NodeTypesWithDBAdapter, PayloadTypes,
};
use reth_node_builder::rpc::{EngineValidatorAddOn, RethRpcAddOns};
use reth_node_builder::{NodeComponents, NodeComponentsBuilder};
use reth_node_core::args::RpcServerArgs;
use reth_optimism_chainspec::{OpChainSpec, OpChainSpecBuilder};
use reth_optimism_node::OpEngineTypes;
use reth_optimism_primitives::OpPrimitives;
use reth_primitives::ForkCondition;
use reth_provider::providers::{BlockchainProvider, ChainStorage};
use revm_primitives::U256;
use std::{
    collections::BTreeMap,
    fs,
    ops::Range,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tracing::{info, span};
use world_chain_node::node::{WorldChainNode, WorldChainNodeContext};
use world_chain_node::{FlashblocksOpApi, OpApiExtServer};
use world_chain_test::node::test_config;
use world_chain_test::utils::{account, tree_root};
use world_chain_test::{DEV_WORLD_ID, PBH_DEV_ENTRYPOINT};

use world_chain_pool::{
    root::LATEST_ROOT_SLOT,
    validator::{MAX_U16, PBH_GAS_LIMIT_SLOT, PBH_NONCE_LIMIT_SLOT},
    BasicWorldChainPool,
};
use world_chain_rpc::{EthApiExtServer, SequencerClient, WorldChainEthApiExt};

const GENESIS: &str = include_str!("../res/genesis.json");

pub struct WorldChainTestingNodeContext<T: WorldChainTestContextBounds>
where
    WorldChainNode<T>: WorldChainNodeTestBounds<T>,
{
    pub node: WorldChainNodeTestContext<T>,
    pub ext_context: WorldChainNodeExtContext<T>,
}

type WorldChainNodeExtContext<T> = <T as WorldChainNodeContext<
    FullNodeTypesAdapter<
        WorldChainNode<T>,
        TmpDB,
        BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<T>, TmpDB>>,
    >,
>>::ExtContext;

type WorldChainNodeTestContext<T> = NodeHelperType<
    WorldChainNode<T>,
    BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<T>, TmpDB>>,
>;

fn create_chain_spec(genesis_str: &str) -> OpChainSpec {
    let spec: Genesis = serde_json::from_str(genesis_str).expect("genesis should parse");
    OpChainSpecBuilder::base_mainnet()
        .genesis(
            spec.extend_accounts(vec![(
                DEV_WORLD_ID,
                GenesisAccount::default().with_storage(Some(BTreeMap::from_iter(vec![(
                    LATEST_ROOT_SLOT.into(),
                    tree_root().into(),
                )]))),
            )])
            .extend_accounts(vec![(
                PBH_DEV_ENTRYPOINT,
                GenesisAccount::default().with_storage(Some(BTreeMap::from_iter(vec![
                    (PBH_GAS_LIMIT_SLOT.into(), U256::from(15000000).into()),
                    (
                        PBH_NONCE_LIMIT_SLOT.into(),
                        (MAX_U16 << U256::from(160)).into(),
                    ),
                ]))),
            )])
            .extend_accounts(vec![(
                account(0),
                GenesisAccount::default().with_balance(U256::from(100_000_000_000_000_000u64)),
            )]),
        )
        .ecotone_activated()
        .build()
}

fn create_chain_spec_from_file(genesis_path: &str) -> eyre::Result<OpChainSpec> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = format!("{}/tests/{}", manifest_dir, genesis_path);
    let genesis_content = fs::read_to_string(&path)?;
    let genesis: Genesis = serde_json::from_str(&genesis_content)?;   
    let op_chain_spec = OpChainSpecBuilder::default().chain(Chain::from_named(NamedChain::Mainnet)).genesis(genesis).ecotone_activated().build();
    Ok(op_chain_spec)
}

pub async fn setup<T>(
    num_nodes: u8,
    attributes_generator: impl Fn(u64) -> <<WorldChainNode<T> as NodeTypes>::Payload as PayloadTypes>::PayloadBuilderAttributes + Send + Sync + Copy + 'static,
    genesis_file: Option<&str>,
) -> eyre::Result<(
    Range<u8>,
    Vec<WorldChainTestingNodeContext<T>>,
    TaskManager,
    Environment<OpEngineTypes>,
)>
where
    T: WorldChainTestContextBounds,
    WorldChainNode<T>: WorldChainNodeTestBounds<T>,
{
    std::env::set_var("PRIVATE_KEY", DEV_WORLD_ID.to_string());
    let op_chain_spec: Arc<OpChainSpec> = if let Some(genesis_path) = genesis_file {
        Arc::new(create_chain_spec_from_file(genesis_path)?)
    } else {
        Arc::new(CHAIN_SPEC.clone())
    };

    let tasks = TaskManager::current();
    let exec = tasks.executor();

    let mut node_config: NodeConfig<OpChainSpec> = NodeConfig::new(op_chain_spec.clone())
        .with_chain(op_chain_spec.clone())
        .with_rpc(
            RpcServerArgs::default()
                .with_unused_ports()
                .with_http_unused_port()
                .with_http(),
        )
        .with_payload_builder(PayloadBuilderArgs {
            deadline: Duration::from_millis(4000),
            max_payload_tasks: 1,
            gas_limit: Some(25_000_000),
            interval: Duration::from_millis(200),
            ..Default::default()
        })
        .with_unused_ports();

    // discv5 ports seem to be clashing
    node_config.network.discovery.disable_discovery = true;
    node_config.network.discovery.addr = [127, 0, 0, 1].into();

    // is 0.0.0.0 by default
    node_config.network.addr = [127, 0, 0, 1].into();

    let mut environment = Environment::default();
    let mut node_contexts =
        Vec::<WorldChainTestingNodeContext<T>>::with_capacity(num_nodes as usize);

    for idx in 0..num_nodes {
        let span = span!(tracing::Level::INFO, "test_node", idx);
        let _enter = span.enter();
        let config = test_config();

        let node = WorldChainNode::<T>::new(config.args.clone().into_config(&op_chain_spec)?);

        let ext_context = node.ext_context();

        let NodeHandle {
            node,
            node_exit_future: _,
        } = NodeBuilder::new(node_config.clone())
            .testing_node(exec.clone())
            .with_types_and_provider::<WorldChainNode<T>, BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<T>, TmpDB>>>()
            .with_components(node.components_builder())
            .with_add_ons(node.add_ons())
            .extend_rpc_modules(move |ctx| {
                let provider = ctx.provider().clone();
                let pool = ctx.pool().clone();
                let sequencer_client = config.args.rollup.sequencer.map(SequencerClient::new);
                let eth_api_ext = WorldChainEthApiExt::new(pool, provider, sequencer_client);
                ctx.modules.replace_configured(eth_api_ext.into_rpc())?;
                ctx.modules.replace_configured(FlashblocksOpApi.into_rpc())?;
                Ok(())
            })
            .launch_with_fn(|builder| {
                let launcher = EngineNodeLauncher::new(
                    builder.task_executor().clone(),
                    builder.config().datadir(),
                    TreeConfig::default(),
                );
                builder.launch_with(launcher)
            }).await?;

        let mut node = WorldChainNodeTestContext::new(node, attributes_generator).await?;
        let genesis = node.inner.chain_spec().sealed_genesis_header();

        node.update_forkchoice(genesis.hash(), genesis.hash())
            .await?;

        // Connect each node in a chain.
        if let Some(previous_node) = node_contexts.last_mut() {
            previous_node.node.connect(&mut node).await;
        }

        // Connect last node with the first if there are more than two
        if idx + 1 == num_nodes && num_nodes > 2 {
            if let Some(first_node) = node_contexts.first_mut() {
                node.connect(&mut first_node.node).await;
            }
        }

        let world_chain_test_node = WorldChainTestingNodeContext { node, ext_context };

        node_contexts.push(world_chain_test_node);
    }

    for n in &node_contexts {
        let node = &n.node;
        let rpc = node
            .rpc_client()
            .ok_or_else(|| eyre!("Failed to create HTTP RPC client for node"))?;
        let auth = node.auth_server_handle();
        let url = node.rpc_url();
        environment
            .node_clients
            .push(NodeClient::new(rpc, auth, url));
    }

    Ok((0..5, node_contexts, tasks, environment))
}

pub static CHAIN_SPEC: LazyLock<OpChainSpec> = LazyLock::new(|| create_chain_spec(GENESIS));

/// Consolidated trait bound for WorldChainNode testing context
pub trait WorldChainTestContextBounds:
    WorldChainNodeContext<
        FullNodeTypesAdapter<
            WorldChainNode<Self>,
            TmpDB,
            BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<Self>, TmpDB>>,
        >,
        AddOns: NodeAddOns<
            Adapter<
                WorldChainNode<Self>,
                BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<Self>, TmpDB>>,
            >,
        > + RethRpcAddOns<
            Adapter<
                WorldChainNode<Self>,
                BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<Self>, TmpDB>>,
            >,
        > + EngineValidatorAddOn<
            Adapter<
                WorldChainNode<Self>,
                BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<Self>, TmpDB>>,
            >,
        >,
        ComponentsBuilder: NodeComponentsBuilder<
            FullNodeTypesAdapter<
                WorldChainNode<Self>,
                TmpDB,
                BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<Self>, TmpDB>>,
            >,
            Components: NodeComponents<
                FullNodeTypesAdapter<
                    WorldChainNode<Self>,
                    TmpDB,
                    BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<Self>, TmpDB>>,
                >,
                Network: PeersHandleProvider,
                Pool = BasicWorldChainPool<
                    FullNodeTypesAdapter<
                        WorldChainNode<Self>,
                        TmpDB,
                        BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<Self>, TmpDB>>,
                    >,
                >,
            >,
        >,
    > + Send
    + Sync
    + 'static
where
    WorldChainNode<Self>: NodeTypes<
            Primitives = OpPrimitives,
            ChainSpec = OpChainSpec,
            Storage: ChainStorage<OpPrimitives>,
        > + Node<
            FullNodeTypesAdapter<
                WorldChainNode<Self>,
                TmpDB,
                BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<Self>, TmpDB>>,
            >,
            AddOns = <Self as WorldChainNodeContext<
                FullNodeTypesAdapter<
                    WorldChainNode<Self>,
                    TmpDB,
                    BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<Self>, TmpDB>>,
                >,
            >>::AddOns,
            ComponentsBuilder: NodeComponentsBuilder<
                FullNodeTypesAdapter<
                    WorldChainNode<Self>,
                    TmpDB,
                    BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<Self>, TmpDB>>,
                >,
                Components: NodeComponents<
                    FullNodeTypesAdapter<
                        WorldChainNode<Self>,
                        TmpDB,
                        BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<Self>, TmpDB>>,
                    >,
                    Network: PeersHandleProvider,
                    Pool = BasicWorldChainPool<
                        FullNodeTypesAdapter<
                            WorldChainNode<Self>,
                            TmpDB,
                            BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<Self>, TmpDB>>,
                        >,
                    >,
                >,
            >,
        >,
{
}

// Adapter<Self, BlockchainProvider<NodeTypesWithDBAdapter<Self, TmpDB>>>,
impl<T> WorldChainTestContextBounds for T
where
    T: WorldChainNodeContext<
        FullNodeTypesAdapter<
            WorldChainNode<T>,
            TmpDB,
            BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<T>, TmpDB>>,
        >,
        AddOns: NodeAddOns<
            Adapter<
                WorldChainNode<Self>,
                BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<Self>, TmpDB>>,
            >,
        > + RethRpcAddOns<
            Adapter<
                WorldChainNode<Self>,
                BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<Self>, TmpDB>>,
            >,
        > + EngineValidatorAddOn<
            Adapter<
                WorldChainNode<Self>,
                BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<Self>, TmpDB>>,
            >,
        >,
        ComponentsBuilder: NodeComponentsBuilder<
            FullNodeTypesAdapter<
                WorldChainNode<T>,
                TmpDB,
                BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<T>, TmpDB>>,
            >,
            Components: NodeComponents<
                FullNodeTypesAdapter<
                    WorldChainNode<T>,
                    TmpDB,
                    BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<T>, TmpDB>>,
                >,
                Network: PeersHandleProvider,
                Pool = BasicWorldChainPool<
                    FullNodeTypesAdapter<
                        WorldChainNode<T>,
                        TmpDB,
                        BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<T>, TmpDB>>,
                    >,
                >,
            >,
        >,
    >,
    WorldChainNode<T>: NodeTypes<
            Primitives = OpPrimitives,
            ChainSpec = OpChainSpec,
            Storage: ChainStorage<OpPrimitives>,
        > + Node<
            FullNodeTypesAdapter<
                WorldChainNode<T>,
                TmpDB,
                BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<T>, TmpDB>>,
            >,
            AddOns = <T as WorldChainNodeContext<
                FullNodeTypesAdapter<
                    WorldChainNode<T>,
                    TmpDB,
                    BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<T>, TmpDB>>,
                >,
            >>::AddOns,
            ComponentsBuilder: NodeComponentsBuilder<
                FullNodeTypesAdapter<
                    WorldChainNode<T>,
                    TmpDB,
                    BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<T>, TmpDB>>,
                >,
                Components: NodeComponents<
                    FullNodeTypesAdapter<
                        WorldChainNode<T>,
                        TmpDB,
                        BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<T>, TmpDB>>,
                    >,
                    Network: PeersHandleProvider,
                    Pool = BasicWorldChainPool<
                        FullNodeTypesAdapter<
                            WorldChainNode<T>,
                            TmpDB,
                            BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<T>, TmpDB>>,
                        >,
                    >,
                >,
            >,
        >,
{
}

/// Wrapper trait that consolidates all trait bounds for WorldChainNode<T> in testing
pub trait WorldChainNodeTestBounds<T>:
    NodeTypes<
        Primitives = OpPrimitives,
        ChainSpec = OpChainSpec,
        Storage: ChainStorage<OpPrimitives>,
    > + Node<
        FullNodeTypesAdapter<Self, TmpDB, BlockchainProvider<NodeTypesWithDBAdapter<Self, TmpDB>>>,
        AddOns = T::AddOns,
        ComponentsBuilder = T::ComponentsBuilder,
    >
where
    T: WorldChainTestContextBounds,
{
}

impl<T, Ctx> WorldChainNodeTestBounds<Ctx> for T
where
    T: NodeTypes<
            Primitives = OpPrimitives,
            ChainSpec = OpChainSpec,
            Storage: ChainStorage<OpPrimitives>,
        > + Node<
            FullNodeTypesAdapter<T, TmpDB, BlockchainProvider<NodeTypesWithDBAdapter<T, TmpDB>>>,
            AddOns = Ctx::AddOns,
            ComponentsBuilder = Ctx::ComponentsBuilder,
        >,
    Ctx: WorldChainTestContextBounds,
{
}
