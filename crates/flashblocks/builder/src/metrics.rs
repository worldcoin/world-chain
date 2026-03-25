//! Metrics and instrumentation for the flashblocks builder.

use metrics::{Counter, Gauge, Histogram};
use metrics_derive::Metrics;
use std::{
    sync::LazyLock,
    time::{Duration, Instant},
};

/// Execution coordinator metrics, auto-registered under `flashblocks.coordinator.*`.
#[derive(Clone, Metrics)]
#[metrics(scope = "flashblocks.coordinator")]
pub struct ExecutionMetrics {
    // -- Latency --
    /// Validation / build phase duration (seconds).
    pub validate_duration: Histogram,
    /// Flashblocks processed in a single epoch (recorded at epoch boundary).
    pub flashblocks_per_epoch: Histogram,

    // -- Issues --
    /// Epoch invalidated by a newer canonical tip.
    pub stale_resets: Counter,
    /// Invalid payload received from P2P (decode error, bad structure).
    pub invalid_payload: Counter,
    /// Broadcast of built payload to in-memory tree failed.
    pub broadcast_failed: Counter,
    /// newPayloadV3/V4 cache hit — payload already built for this id+index.
    // TODO: FIXME: Figure out how to track this
    pub payload_cache_hits: Counter,
}

/// Global singleton — zero lookup cost per call site.
pub static EXECUTION: LazyLock<ExecutionMetrics> = LazyLock::new(ExecutionMetrics::default);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PayloadBuildStage {
    Total,
    PreExecutionChanges,
    SequencerTxExecution,
    TxPoolFetch,
    BestTxExecution,
    Finalize,
    MergeTransitions,
    StateRoot,
    BlockAssembly,
}

impl PayloadBuildStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Total => "total",
            Self::PreExecutionChanges => "pre_execution_changes",
            Self::SequencerTxExecution => "sequencer_tx_execution",
            Self::TxPoolFetch => "txpool_fetch",
            Self::BestTxExecution => "best_tx_execution",
            Self::Finalize => "finalize",
            Self::MergeTransitions => "merge_transitions",
            Self::StateRoot => "state_root",
            Self::BlockAssembly => "block_assembly",
        }
    }
}

#[derive(Debug, Default)]
pub struct PayloadBuildAttemptMetrics {
    total_duration: Option<Duration>,
    pre_execution_changes_duration: Option<Duration>,
    sequencer_tx_execution_duration: Option<Duration>,
    txpool_fetch_duration: Option<Duration>,
    best_tx_execution_duration: Option<Duration>,
    finalize_duration: Option<Duration>,
    merge_transitions_duration: Option<Duration>,
    state_root_duration: Option<Duration>,
    block_assembly_duration: Option<Duration>,
    transactions_considered_per_build: Option<u64>,
    transactions_executed_per_build: Option<u64>,
    invalid_transactions_removed_per_build: Option<u64>,
    transaction_execution_durations: Vec<Duration>,
    transaction_gas_used: Vec<u64>,
    transaction_sizes_bytes: Vec<u64>,
    transaction_da_sizes_bytes: Vec<u64>,
    transaction_over_limits_rejections: u64,
    transaction_uncompressed_size_rejections: u64,
    transaction_conditional_invalid_rejections: u64,
    transaction_blob_or_deposit_rejections: u64,
    transaction_verified_gas_limit_rejections: u64,
    transaction_duplicate_nullifier_rejections: u64,
    transaction_nonce_too_low_rejections: u64,
    transaction_invalid_descendant_rejections: u64,
    spend_nullifiers_attempts: u64,
    spend_nullifiers_success: u64,
    spend_nullifiers_failure: u64,
    spend_nullifiers_durations: Vec<Duration>,
    spend_nullifiers_counts: Vec<u64>,
}

