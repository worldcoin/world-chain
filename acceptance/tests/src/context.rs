//! Per-check execution context.

use std::{
    ops::Deref,
    sync::{Arc, Mutex},
};

use serde::Serialize;

use crate::env::Env;

/// A value recorded by a check for inclusion in the report (e.g. a measured
/// block time or throughput number).
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum MetricValue {
    Int(i64),
    Float(f64),
    Text(String),
}

impl std::fmt::Display for MetricValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetricValue::Int(v) => write!(f, "{v}"),
            MetricValue::Float(v) => write!(f, "{v:.3}"),
            MetricValue::Text(v) => f.write_str(v),
        }
    }
}

/// A named metric emitted by a check.
#[derive(Clone, Debug, Serialize)]
pub struct Metric {
    pub key: String,
    pub value: MetricValue,
}

/// Context handed to every check.
///
/// Derefs to the [`Env`], so checks call `ctx.chain_id().await` directly, and
/// additionally lets a check record metrics that surface in the report.
pub struct TestCtx {
    env: Arc<Env>,
    metrics: Mutex<Vec<Metric>>,
}

impl TestCtx {
    /// Wrap a shared environment in a fresh context.
    pub fn new(env: Arc<Env>) -> Arc<Self> {
        Arc::new(Self {
            env,
            metrics: Mutex::new(Vec::new()),
        })
    }

    /// The environment under test.
    pub fn env(&self) -> &Env {
        &self.env
    }

    /// Record an integer metric.
    pub fn record_i64(&self, key: impl Into<String>, value: i64) {
        self.record(key, MetricValue::Int(value));
    }

    /// Record a floating-point metric.
    pub fn record_f64(&self, key: impl Into<String>, value: f64) {
        self.record(key, MetricValue::Float(value));
    }

    /// Record a textual metric.
    pub fn record_text(&self, key: impl Into<String>, value: impl Into<String>) {
        self.record(key, MetricValue::Text(value.into()));
    }

    /// Record an arbitrary metric value.
    pub fn record(&self, key: impl Into<String>, value: MetricValue) {
        self.metrics
            .lock()
            .expect("metrics mutex poisoned")
            .push(Metric {
                key: key.into(),
                value,
            });
    }

    /// Drain the metrics recorded by the check.
    pub(crate) fn take_metrics(&self) -> Vec<Metric> {
        std::mem::take(&mut self.metrics.lock().expect("metrics mutex poisoned"))
    }
}

impl TestCtx {
    /// Skip the check at run time with a reason (e.g. an optional endpoint is
    /// not configured). Returns an error the runner recognises as a skip rather
    /// than a failure.
    pub fn skip(&self, reason: impl Into<String>) -> eyre::Report {
        eyre::eyre::Report::new(Skipped(reason.into()))
    }

    /// Skip the check when `condition` holds; otherwise continue.
    pub fn skip_if(&self, condition: bool, reason: impl Into<String>) -> eyre::Result<()> {
        if condition {
            Err(self.skip(reason))
        } else {
            Ok(())
        }
    }
}

impl Deref for TestCtx {
    type Target = Env;

    fn deref(&self) -> &Self::Target {
        &self.env
    }
}

/// Error payload signalling a run-time skip. The runner downcasts to this to
/// distinguish an intentional skip from a failure.
#[derive(Debug)]
pub struct Skipped(pub String);

impl std::fmt::Display for Skipped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Skipped {}
