//! Network-health checks: the chain is live and making progress.

use std::{sync::Arc, time::Instant};

use alloy_eips::BlockNumberOrTag;
use eyre::eyre::bail;
use tokio::time::sleep;
use tracing::info;

use crate::{TestCtx, acceptance_test};

/// The L2 RPC answers `eth_chainId`.
#[acceptance_test(category = Health)]
async fn rpc_liveness(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    let chain_id = ctx.chain_id().await?;
    ctx.record_i64("chain_id", chain_id as i64);
    Ok(())
}

/// `eth_getBlockByNumber("latest")` returns a block.
#[acceptance_test(category = Health)]
async fn latest_block_exists(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    let block = ctx.latest_block().await?;
    ctx.record_i64("latest_block", block.header.number as i64);
    ctx.record_i64("latest_timestamp", block.header.timestamp as i64);
    Ok(())
}

/// The block number advances by at least the configured increment within the
/// advance timeout.
#[acceptance_test(category = Health)]
async fn block_number_advances(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    let thresholds = ctx.thresholds();
    let start = ctx.block_number().await?;
    let target = start + thresholds.min_block_increments;
    let deadline = Instant::now() + thresholds.block_advance_timeout;

    let mut last = start;
    while Instant::now() < deadline {
        last = ctx.block_number().await?;
        if last >= target {
            ctx.record_i64("blocks_advanced", (last - start) as i64);
            return Ok(());
        }
        sleep(thresholds.block_poll_interval).await;
    }

    bail!(
        "block number did not advance: start={start} last={last} required_increments={} timeout_secs={}",
        thresholds.min_block_increments,
        thresholds.block_advance_timeout.as_secs()
    );
}

/// The safe head progresses, confirming L1 derivation/batching is healthy.
///
/// Only meaningful where an L1 endpoint is wired, so it skips at run time when
/// the environment has no L1 endpoint configured.
#[acceptance_test(category = Health)]
async fn safe_head_progresses(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    ctx.skip_if(ctx.l1().is_none(), "no L1 endpoint configured")?;

    let thresholds = ctx.thresholds();
    let Some(start) = ctx.block_number_by_tag(BlockNumberOrTag::Safe).await? else {
        bail!("safe head is unavailable");
    };
    let deadline = Instant::now() + thresholds.block_advance_timeout;

    let mut last = start;
    while Instant::now() < deadline {
        if let Some(safe) = ctx.block_number_by_tag(BlockNumberOrTag::Safe).await? {
            if safe < last {
                bail!("safe head regressed from {last} to {safe}");
            }
            if safe > start {
                info!(start, safe, "safe head progressed");
                ctx.record_i64("safe_head_progress", (safe - start) as i64);
                return Ok(());
            }
            last = safe;
        }
        sleep(thresholds.block_poll_interval).await;
    }

    bail!(
        "safe head did not progress beyond {start} within {}s",
        thresholds.block_advance_timeout.as_secs()
    );
}