impl PayloadBuildAttemptMetrics {
    pub fn record_stage_duration(&mut self, stage: PayloadBuildStage, duration: Duration) {
        match stage {
            PayloadBuildStage::Total => self.total_duration = Some(duration),
            PayloadBuildStage::PreExecutionChanges => {
                self.pre_execution_changes_duration = Some(duration);
            }
            PayloadBuildStage::SequencerTxExecution => {
                self.sequencer_tx_execution_duration = Some(duration);
            }
            PayloadBuildStage::TxPoolFetch => self.txpool_fetch_duration = Some(duration),
            PayloadBuildStage::BestTxExecution => {
                self.best_tx_execution_duration = Some(duration);
            }
            PayloadBuildStage::Finalize => self.finalize_duration = Some(duration),
            PayloadBuildStage::MergeTransitions => {
                self.merge_transitions_duration = Some(duration);
            }
            PayloadBuildStage::StateRoot => self.state_root_duration = Some(duration),
            PayloadBuildStage::BlockAssembly => self.block_assembly_duration = Some(duration),
        }
    }

    pub fn record_transactions_considered_per_build(&mut self, count: u64) {
        self.transactions_considered_per_build = Some(count);
    }

    pub fn record_transactions_executed_per_build(&mut self, count: u64) {
        self.transactions_executed_per_build = Some(count);
    }

    pub fn record_invalid_transactions_removed_per_build(&mut self, count: u64) {
        self.invalid_transactions_removed_per_build = Some(count);
    }

    pub fn record_transaction_execution_duration(&mut self, duration: Duration) {
        self.transaction_execution_durations.push(duration);
    }

    pub fn record_transaction_gas_used(&mut self, gas_used: u64) {
        self.transaction_gas_used.push(gas_used);
    }

    pub fn record_transaction_size_bytes(&mut self, size_bytes: u64) {
        self.transaction_sizes_bytes.push(size_bytes);
    }

    pub fn record_transaction_da_size_bytes(&mut self, size_bytes: u64) {
        self.transaction_da_sizes_bytes.push(size_bytes);
    }

    pub fn increment_rejection(&mut self, reason: PayloadBuildRejectionReason) {
        match reason {
            PayloadBuildRejectionReason::OverLimits => self.transaction_over_limits_rejections += 1,
            PayloadBuildRejectionReason::UncompressedSize => {
                self.transaction_uncompressed_size_rejections += 1;
            }
            PayloadBuildRejectionReason::ConditionalInvalid => {
                self.transaction_conditional_invalid_rejections += 1;
            }
            PayloadBuildRejectionReason::BlobOrDeposit => {
                self.transaction_blob_or_deposit_rejections += 1;
            }
            PayloadBuildRejectionReason::VerifiedGasLimit => {
                self.transaction_verified_gas_limit_rejections += 1;
            }
            PayloadBuildRejectionReason::DuplicateNullifier => {
                self.transaction_duplicate_nullifier_rejections += 1;
            }
            PayloadBuildRejectionReason::NonceTooLow => {
                self.transaction_nonce_too_low_rejections += 1;
            }
            PayloadBuildRejectionReason::InvalidDescendant => {
                self.transaction_invalid_descendant_rejections += 1;
            }
        }
    }

    pub fn increment_spend_nullifiers_attempts(&mut self) {
        self.spend_nullifiers_attempts += 1;
    }

    pub fn record_spend_nullifiers_outcome(&mut self, outcome: PayloadBuildTaskOutcome) {
        match outcome {
            PayloadBuildTaskOutcome::Success => self.spend_nullifiers_success += 1,
            PayloadBuildTaskOutcome::Failure => self.spend_nullifiers_failure += 1,
        }
    }

    pub fn record_spend_nullifiers_duration(&mut self, duration: Duration) {
        self.spend_nullifiers_durations.push(duration);
    }

    pub fn record_spend_nullifiers_count(&mut self, count: u64) {
        self.spend_nullifiers_counts.push(count);
    }

