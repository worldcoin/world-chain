use crate::metrics::{FlashblockExecutionMetrics, PayloadBuildStage};
use metrics::{Counter, Gauge, Histogram};
use metrics_derive::Metrics;
use std::{sync::Arc, time::Duration};

#[derive(Debug, Default)]
pub struct FlashblockValidationAttemptMetrics {
    total_duration: Option<Duration>,
    pre_execution_changes_duration: Option<Duration>,
    tx_execution_duration: Option<Duration>,
    finalize_duration: Option<Duration>,
    merge_transitions_duration: Option<Duration>,
    state_root_duration: Option<Duration>,
    block_assembly_duration: Option<Duration>,
}

impl FlashblockValidationAttemptMetrics {
    pub fn record_stage_duration(&mut self, stage: PayloadBuildStage, duration: Duration) {
        match stage {
            PayloadBuildStage::Total => self.total_duration = Some(duration),
            PayloadBuildStage::PreExecutionChanges => {
                self.pre_execution_changes_duration = Some(duration);
            }
            PayloadBuildStage::SequencerTxExecution => {
                self.tx_execution_duration = Some(duration);
            }
            PayloadBuildStage::Finalize => self.finalize_duration = Some(duration),
            PayloadBuildStage::MergeTransitions => {
                self.merge_transitions_duration = Some(duration);
            }
            PayloadBuildStage::StateRoot => self.state_root_duration = Some(duration),
            PayloadBuildStage::BlockAssembly => self.block_assembly_duration = Some(duration),
            _ => {
                unreachable!(
                    "we don't have TxPoolFetch and BestTxExecution in flashblock validation"
                );
            }
        }
    }

    pub fn publish(self, metrics: &FlashblockValidationMetrics) {
        if let Some(duration) = self.total_duration {
            metrics.record_stage_duration(PayloadBuildStage::Total, duration);
        }
        if let Some(duration) = self.pre_execution_changes_duration {
            metrics.record_stage_duration(PayloadBuildStage::PreExecutionChanges, duration);
        }
        if let Some(duration) = self.tx_execution_duration {
            metrics.record_stage_duration(PayloadBuildStage::SequencerTxExecution, duration);
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
    }
}

#[derive(Clone, Metrics)]
#[metrics(scope = "flashblocks.validation")]
pub struct FlashblockValidationMetrics {
    /// The number of already processed flashblocks that don't need to be re-executed.
    pub already_processed_flashblocks: Counter,
    /// Total number of flashblock validation errors before completion.
    pub validation_errors_total: Counter,
    /// Histogram of full flashblock validation duration in seconds.
    pub full_flashblock_validation_duration_seconds: Histogram,
    /// Latest full flashblock validation duration in seconds.
    pub full_flashblock_validation_duration_seconds_latest: Gauge,
    /// Histogram of pre-execution changes duration in seconds.
    pub pre_execution_changes_duration_seconds: Histogram,
    /// Latest pre-execution changes duration in seconds.
    pub pre_execution_changes_duration_seconds_latest: Gauge,
    /// Histogram of transactions execution duration in seconds.
    pub tx_execution_duration_seconds: Histogram,
    /// Latest transactions execution duration in seconds.
    pub tx_execution_duration_seconds_latest: Gauge,
    /// Histogram of flashblock finalization duration in seconds.
    pub finalize_duration_seconds: Histogram,
    /// Latest flashblock finalization duration in seconds.
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
}

impl FlashblockValidationMetrics {
    pub fn increment_already_processed_flashblocks(&self) {
        self.already_processed_flashblocks.increment(1);
    }

    pub fn increment_validation_errors(&self) {
        self.validation_errors_total.increment(1);
    }

    pub fn record_stage_duration(&self, stage: PayloadBuildStage, duration: Duration) {
        let duration_secs = duration.as_secs_f64();
        match stage {
            PayloadBuildStage::Total => {
                self.full_flashblock_validation_duration_seconds
                    .record(duration_secs);
                self.full_flashblock_validation_duration_seconds_latest
                    .set(duration_secs);
            }
            PayloadBuildStage::PreExecutionChanges => {
                self.pre_execution_changes_duration_seconds
                    .record(duration_secs);
                self.pre_execution_changes_duration_seconds_latest
                    .set(duration_secs);
            }
            PayloadBuildStage::SequencerTxExecution => {
                self.tx_execution_duration_seconds.record(duration_secs);
                self.tx_execution_duration_seconds_latest.set(duration_secs);
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
            _ => {
                unreachable!(
                    "we don't have TxPoolFetch and BestTxExecution in flashblock validation"
                );
            }
        }
    }
}

impl FlashblockExecutionMetrics for FlashblockValidationAttemptMetrics {
    fn record_stage_duration(&mut self, stage: PayloadBuildStage, duration: Duration) {
        self.record_stage_duration(stage, duration);
    }
}
