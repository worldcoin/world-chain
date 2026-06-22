//! Acceptance tests for deployed networks.
//!
mod checks;
mod config;
mod erc4337;
mod rpc;

use rpc::RpcEnv;

async fn run_acceptance_tests() -> eyre::Result<()> {
    let Some(env) = RpcEnv::connect().await? else {
        return Ok(());
    };

    checks::chain_id_matches(&env).await?;
    checks::latest_block_exists(&env).await?;
    checks::block_number_advances(&env).await?;
    erc4337::sponsored_user_operations(&env).await
}

#[tokio::test(flavor = "multi_thread")]
async fn test_network() -> eyre::Result<()> {
    run_acceptance_tests().await
}