    pub fn publish(self, metrics: &PayloadBuildMetrics) {
        if let Some(duration) = self.total_duration {
            metrics.record_stage_duration(PayloadBuildStage::Total, duration);
        }
        if let Some(duration) = self.pre_execution_changes_duration {
            metrics.record_stage_duration(PayloadBuildStage::PreExecutionChanges, duration);
        }
        if let Some(duration) = self.sequencer_tx_execution_duration {
            metrics.record_stage_duration(PayloadBuildStage::SequencerTxExecution, duration);
        }
        if let Some(duration) = self.txpool_fetch_duration {
            metrics.record_stage_duration(PayloadBuildStage::TxPoolFetch, duration);
        }
        if let Some(duration) = self.best_tx_execution_duration {
            metrics.record_stage_duration(PayloadBuildStage::BestTxExecution, duration);
        }
        if let Some(duration) = self.finalize_duration {
            metrics.record_stage_duration(PayloadBuildStage::Finalize, duration);
        }
        if let Some(duration) = self.merge_transitions_duration {
            metrics.record_stage_duration(PayloadBuildStage::MergeTransitions, duration);
        }
        if let Some(duration) = self.state_root_duration {
            metrics.record_stage_duration(PayloadBuildStage::StateRoot, duration);
        }
        if let Some(duration) = self.block_assembly_duration {
            metrics.record_stage_duration(PayloadBuildStage::BlockAssembly, duration);
        }
        if let Some(count) = self.transactions_considered_per_build {
            metrics.record_transactions_considered_per_build(count);
        }
        if let Some(count) = self.transactions_executed_per_build {
            metrics.record_transactions_executed_per_build(count);
        }
        if let Some(count) = self.invalid_transactions_removed_per_build {
            metrics.record_invalid_transactions_removed_per_build(count);
        }
        for duration in self.transaction_execution_durations {
            metrics.record_transaction_execution_duration(duration);
        }
        for gas_used in self.transaction_gas_used {
            metrics.record_transaction_gas_used(gas_used);
        }
        for size_bytes in self.transaction_sizes_bytes {
            metrics.record_transaction_size_bytes(size_bytes);
        }
        for size_bytes in self.transaction_da_sizes_bytes {
            metrics.record_transaction_da_size_bytes(size_bytes);
        }
        metrics
            .transaction_over_limits_rejections_total
            .increment(self.transaction_over_limits_rejections);
        metrics
            .transaction_uncompressed_size_rejections_total
            .increment(self.transaction_uncompressed_size_rejections);
        metrics
            .transaction_conditional_invalid_rejections_total
            .increment(self.transaction_conditional_invalid_rejections);
        metrics
            .transaction_blob_or_deposit_rejections_total
            .increment(self.transaction_blob_or_deposit_rejections);
        metrics
            .transaction_verified_gas_limit_rejections_total
            .increment(self.transaction_verified_gas_limit_rejections);
        metrics
            .transaction_duplicate_nullifier_rejections_total
            .increment(self.transaction_duplicate_nullifier_rejections);
        metrics
            .transaction_nonce_too_low_rejections_total
            .increment(self.transaction_nonce_too_low_rejections);
        metrics
            .transaction_invalid_descendant_rejections_total
            .increment(self.transaction_invalid_descendant_rejections);
        metrics
            .spend_nullifiers_attempts_total
            .increment(self.spend_nullifiers_attempts);
        metrics
            .spend_nullifiers_success_total
            .increment(self.spend_nullifiers_success);
        metrics
            .spend_nullifiers_failure_total
            .increment(self.spend_nullifiers_failure);
        for duration in self.spend_nullifiers_durations {
            metrics.record_spend_nullifiers_duration(duration);
        }
        for count in self.spend_nullifiers_counts {
            metrics.record_spend_nullifiers_count(count);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PayloadBuildOutcome {
    Better,
    Freeze,
    Aborted,
    Cancelled,
    Error,
}

impl PayloadBuildOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Better => "better",
            Self::Freeze => "freeze",
            Self::Aborted => "aborted",
            Self::Cancelled => "cancelled",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PayloadBuildRejectionReason {
    OverLimits,
    UncompressedSize,
    ConditionalInvalid,
    BlobOrDeposit,
    VerifiedGasLimit,
    DuplicateNullifier,
    NonceTooLow,
    InvalidDescendant,
}

impl PayloadBuildRejectionReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OverLimits => "over_limits",
            Self::UncompressedSize => "uncompressed_size",
            Self::ConditionalInvalid => "conditional_invalid",
            Self::BlobOrDeposit => "blob_or_deposit",
            Self::VerifiedGasLimit => "verified_gas_limit",
            Self::DuplicateNullifier => "duplicate_nullifier",
            Self::NonceTooLow => "nonce_too_low",
            Self::InvalidDescendant => "invalid_descendant",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PayloadBuildTaskOutcome {
    Success,
    Failure,
}

impl PayloadBuildTaskOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }
}

#[derive(Clone, Metrics)]
#[metrics(scope = "flashblocks.payload_build")]
pub struct PayloadBuildMetrics {
    /// Total number of payload build attempts started.
    pub attempts_total: Counter,
    /// Total number of payload build attempts that produced a better payload.
    pub better_outcomes_total: Counter,
    /// Total number of payload build attempts that produced a frozen payload.
    pub freeze_outcomes_total: Counter,
    /// Total number of payload build attempts aborted for being worse than the current best payload.
    pub aborted_outcomes_total: Counter,
    /// Total number of payload build attempts cancelled before completion.
    pub cancelled_outcomes_total: Counter,
    /// Total number of payload build attempts that failed with an error.
    pub error_outcomes_total: Counter,
    /// Histogram of total payload build duration in seconds.
    pub total_duration_seconds: Histogram,
    /// Latest total payload build duration in seconds.
    pub total_duration_seconds_latest: Gauge,
    /// Histogram of pre-execution changes duration in seconds.
    pub pre_execution_changes_duration_seconds: Histogram,
    /// Latest pre-execution changes duration in seconds.
    pub pre_execution_changes_duration_seconds_latest: Gauge,
    /// Histogram of sequencer transaction execution duration in seconds.
    pub sequencer_tx_execution_duration_seconds: Histogram,
    /// Latest sequencer transaction execution duration in seconds.
    pub sequencer_tx_execution_duration_seconds_latest: Gauge,
    /// Histogram of tx-pool selection and setup duration in seconds.
    pub txpool_fetch_duration_seconds: Histogram,
    /// Latest tx-pool selection and setup duration in seconds.
    pub txpool_fetch_duration_seconds_latest: Gauge,
    /// Histogram of best-transaction execution duration in seconds.
    pub best_tx_execution_duration_seconds: Histogram,
    /// Latest best-transaction execution duration in seconds.
    pub best_tx_execution_duration_seconds_latest: Gauge,
    /// Histogram of payload finalization duration in seconds.
    pub finalize_duration_seconds: Histogram,
    /// Latest payload finalization duration in seconds.
    pub finalize_duration_seconds_latest: Gauge,
    /// Histogram of state transition merge duration in seconds.
    pub merge_transitions_duration_seconds: Histogram,
    /// Latest state transition merge duration in seconds.
    pub merge_transitions_duration_seconds_latest: Gauge,
    /// Histogram of state root computation duration in seconds.
    pub state_root_duration_seconds: Histogram,
    /// Latest state root computation duration in seconds.
    pub state_root_duration_seconds_latest: Gauge,
    /// Histogram of block assembly duration in seconds.
    pub block_assembly_duration_seconds: Histogram,
    /// Latest block assembly duration in seconds.
    pub block_assembly_duration_seconds_latest: Gauge,
    /// Histogram of transactions considered per payload build.
    pub transactions_considered_per_build: Histogram,
    /// Latest number of transactions considered for the current payload build.
    pub transactions_considered_per_build_latest: Gauge,
    /// Histogram of transactions executed per payload build.
    pub transactions_executed_per_build: Histogram,
    /// Latest number of transactions executed for the current payload build.
    pub transactions_executed_per_build_latest: Gauge,
    /// Histogram of invalid transactions removed from the pool per payload build.
    pub invalid_transactions_removed_per_build: Histogram,
    /// Latest number of invalid transactions removed from the pool for the current payload build.
    pub invalid_transactions_removed_per_build_latest: Gauge,
    /// Histogram of single-transaction execution duration in seconds.
    pub transaction_execution_duration_seconds: Histogram,
    /// Histogram of gas used by executed transactions.
    pub transaction_gas_used: Histogram,
    /// Histogram of raw transaction size in bytes.
    pub transaction_size_bytes: Histogram,
    /// Histogram of estimated data availability transaction size in bytes.
    pub transaction_da_size_bytes: Histogram,
    /// Total number of transactions rejected because they exceeded payload limits.
    pub transaction_over_limits_rejections_total: Counter,
    /// Total number of transactions rejected because they exceeded the uncompressed block size limit.
    pub transaction_uncompressed_size_rejections_total: Counter,
    /// Total number of transactions rejected because conditional options validation failed.
    pub transaction_conditional_invalid_rejections_total: Counter,
    /// Total number of transactions rejected because they were blob or deposit transactions.
    pub transaction_blob_or_deposit_rejections_total: Counter,
    /// Total number of transactions rejected because they exceeded the verified gas budget.
    pub transaction_verified_gas_limit_rejections_total: Counter,
    /// Total number of transactions rejected because they reused a nullifier hash.
    pub transaction_duplicate_nullifier_rejections_total: Counter,
    /// Total number of transactions skipped because their nonce was too low.
    pub transaction_nonce_too_low_rejections_total: Counter,
    /// Total number of transactions rejected together with their descendants after validation failure.
    pub transaction_invalid_descendant_rejections_total: Counter,
    /// Total number of spend-nullifiers builder transactions attempted.
    pub spend_nullifiers_attempts_total: Counter,
    /// Total number of spend-nullifiers builder transactions that executed successfully.
    pub spend_nullifiers_success_total: Counter,
    /// Total number of spend-nullifiers builder transactions that failed to build or execute.
    pub spend_nullifiers_failure_total: Counter,
    /// Histogram of spend-nullifiers builder transaction duration in seconds.
    pub spend_nullifiers_duration_seconds: Histogram,
    /// Latest spend-nullifiers builder transaction duration in seconds.
    pub spend_nullifiers_duration_seconds_latest: Gauge,
    /// Histogram of nullifier count included in the spend-nullifiers builder transaction.
    pub spend_nullifiers_nullifier_count: Histogram,
    /// Latest nullifier count included in the spend-nullifiers builder transaction.
    pub spend_nullifiers_nullifier_count_latest: Gauge,
    /// Total number of committed payloads.
    pub committed_payloads_total: Counter,
    /// Histogram of committed payload size in bytes.
    pub committed_payload_size_bytes: Histogram,
    /// Latest committed payload size in bytes.
    pub committed_payload_size_bytes_latest: Gauge,
    /// Histogram of committed payload gas used.
    pub committed_payload_gas_used: Histogram,
    /// Latest committed payload gas used.
    pub committed_payload_gas_used_latest: Gauge,
    /// Histogram of committed payload transaction count.
    pub committed_payload_transaction_count: Histogram,
    /// Latest committed payload transaction count.
    pub committed_payload_transaction_count_latest: Gauge,
    /// Histogram of committed payload fees in wei.
    pub committed_payload_fees_wei: Histogram,
    /// Latest committed payload fees in wei.
    pub committed_payload_fees_wei_latest: Gauge,
    /// Histogram of committed flashblock index values.
    pub committed_flashblock_index: Histogram,
    /// Latest committed flashblock index value.
    pub committed_flashblock_index_latest: Gauge,
}

impl PayloadBuildMetrics {
    pub fn increment_attempts(&self) {
        self.attempts_total.increment(1);
    }

