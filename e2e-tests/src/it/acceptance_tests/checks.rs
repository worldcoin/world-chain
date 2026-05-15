use std::time::Instant;

use eyre::eyre::{bail, ensure};
use tokio::time::sleep;
use tracing::info;

use super::rpc::RpcEnv;

pub(super) async fn chain_id_matches(env: &RpcEnv) -> eyre::Result<()> {
    let actual = env.chain_id().await?;
    let expected = env.config().expected_chain_id;

    ensure!(
        actual == expected,
        "chain ID mismatch for network={} rpc={} expected={} actual={}",
        env.config().network,
        env.config().rpc_target(),
        expected,
        actual
    );

    info!(
        network = %env.config().network,
        expected,
        actual,
        "chain ID matched"
    );
    Ok(())
}

pub(super) async fn latest_block_exists(env: &RpcEnv) -> eyre::Result<()> {
    let block = env.latest_block().await?;
    info!(
        network = %env.config().network,
        number = block.header.number,
        timestamp = block.header.timestamp,
        "latest block exists"
    );
    Ok(())
}

pub(super) async fn block_number_advances(env: &RpcEnv) -> eyre::Result<()> {
    let start = env.latest_block_number().await?;
    let target = start + env.config().min_block_increments;
    let started_at = Instant::now();
    let deadline = started_at + env.config().block_advance_timeout;
    let mut last = start;

    while Instant::now() < deadline {
        sleep(env.config().block_poll_interval).await;
        last = env.latest_block_number().await?;

        if last >= target {
            info!(
                network = %env.config().network,
                start,
                end = last,
                elapsed_secs = started_at.elapsed().as_secs_f64(),
                "block number advanced"
            );
            return Ok(());
        }
    }

    bail!(
        "block number did not advance for network={} rpc={} start={} last={} required_increments={} timeout_secs={}",
        env.config().network,
        env.config().rpc_target(),
        start,
        last,
        env.config().min_block_increments,
        env.config().block_advance_timeout.as_secs()
    );
}
