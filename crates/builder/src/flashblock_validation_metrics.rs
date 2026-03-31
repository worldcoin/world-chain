use crate::metrics::{FlashblockExecutionMetrics, PayloadBuildStage};
use metrics::{Gauge, Histogram};
use metrics_derive::Metrics;
use std::time::Duration;

#[derive(Clone, Metrics)]
#[metrics(scope = "flashblocks.validation")]
pub struct FlashblockValidationMetrics {
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

impl FlashblockExecutionMetrics for FlashblockValidationMetrics {
    fn record_merge_transitions(&mut self, duration: Duration) {
        self.record_stage_duration(PayloadBuildStage::MergeTransitions, duration);
    }

    fn record_state_root(&mut self, duration: Duration) {
        self.record_stage_duration(PayloadBuildStage::StateRoot, duration);
    }

    fn record_block_assembly(&mut self, duration: Duration) {
        self.record_stage_duration(PayloadBuildStage::BlockAssembly, duration);
    }
}