    pub fn record_outcome(&self, outcome: PayloadBuildOutcome) {
        match outcome {
            PayloadBuildOutcome::Better => self.better_outcomes_total.increment(1),
            PayloadBuildOutcome::Freeze => self.freeze_outcomes_total.increment(1),
            PayloadBuildOutcome::Aborted => self.aborted_outcomes_total.increment(1),
            PayloadBuildOutcome::Cancelled => self.cancelled_outcomes_total.increment(1),
            PayloadBuildOutcome::Error => self.error_outcomes_total.increment(1),
        }
    }

    pub fn record_stage_duration(&self, stage: PayloadBuildStage, duration: Duration) {
        let duration_secs = duration.as_secs_f64();

        match stage {
            PayloadBuildStage::Total => {
                self.total_duration_seconds.record(duration_secs);
                self.total_duration_seconds_latest.set(duration_secs);
            }
            PayloadBuildStage::PreExecutionChanges => {
                self.pre_execution_changes_duration_seconds
                    .record(duration_secs);
                self.pre_execution_changes_duration_seconds_latest
                    .set(duration_secs);
            }
            PayloadBuildStage::SequencerTxExecution => {
                self.sequencer_tx_execution_duration_seconds
                    .record(duration_secs);
                self.sequencer_tx_execution_duration_seconds_latest
                    .set(duration_secs);
            }
            PayloadBuildStage::TxPoolFetch => {
                self.txpool_fetch_duration_seconds.record(duration_secs);
                self.txpool_fetch_duration_seconds_latest.set(duration_secs);
            }
            PayloadBuildStage::BestTxExecution => {
                self.best_tx_execution_duration_seconds
                    .record(duration_secs);
                self.best_tx_execution_duration_seconds_latest
                    .set(duration_secs);
            }
            PayloadBuildStage::Finalize => {
                self.finalize_duration_seconds.record(duration_secs);
                self.finalize_duration_seconds_latest.set(duration_secs);
            }
            PayloadBuildStage::MergeTransitions => {
                self.merge_transitions_duration_seconds
                    .record(duration_secs);
                self.merge_transitions_duration_seconds_latest
                    .set(duration_secs);
            }
            PayloadBuildStage::StateRoot => {
                self.state_root_duration_seconds.record(duration_secs);
                self.state_root_duration_seconds_latest.set(duration_secs);
            }
            PayloadBuildStage::BlockAssembly => {
                self.block_assembly_duration_seconds.record(duration_secs);
                self.block_assembly_duration_seconds_latest
                    .set(duration_secs);
            }
        }
    }

