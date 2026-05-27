//! Prometheus metrics for the OP Proposer ExEx.
//!
//! Mirrors `op-proposer/metrics/metrics.go`. The metric name prefix matches
//! upstream (`op_proposer_*`) so existing dashboards keep working.

use metrics::{Counter, Gauge};
use metrics_derive::Metrics;

#[derive(Clone, Metrics)]
#[metrics(scope = "op_proposer")]
pub struct ProposerMetrics {
    /// Sequence number (block number or super-root timestamp) of the latest
    /// successfully submitted proposal.
    pub proposed_sequence_number: Gauge,
    /// L2 block number of the latest successfully submitted proposal (legacy).
    pub l2_blocks_proposed: Gauge,
    /// Number of successful proposal submissions.
    pub proposal_submissions: Counter,
    /// Number of failed proposal submissions.
    pub proposal_failures: Counter,
    /// Number of skipped proposal attempts (recent proposal already exists or
    /// no change in root).
    pub proposal_skipped: Counter,
    /// `1` once the proposer has finished starting up.
    pub up: Gauge,
}

impl ProposerMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_l2_proposal(&self, sequence_num: u64) {
        self.proposed_sequence_number.set(sequence_num as f64);
        self.proposal_submissions.increment(1);
    }

    pub fn record_l2_block_proposed(&self, block_number: u64) {
        self.l2_blocks_proposed.set(block_number as f64);
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
