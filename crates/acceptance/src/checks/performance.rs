//! Performance checks: the network meets its production budgets.

use std::{sync::Arc, time::Instant};

use eyre::eyre::bail;
use tokio::time::sleep;

use crate::{TestCtx, acceptance_test};

/// Number of blocks to sample when estimating the average block time.
const BLOCK_TIME_SAMPLES: u64 = 3;

/// The average block time stays within the configured budget.
///
/// Samples block timestamps across a few consecutive blocks and compares the
/// observed average interval against `thresholds.max_block_time`.
#[acceptance_test(category = Performance)]
async fn block_time_within_budget(ctx: Arc<TestCtx>) -> eyre::Result<()> {
    let thresholds = ctx.thresholds();

    let start = ctx.latest_block().await?;
    let start_number = start.header.number;
    let start_timestamp = start.header.timestamp;
    let target = start_number + BLOCK_TIME_SAMPLES;
    let deadline = Instant::now() + thresholds.block_advance_timeout;

    let end = loop {
        let block = ctx.latest_block().await?;
        if block.header.number >= target {
            break block;
        }
        if Instant::now() >= deadline {
            bail!(
                "only reached block {} of target {target} within {}s",
                block.header.number,
                thresholds.block_advance_timeout.as_secs()
            );
        }
        sleep(thresholds.block_poll_interval).await;
    };

    let blocks = end.header.number - start_number;
    let elapsed = end.header.timestamp.saturating_sub(start_timestamp);
    if blocks == 0 {
        bail!("no blocks observed while sampling block time");
    }

    let avg_block_time = elapsed as f64 / blocks as f64;
    ctx.record_f64("avg_block_time_secs", avg_block_time);
    ctx.record_i64("sampled_blocks", blocks as i64);

    let budget = thresholds.max_block_time.as_secs_f64();
    if avg_block_time > budget {
        bail!(
            "average block time {avg_block_time:.3}s exceeds budget {budget:.3}s over {blocks} blocks"
        );
    }
    Ok(())
}