    pub fn record_transactions_considered_per_build(&self, count: u64) {
        self.transactions_considered_per_build.record(count as f64);
        self.transactions_considered_per_build_latest
            .set(count as f64);
    }

    pub fn record_transactions_executed_per_build(&self, count: u64) {
        self.transactions_executed_per_build.record(count as f64);
        self.transactions_executed_per_build_latest
            .set(count as f64);
    }

    pub fn record_invalid_transactions_removed_per_build(&self, count: u64) {
        self.invalid_transactions_removed_per_build
            .record(count as f64);
        self.invalid_transactions_removed_per_build_latest
            .set(count as f64);
    }

    pub fn record_transaction_execution_duration(&self, duration: Duration) {
        self.transaction_execution_duration_seconds
            .record(duration.as_secs_f64());
    }

    pub fn record_transaction_gas_used(&self, gas_used: u64) {
        self.transaction_gas_used.record(gas_used as f64);
    }

    pub fn record_transaction_size_bytes(&self, size_bytes: u64) {
        self.transaction_size_bytes.record(size_bytes as f64);
    }

    pub fn record_transaction_da_size_bytes(&self, size_bytes: u64) {
        self.transaction_da_size_bytes.record(size_bytes as f64);
    }

    pub fn increment_rejection(&self, reason: PayloadBuildRejectionReason) {
        match reason {
            PayloadBuildRejectionReason::OverLimits => {
                self.transaction_over_limits_rejections_total.increment(1)
            }
            PayloadBuildRejectionReason::UncompressedSize => self
                .transaction_uncompressed_size_rejections_total
                .increment(1),
            PayloadBuildRejectionReason::ConditionalInvalid => self
                .transaction_conditional_invalid_rejections_total
                .increment(1),
            PayloadBuildRejectionReason::BlobOrDeposit => self
                .transaction_blob_or_deposit_rejections_total
                .increment(1),
            PayloadBuildRejectionReason::VerifiedGasLimit => self
                .transaction_verified_gas_limit_rejections_total
                .increment(1),
            PayloadBuildRejectionReason::DuplicateNullifier => self
                .transaction_duplicate_nullifier_rejections_total
                .increment(1),
            PayloadBuildRejectionReason::NonceTooLow => {
                self.transaction_nonce_too_low_rejections_total.increment(1)
            }
            PayloadBuildRejectionReason::InvalidDescendant => self
                .transaction_invalid_descendant_rejections_total
                .increment(1),
        }
    }

