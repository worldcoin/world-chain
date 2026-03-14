//! Metrics and instrumentation for the flashblocks builder.

use metrics::{Counter, Histogram};
use metrics_derive::Metrics;
use std::{sync::LazyLock, time::Instant};

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
    pub payload_cache_hits: Counter,
}

/// Global singleton — zero lookup cost per call site.
pub static EXECUTION: LazyLock<ExecutionMetrics> = LazyLock::new(ExecutionMetrics::default);

/// RAII guard that enters a [`tracing::Span`] on creation. On drop it:
/// 1. Records `duration_ms` on the tracing span
/// 2. Records elapsed seconds to the provided [`Histogram`]
pub struct MetricsSpan {
    inner: tracing::span::EnteredSpan,
    start: Instant,
    histogram: Histogram,
}

impl MetricsSpan {
    /// Enter `span` and start the timer. `histogram` receives elapsed seconds on drop.
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
        self.histogram.record(elapsed.as_secs_f64());
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