use auto_impl::auto_impl;
use std::time::Duration;

/// General trait to collect metrics around flashblock execution.
///
/// This trait is necessary because both the flashblock builder and flashblock validation pipelines
/// use the `FlashblocksBlockBuilder::finish_with_bundle` fn but we need to distinguish metrics being
/// generated when actually creating the flashblock (flashblock building metrics) and metrics being
/// generated when validating it (flashblock validation metrics).
#[auto_impl(&mut)]
pub trait FlashblockExecutionMetrics {
    /// Record a flashblock execution stage.
    fn record_stage_duration(&mut self, stage: PayloadBuildStage, duration: Duration);
}

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