    pub fn increment_spend_nullifiers_attempts(&self) {
        self.spend_nullifiers_attempts_total.increment(1);
    }

    pub fn record_spend_nullifiers_outcome(&self, outcome: PayloadBuildTaskOutcome) {
        match outcome {
            PayloadBuildTaskOutcome::Success => self.spend_nullifiers_success_total.increment(1),
            PayloadBuildTaskOutcome::Failure => self.spend_nullifiers_failure_total.increment(1),
        }
    }

    pub fn record_spend_nullifiers_duration(&self, duration: Duration) {
        let duration_secs = duration.as_secs_f64();

        self.spend_nullifiers_duration_seconds.record(duration_secs);
        self.spend_nullifiers_duration_seconds_latest
            .set(duration_secs);
    }

    pub fn record_spend_nullifiers_count(&self, count: u64) {
        self.spend_nullifiers_nullifier_count.record(count as f64);
        self.spend_nullifiers_nullifier_count_latest
            .set(count as f64);
    }

    pub fn record_committed_payload(
        &self,
        size_bytes: u64,
        gas_used: u64,
        tx_count: u64,
        fees_wei: f64,
        flashblock_index: u64,
    ) {
        self.committed_payloads_total.increment(1);
        self.committed_payload_size_bytes.record(size_bytes as f64);
        self.committed_payload_size_bytes_latest
            .set(size_bytes as f64);
        self.committed_payload_gas_used.record(gas_used as f64);
        self.committed_payload_gas_used_latest.set(gas_used as f64);
        self.committed_payload_transaction_count
            .record(tx_count as f64);
        self.committed_payload_transaction_count_latest
            .set(tx_count as f64);
        self.committed_payload_fees_wei.record(fees_wei);
        self.committed_payload_fees_wei_latest.set(fees_wei);
        self.committed_flashblock_index
            .record(flashblock_index as f64);
        self.committed_flashblock_index_latest
            .set(flashblock_index as f64);
    }
}

/// RAII guard that enters a [`tracing::Span`] on creation. On drop it:
/// 1. Records `duration_ms` on the tracing span
/// 2. Records elapsed milliseconds to the provided [`Histogram`]
pub struct MetricsSpan {
    inner: tracing::span::EnteredSpan,
    start: Instant,
    histogram: Histogram,
}

impl MetricsSpan {
    /// Enter `span` and start the timer. `histogram` receives elapsed
    /// milliseconds on drop.
    ///
    /// The histogram can carry labels — pass one created with
    /// `metrics::histogram!("name", "label" => "value")` to attribute labels.
    pub fn new(span: tracing::Span, histogram: Histogram) -> Self {
        Self {
            inner: span.entered(),
            start: Instant::now(),
            histogram,
        }
    }

    /// Record a field on the underlying tracing span.
    pub fn record<V: tracing::field::Value>(&self, field: &str, value: V) {
        self.inner.record(field, value);
    }
}

impl Drop for MetricsSpan {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        self.inner.record("duration_ms", elapsed.as_millis() as u64);
        self.histogram.record(elapsed.as_millis() as f64);
    }
}

/// Execute `f` inside a metered tracing span. The span is entered before `f`
/// runs and duration is recorded (both on the span and as a histogram) on
/// completion. `f` receives a [`MetricsSpan`] reference for recording dynamic
/// span fields mid-execution.
pub fn metered_fn<F, R>(span: tracing::Span, histogram: Histogram, f: F) -> R
where
    F: FnOnce(&MetricsSpan) -> R,
{
    let guard = MetricsSpan::new(span, histogram);
    f(&guard)
}
