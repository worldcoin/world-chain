use auto_impl::auto_impl;
use metrics::{Counter, Gauge, Histogram};
use metrics_derive::Metrics;
use std::{sync::LazyLock, time::Duration};

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

/// Process-wide metrics for the live pre-image witness oracle (`--witness.collect`).
#[derive(Clone, Metrics)]
#[metrics(scope = "world_chain.witness")]
pub struct WitnessMetrics {
    /// Witnesses captured from the block-import executor and handed to the collector.
    pub captured: Counter,
    /// Witnesses dropped at capture time because the collector channel was full or closed.
    pub dropped: Counter,
    /// Witnesses the collector failed to assemble, leaving a permanent hole in the cache.
    pub assembly_failed: Counter,
    /// Witnesses inserted into the cache.
    pub inserted: Counter,
    /// Witnesses evicted from the cache by the ring-buffer depth bound.
    pub evicted: Counter,
    /// Size of each cached witness, in bytes.
    pub witness_bytes: Histogram,
    /// Bytes of witness data currently retained by the cache.
    pub cache_bytes: Gauge,
    /// Witnesses currently retained by the cache.
    pub cache_len: Gauge,
    /// Lowest block number currently cached.
    pub cache_oldest_block: Gauge,
    /// Highest block number currently cached.
    pub cache_newest_block: Gauge,
    /// Range lookups served in full from the cache.
    pub range_hit: Counter,
    /// Range lookups rejected because at least one block in the range was missing.
    pub range_miss: Counter,
    /// Blocks absent from the cache, summed over every rejected range lookup.
    pub range_missing_blocks: Counter,
}

impl WitnessMetrics {
    /// Returns the process-wide witness metrics, registered on first use.
    ///
    /// Registration is lazy so the handles resolve against the Prometheus recorder installed at
    /// node startup; the first use is a block import, long after that.
    pub fn get() -> &'static Self {
        static METRICS: LazyLock<WitnessMetrics> = LazyLock::new(WitnessMetrics::default);
        &METRICS
    }
}
