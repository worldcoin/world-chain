use reth_db::test_utils::create_test_rw_db;
use reth_node_api::{FullNodeComponents, NodeTypesWithDBAdapter};
use reth_node_builder::{NodeBuilder, NodeConfig};
use reth_optimism_chainspec::BASE_MAINNET;
use reth_provider::providers::BlockchainProvider;
use world_chain_node::{
    context::{BasicContext, FlashblocksContext},
    node::WorldChainNode,
};
use world_chain_test::node::test_config;

#[test]
fn test_basic_flashblocks_setup() {
    // parse CLI -> config
    let config = NodeConfig::new(BASE_MAINNET.clone());
    let db = create_test_rw_db();
    let node = WorldChainNode::<FlashblocksContext>::new(test_config());
    let _builder = NodeBuilder::new(config)
        .with_database(db)
        .with_types_and_provider::<WorldChainNode<FlashblocksContext>, BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<FlashblocksContext>, _>>>()
        .with_components(node.components())
        .with_add_ons(node.add_ons())
        .on_component_initialized(move |ctx| {
            let _provider = ctx.provider();
            Ok(())
        })
        .on_node_started(|_full_node| Ok(()))
        .on_rpc_started(|_ctx, handles| {
            let _client = handles.rpc.http_client();
            Ok(())
        })
        .extend_rpc_modules(|ctx| {
            let _ = ctx.config();
            let _ = ctx.node().provider();

            Ok(())
        })
        .check_launch();
}

#[test]
fn test_basic_setup() {
    // parse CLI -> config
    let config = NodeConfig::new(BASE_MAINNET.clone());
    let db = create_test_rw_db();
    let node = WorldChainNode::<BasicContext>::new(test_config());
    let _builder = NodeBuilder::new(config)
        .with_database(db)
        .with_types_and_provider::<WorldChainNode<BasicContext>, BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<BasicContext>, _>>>()
        .with_components(node.components())
        .with_add_ons(node.add_ons())
        .on_component_initialized(move |ctx| {
            let _provider = ctx.provider();
            Ok(())
        })
        .on_node_started(|_full_node| Ok(()))
        .on_rpc_started(|_ctx, handles| {
            let _client = handles.rpc.http_client();
            Ok(())
        })
        .extend_rpc_modules(|ctx| {
            let _ = ctx.config();
            let _ = ctx.node().provider();

            Ok(())
        })
        .check_launch();
}
