use metrics::{Counter, Histogram};
use metrics_derive::Metrics;

/// Transaction pool metrics
#[derive(Clone, Metrics)]
#[metrics(scope = "payloads")]
pub struct PayloadBuilderMetrics {
    /// Total number of times an empty payload was returned because a built one was not ready.
    pub(crate) requested_empty_payload: Counter,
    /// Total number of initiated payload build attempts.
    pub(crate) initiated_payload_builds: Counter,
    /// Total number of failed payload build attempts.
    pub(crate) failed_payload_builds: Counter,
    /// Total number of job creation errors.
    pub(crate) job_creation_errors: Counter,
    /// Total number of payload build errors).
    pub(crate) payload_build_errors: Counter,
    /// Total number of EVM execution errors.
    pub(crate) evm_execution_errors: Counter,
    /// Total number of P2P publishing errors.
    pub(crate) p2p_publishing_errors: Counter,
    /// Total number of database errors.
    pub(crate) database_errors: Counter,
    /// Histogram of P2P throttle durations in milliseconds.
    pub(crate) p2p_throttle_duration_ms: Histogram,
    /// Histogram of payload sizes in bytes.
    pub(crate) payload_size_bytes: Histogram,
    /// Histogram of payload gas usage.
    pub(crate) payload_gas_used: Histogram,
    /// Histogram of payload transaction count.
    pub(crate) payload_transaction_count: Histogram,
}

impl PayloadBuilderMetrics {
    pub(crate) fn inc_requested_empty_payload(&self) {
        self.requested_empty_payload.increment(1);
    }

    pub(crate) fn inc_initiated_payload_builds(&self) {
        self.initiated_payload_builds.increment(1);
    }

    pub(crate) fn inc_failed_payload_builds(&self) {
        self.failed_payload_builds.increment(1);
    }

    /// Increment job creation errors (header fetch/missing, pre-state check failures)
    pub(crate) fn inc_job_creation_errors(&self) {
        self.job_creation_errors.increment(1);
    }

    /// Increment payload build errors (general build failures)
    pub(crate) fn inc_payload_build_errors(&self) {
        self.payload_build_errors.increment(1);
    }

    /// Increment EVM execution errors (transaction execution failures)
    pub(crate) fn inc_evm_execution_errors(&self) {
        self.evm_execution_errors.increment(1);
    }

    /// Increment P2P publishing errors (flashblock network failures)
    pub(crate) fn inc_p2p_publishing_errors(&self) {
        self.p2p_publishing_errors.increment(1);
    }

    /// Increment database errors (state access failures)
    pub(crate) fn inc_database_errors(&self) {
        self.database_errors.increment(1);
    }

    /// Record P2P throttle duration in milliseconds
    pub(crate) fn record_p2p_throttle_duration(&self, duration_ms: f64) {
        self.p2p_throttle_duration_ms.record(duration_ms);
    }

    /// Record payload size metrics (bytes, gas, transaction count)
    pub(crate) fn record_payload_metrics(&self, size_bytes: u64, gas_used: u64, tx_count: usize) {
        self.payload_size_bytes.record(size_bytes as f64);
        self.payload_gas_used.record(gas_used as f64);
        self.payload_transaction_count.record(tx_count as f64);
    }
}
