//! Prometheus metrics for the OP Proposer ExEx.
//!
//! Mirrors `op-proposer/metrics/metrics.go`. The metric name prefix matches
//! upstream (`op_proposer_*`) so existing dashboards keep working.

use std::{sync::Arc, time::Duration};

use alloy_primitives::Address;
use alloy_provider::{DynProvider, Provider};
use metrics::{Counter, Gauge};
use metrics_derive::Metrics;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Wei → ETH (Gwei would be similarly trivial; we expose ETH because it's
/// what dashboards typically alert on).
const WEI_PER_ETH: f64 = 1e18;

#[derive(Clone, Metrics)]
#[metrics(scope = "op_proposer")]
pub struct ProposerMetrics {
    /// L2 block number of the latest successfully submitted proposal.
    pub proposed_block_number: Gauge,
    /// Number of successful proposal submissions.
    pub proposal_submissions: Counter,
    /// Number of failed proposal submissions.
    pub proposal_failures: Counter,
    /// Number of skipped proposal attempts (recent proposal already exists
    /// or no change in root).
    pub proposal_skipped: Counter,
    /// Wallet balance of the proposer EOA, in ETH (1e-18 wei).
    pub wallet_balance_eth: Gauge,
    /// Total number of times the wallet balance gauge has been refreshed.
    pub wallet_balance_polls: Counter,
    /// Number of times a wallet balance refresh has failed.
    pub wallet_balance_failures: Counter,
    /// `1` once the proposer has finished starting up.
    pub up: Gauge,
}

impl ProposerMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_l2_proposal(&self, block_number: u64) {
        self.proposed_block_number.set(block_number as f64);
        self.proposal_submissions.increment(1);
    }

    pub fn record_failure(&self) {
        self.proposal_failures.increment(1);
    }

    pub fn record_skipped(&self) {
        self.proposal_skipped.increment(1);
    }

    pub fn record_up(&self) {
        self.up.set(1.0);
    }
}

/// Periodically polls `provider.get_balance(address)` and updates
/// [`ProposerMetrics::wallet_balance_eth`]. Cancellation via the supplied
/// token shuts the task down.
pub fn spawn_balance_poller(
    provider: DynProvider,
    address: Address,
    interval: Duration,
    metrics: Arc<ProposerMetrics>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    debug!(target: "exex::proposer::metrics", "balance poller cancelled");
                    return;
                }
                _ = ticker.tick() => {
                    match provider.get_balance(address).await {
                        Ok(wei) => {
                            // U256 → f64 with wei/1e18 in ETH; loses precision
                            // beyond ~15 sig figs, which is fine for a gauge.
                            let eth = wei.to_string().parse::<f64>().unwrap_or(f64::NAN)
                                / WEI_PER_ETH;
                            metrics.wallet_balance_eth.set(eth);
                            metrics.wallet_balance_polls.increment(1);
                        }
                        Err(e) => {
                            warn!(
                                target: "exex::proposer::metrics",
                                error = %e,
                                "failed to refresh wallet balance",
                            );
                            metrics.wallet_balance_failures.increment(1);
                        }
                    }
                }
            }
        }
    });
}
