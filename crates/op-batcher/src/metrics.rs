//! Prometheus metrics for the OP Batcher ExEx.
//!
//! Mirrors `op-batcher/metrics/metrics.go`. The metric prefix is `op_batcher_*`
//! so existing dashboards keep working. The wallet-balance gauge is polled by a
//! background task (same approach as the proposer crate), not a separate Go
//! balance-monitor goroutine.

use std::{sync::Arc, time::Duration};

use alloy_primitives::Address;
use alloy_provider::{DynProvider, Provider};
use metrics::{Counter, Gauge};
use metrics_derive::Metrics;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

const WEI_PER_ETH: f64 = 1e18;

#[derive(Clone, Metrics)]
#[metrics(scope = "op_batcher")]
pub struct BatcherMetrics {
    /// `1` once the batcher has finished starting up.
    pub up: Gauge,
    /// L2 block number of the most recent block included in a confirmed batch.
    pub last_batch_l2_block: Gauge,
    /// Number of successful batch-tx submissions (confirmed frames).
    pub batch_submissions: Counter,
    /// Number of failed batch-tx submissions.
    pub batch_failures: Counter,
    /// Number of channels closed (and framed for submission).
    pub channel_closed_total: Counter,
    /// Number of L2 blocks currently buffered in the channel manager.
    pub pending_blocks_count: Gauge,
    /// Total bytes of calldata posted across all confirmed batches.
    pub calldata_bytes_total: Counter,
    /// Wallet balance of the batcher EOA, in ETH.
    pub wallet_balance_eth: Gauge,
    /// Total number of wallet-balance gauge refreshes.
    pub wallet_balance_polls: Counter,
    /// Number of failed wallet-balance refreshes.
    pub wallet_balance_failures: Counter,
}

impl BatcherMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_up(&self) {
        self.up.set(1.0);
    }

    pub fn record_batch_submission(&self, l2_block: u64, calldata_len: usize) {
        self.batch_submissions.increment(1);
        self.last_batch_l2_block.set(l2_block as f64);
        self.calldata_bytes_total.increment(calldata_len as u64);
    }

    pub fn record_failure(&self) {
        self.batch_failures.increment(1);
    }

    pub fn record_channel_closed(&self) {
        self.channel_closed_total.increment(1);
    }

    pub fn set_pending_blocks(&self, n: usize) {
        self.pending_blocks_count.set(n as f64);
    }
}

/// Spawn a background task that periodically refreshes the wallet balance gauge.
/// Mirrors the proposer crate's `spawn_balance_poller`.
pub fn spawn_balance_poller(
    provider: DynProvider,
    from: Address,
    interval: Duration,
    metrics: Arc<BatcherMetrics>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    metrics.wallet_balance_polls.increment(1);
                    match provider.get_balance(from).await {
                        Ok(bal) => {
                            let eth = bal.to_string().parse::<f64>().unwrap_or(0.0) / WEI_PER_ETH;
                            metrics.wallet_balance_eth.set(eth);
                            debug!(target: "exex::batcher::metrics", balance_eth = eth, "refreshed wallet balance");
                        }
                        Err(e) => {
                            metrics.wallet_balance_failures.increment(1);
                            warn!(target: "exex::batcher::metrics", error = %e, "failed to refresh wallet balance");
                        }
                    }
                }
            }
        }
    });
}
