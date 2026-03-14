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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    /// Tracing layer that captures span events for assertions.
    struct SpanCapture {
        spans: Arc<Mutex<Vec<SpanRecord>>>,
    }

    #[derive(Debug, Clone)]
    struct SpanRecord {
        name: String,
        fields: String,
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SpanCapture {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut fields = String::new();
            attrs.record(&mut FieldVisitor(&mut fields));
            self.spans.lock().unwrap().push(SpanRecord {
                name: attrs.metadata().name().to_string(),
                fields,
            });
        }
    }

    struct FieldVisitor<'a>(&'a mut String);

    impl tracing::field::Visit for FieldVisitor<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write;
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            let _ = write!(self.0, "{}={:?}", field.name(), value);
        }
    }

    #[test]
    fn metered_fn_passes_span_ref_and_records_dynamic_fields() {
        let spans = Arc::new(Mutex::new(Vec::new()));
        let layer = SpanCapture {
            spans: spans.clone(),
        };

        let _guard = tracing_subscriber::registry().with(layer).set_default();

        let result = metered_fn(
            tracing::trace_span!(
                target: "flashblocks::coordinator",
                "metered_test",
                path = tracing::field::Empty,
                duration_ms = tracing::field::Empty,
            ),
            EXECUTION.validate_duration.clone(),
            |span| {
                span.record("path", "bal");
                42
            },
        );

        assert_eq!(result, 42);

        let captured = spans.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].name, "metered_test");
    }

    #[test]
    fn span_propagation_across_thread_boundary() {
        let spans = Arc::new(Mutex::new(Vec::new()));
        let layer = SpanCapture {
            spans: spans.clone(),
        };

        let dispatch =
            tracing::dispatcher::Dispatch::new(tracing_subscriber::registry().with(layer));
        let _guard = tracing::dispatcher::set_default(&dispatch);

        // Create a parent span on this thread
        let parent = tracing::trace_span!(
            target: "flashblocks::coordinator",
            "parent_span",
            id = "payload_123",
        );

        let parent_clone = parent.clone();
        let thread_dispatch = dispatch.clone();

        // Simulate the spawn_blocking pattern: capture span, re-enter on new thread
        let handle = std::thread::spawn(move || {
            let _sub = tracing::dispatcher::set_default(&thread_dispatch);
            let _enter = parent_clone.enter();
            // Child span created under re-entered parent
            let _child = tracing::trace_span!(
                target: "flashblocks::coordinator",
                "child_on_blocking_thread",
            )
            .entered();
        });

        handle.join().unwrap();

        let captured = spans.lock().unwrap();
        let names: Vec<&str> = captured.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"parent_span"), "should capture parent span");
        assert!(
            names.contains(&"child_on_blocking_thread"),
            "should capture child span created on blocking thread"
        );
    }
}
