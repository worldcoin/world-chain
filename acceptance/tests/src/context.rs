//! Per-test execution context.

use std::{
    ops::Deref,
    sync::{Arc, Mutex},
};

use alloy_signer_local::{MnemonicBuilder, PrivateKeySigner, coins_bip39::English};
use serde::Serialize;

use crate::env::Env;

/// Deterministic mnemonic used to derive per-test signers. Combined with the
/// run-level salt and the test index, it gives every test its own account so
/// tests running concurrently against a shared devnet do not collide on nonces.
const TEST_MNEMONIC: &str = "test test test test test test test test test test test junk";

/// Run-level configuration threaded into every [`TestCtx`].
#[derive(Clone, Copy, Debug, Default)]
pub struct RunConfig {
    /// When `true`, a run-time [`TestCtx::skip`] is converted into a failure
    /// because an orchestrator selected this test and expected it to run.
    /// Declarative pre-gating (unmet `requires_*`) still skips regardless.
    pub expect_preconditions: bool,
    /// Salt applied to per-test signer derivation, for reproducible isolation
    /// across runs. Sourced from `ACCEPTANCE_KEYS_SALT`.
    pub keys_salt: u32,
}

impl RunConfig {
    /// Build a [`RunConfig`] from the acceptance environment variables.
    pub fn from_env() -> Self {
        Self {
            expect_preconditions: env_flag("ACCEPTANCE_EXPECT_PRECONDITIONS_MET"),
            keys_salt: std::env::var("ACCEPTANCE_KEYS_SALT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        }
    }
}

/// Read a boolean acceptance flag from the environment, treating the usual
/// truthy spellings as `true` and an unset/other value as `false`.
pub(crate) fn env_flag(key: &str) -> bool {
    std::env::var(key)
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

/// A value recorded by a test for inclusion in the report (e.g. a measured
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

/// A named metric emitted by a test.
#[derive(Clone, Debug, Serialize)]
pub struct Metric {
    pub key: String,
    pub value: MetricValue,
}

/// Context handed to every test.
///
/// Derefs to the [`Env`], so tests call `ctx.chain_id().await` directly, and
/// additionally lets a test record metrics, derive an isolated signer, and skip
/// at run time.
pub struct TestCtx {
    env: Arc<Env>,
    config: RunConfig,
    /// Stable index of this test within the run, used to namespace derived keys.
    test_index: u32,
    metrics: Mutex<Vec<Metric>>,
}

impl TestCtx {
    /// Wrap a shared environment in a fresh context.
    pub fn new(env: Arc<Env>, config: RunConfig, test_index: u32) -> Arc<Self> {
        Arc::new(Self {
            env,
            config,
            test_index,
            metrics: Mutex::new(Vec::new()),
        })
    }

    /// The environment under test.
    pub fn env(&self) -> &Env {
        &self.env
    }

    /// A deterministic signer unique to this test (and run salt), so concurrent
    /// tests against a shared network do not collide on account nonces. `index`
    /// distinguishes multiple signers within a single test.
    pub fn signer(&self, index: u32) -> eyre::Result<PrivateKeySigner> {
        // Namespace by test index so two tests never derive the same account.
        let derivation = self
            .config
            .keys_salt
            .wrapping_add(self.test_index.wrapping_mul(64))
            .wrapping_add(index);
        Ok(MnemonicBuilder::<English>::default()
            .phrase(TEST_MNEMONIC)
            .index(derivation)?
            .build()?)
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

    /// Drain the metrics recorded by the test.
    pub(crate) fn take_metrics(&self) -> Vec<Metric> {
        std::mem::take(&mut self.metrics.lock().expect("metrics mutex poisoned"))
    }
}

impl TestCtx {
    /// Skip the test at run time with a reason (e.g. an optional endpoint is
    /// not configured). Returns an error the runner recognises as a skip.
    ///
    /// When the run sets `ACCEPTANCE_EXPECT_PRECONDITIONS_MET`, this is promoted
    /// to a failure instead — an orchestrator selected the test and expected its
    /// preconditions to hold (mirrors Optimism's `DEVNET_EXPECT_PRECONDITIONS_MET`).
    pub fn skip(&self, reason: impl Into<String>) -> eyre::Report {
        let reason = reason.into();
        if self.config.expect_preconditions {
            eyre::eyre::eyre!("precondition not met (expected to be met): {reason}")
        } else {
            eyre::eyre::Report::new(Skipped(reason))
        }
    }

    /// Skip the test when `condition` holds; otherwise continue.
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
