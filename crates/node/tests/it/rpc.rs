use reth_rpc_api::Web3ApiClient;
use world_chain_node::{
    context::WorldChainDefaultContext,
    version::{
        WORLD_CHAIN_CLIENT_NAME, WORLD_CHAIN_CLIENT_VERSION, WORLD_CHAIN_CLIENT_VERSION_SHA,
    },
};
use world_chain_test_utils::e2e_harness::setup::WorldChainTestBuilder;

#[tokio::test]
async fn web3_client_version_reports_world_chain_build_identity() -> eyre::Result<()> {
    world_chain_node::init_version_metadata();

    let (_, nodes, _exec, _env, _spammer) = WorldChainTestBuilder::builder()
        .flashblocks(false)
        .build()
        .setup::<WorldChainDefaultContext>()
        .await?;

    let client = nodes[0]
        .node
        .rpc_client()
        .ok_or_else(|| eyre::eyre::eyre!("expected HTTP RPC client"))?;

    let version = Web3ApiClient::client_version(&client).await?;

    assert_eq!(version, WORLD_CHAIN_CLIENT_VERSION);
    assert!(version.starts_with(&format!("{WORLD_CHAIN_CLIENT_NAME}/")));
    assert!(version.contains(WORLD_CHAIN_CLIENT_VERSION_SHA));

    Ok(())
}
