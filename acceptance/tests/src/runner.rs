//! Executes the code-derived acceptance-test catalog against an [`Env`] or,
//! for the fork-matrix sweep, against a sequence of provisioned cells.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{StreamExt, stream};
use tracing::{Instrument, info, info_span, warn};

use crate::{
    catalog::{AcceptanceTest, Applicability, Catalog},
    context::{Metric, RunConfig, Skipped, TestCtx},
    env::Env,
    report::{Report, Status, TestResult},
    target::{AcceptanceTarget, MatrixCell},
};

/// Environment variable that makes flaky tests fail normally.
pub const FAIL_FLAKY_TESTS_ENV: &str = "ACCEPTANCE_FAIL_FLAKY_TESTS";

/// Environment variable overriding intra-cell test concurrency.
pub const CONCURRENCY_ENV: &str = "ACCEPTANCE_CONCURRENCY";

/// Default intra-cell test concurrency when none is configured.
const DEFAULT_CONCURRENCY: usize = 4;

/// Filters and knobs applied after catalog collection.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    /// Run only tests whose module/package path contains this substring.
    pub package_filter: Option<String>,
    /// Run only tests whose name contains this substring.
    pub name_filter: Option<String>,
    /// Maximum number of non-serial tests to run concurrently within a cell.
    /// Falls back to `ACCEPTANCE_CONCURRENCY`, then [`DEFAULT_CONCURRENCY`].
    pub concurrency: Option<usize>,
}

impl RunOptions {
    fn selects(&self, test: &AcceptanceTest) -> bool {
        if let Some(filter) = &self.package_filter
            && !test.package.contains(filter.as_str())
        {
            return false;
        }
        if let Some(filter) = &self.name_filter
            && !test.name.contains(filter.as_str())
        {
            return false;
        }
        true
    }

    fn resolved_concurrency(&self) -> usize {
        self.concurrency
            .or_else(|| {
                std::env::var(CONCURRENCY_ENV)
                    .ok()
                    .and_then(|v| v.parse().ok())
            })
            .filter(|&c| c > 0)
            .unwrap_or(DEFAULT_CONCURRENCY)
    }
}

/// The outcome of executing a single test, before report assembly.
#[derive(Debug)]
pub struct Execution {
    /// Pass/fail/skip status.
    pub status: Status,
    /// Wall-clock time spent executing the test.
    pub duration: Duration,
    /// Failure detail, when the test failed.
    pub error: Option<String>,
    /// Skip reason, when the test skipped itself or was flaky.
    pub skip_reason: Option<String>,
    /// Metrics the test recorded.
    pub metrics: Vec<Metric>,
}

impl Execution {
    /// Interpret a test's result and harvested metrics into an [`Execution`].
    pub fn from_result(
        outcome: eyre::Result<()>,
        duration: Duration,
        metrics: Vec<Metric>,
    ) -> Self {
        let (status, error, skip_reason) = match outcome {
            Ok(()) => (Status::Passed, None, None),
            Err(err) => match err.downcast_ref::<Skipped>() {
                Some(skip) => (Status::Skipped, None, Some(skip.0.clone())),
                None => (Status::Failed, Some(format!("{err:#}")), None),
            },
        };
        Self {
            status,
            duration,
            error,
            skip_reason,
            metrics,
        }
    }

    /// Build a failed execution from a message (e.g. a captured panic).
    pub fn failed(duration: Duration, error: impl Into<String>, metrics: Vec<Metric>) -> Self {
        Self {
            status: Status::Failed,
            duration,
            error: Some(error.into()),
            skip_reason: None,
            metrics,
        }
    }

    fn apply_flaky_policy(mut self, test: &AcceptanceTest, fail_flaky: bool) -> Self {
        if fail_flaky || self.status != Status::Failed {
            return self;
        }

        let Some(reason) = test.flaky else {
            return self;
        };

        let failure = self
            .error
            .take()
            .unwrap_or_else(|| "test failed".to_string());
        self.status = Status::Skipped;
        self.skip_reason = Some(format!("flaky: {reason}; failure: {failure}"));
        self
    }
}

/// Run the collected acceptance tests against a single pre-built environment.
///
/// The gating commitment and cell label are taken from the environment's
/// manifest. The caller owns the environment lifecycle (no teardown is run).
pub async fn run(env: Env, options: &RunOptions) -> Report {
    let env = Arc::new(env);
    let cell = MatrixCell::from_manifest(env.manifest());
    let summary = env.summary();
    let results = run_cell(&env, &cell, None, options).await;
    Report::new(summary, results)
}

/// Run the suite across a sequence of matrix cells.
///
/// For each cell the `target` is asked to [`provision`](AcceptanceTarget::provision)
/// an environment; the applicable subset of the catalog runs against it; then the
/// environment is torn down before the next cell. Results are labelled per cell
/// and aggregated into one [`Report`].
pub async fn run_matrix(
    target: &dyn AcceptanceTarget,
    cells: &[MatrixCell],
    options: &RunOptions,
) -> eyre::Result<Report> {
    let mut results = Vec::new();
    let mut summary = None;

    for cell in cells {
        info!(cell = %cell.label(), "provisioning acceptance cell");
        let provisioned = target.provision(cell.clone()).await?;
        let (env, teardown) = provisioned.into_parts();
        let env = Arc::new(env);
        summary.get_or_insert_with(|| env.summary());

        let cell_results = run_cell(&env, cell, Some(cell.label()), options).await;
        results.extend(cell_results);

        // Drop our env handle before teardown so the devnet can shut down cleanly.
        drop(env);
        if let Some(teardown) = teardown
            && let Err(err) = teardown().await
        {
            warn!(cell = %cell.label(), error = %format!("{err:#}"), "cell teardown failed");
        }
    }

    let summary = match summary {
        Some(summary) => summary,
        None => eyre::eyre::bail!("fork matrix had no cells to run"),
    };
    Ok(Report::new(summary, results))
}

/// Run the applicable subset of the catalog against one provisioned cell.
async fn run_cell(
    env: &Arc<Env>,
    cell: &MatrixCell,
    cell_label: Option<String>,
    options: &RunOptions,
) -> Vec<TestResult> {
    let catalog = Catalog::collect();
    let commitment = cell.commitment();
    let run_config = RunConfig::from_env();
    let fail_flaky = fail_flaky_tests();

    let mut results = Vec::new();
    let mut parallel = Vec::new();
    let mut serial = Vec::new();

    for (index, test) in catalog.iter().enumerate() {
        if !options.selects(test) {
            continue;
        }
        match test.requirements.applicability(&commitment) {
            Applicability::Applicable => {
                if test.requirements.serial {
                    serial.push((index as u32, test));
                } else {
                    parallel.push((index as u32, test));
                }
            }
            Applicability::Skip(reason) => {
                results.push(gated_result(test, &cell_label, Status::Skipped, reason));
            }
            Applicability::Invalid(reason) => {
                results.push(gated_result(
                    test,
                    &cell_label,
                    Status::Failed,
                    format!("invalid test requirements: {reason}"),
                ));
            }
        }
    }

    // Non-serial tests run with bounded concurrency; each gets a fresh context
    // and an isolated derived signer so they do not collide on the shared chain.
    let concurrency = options.resolved_concurrency();
    let parallel_results: Vec<TestResult> =
        stream::iter(parallel.into_iter().map(|(index, test)| {
            let env = env.clone();
            let cell_label = cell_label.clone();
            async move { evaluate(test, env, run_config, index, fail_flaky, cell_label).await }
        }))
        .buffer_unordered(concurrency)
        .collect()
        .await;
    results.extend(parallel_results);

    // Serial tests run one at a time, after the parallel batch.
    for (index, test) in serial {
        results.push(
            evaluate(
                test,
                env.clone(),
                run_config,
                index,
                fail_flaky,
                cell_label.clone(),
            )
            .await,
        );
    }

    // Stable ordering for the report regardless of completion order.
    results.sort_by(|a, b| {
        a.cell
            .cmp(&b.cell)
            .then_with(|| a.package.cmp(&b.package))
            .then_with(|| a.name.cmp(&b.name))
    });
    results
}

/// Build a [`TestResult`] for a test that was gated before execution.
fn gated_result(
    test: &AcceptanceTest,
    cell_label: &Option<String>,
    status: Status,
    detail: String,
) -> TestResult {
    let (error, skip_reason) = match status {
        Status::Failed => (Some(detail), None),
        _ => (None, Some(detail)),
    };
    TestResult {
        name: test.name.to_string(),
        package: test.package.to_string(),
        location: format!("{}:{}", test.location.file, test.location.line),
        cell: cell_label.clone(),
        flaky: test.flaky.map(ToString::to_string),
        status,
        duration_ms: 0,
        error,
        skip_reason,
        metrics: Vec::new(),
    }
}

async fn evaluate(
    test: &'static AcceptanceTest,
    env: Arc<Env>,
    run_config: RunConfig,
    test_index: u32,
    fail_flaky: bool,
    cell_label: Option<String>,
) -> TestResult {
    let span = info_span!(
        "acceptance_test",
        test = test.name,
        package = test.package,
        cell = cell_label.as_deref().unwrap_or("")
    );
    async move {
        info!("running acceptance test");
        let ctx = TestCtx::new(env, run_config, test_index);
        let execution = execute(test, ctx)
            .await
            .apply_flaky_policy(test, fail_flaky);
        log_execution(test, &execution);

        TestResult {
            name: test.name.to_string(),
            package: test.package.to_string(),
            location: format!("{}:{}", test.location.file, test.location.line),
            cell: cell_label,
            flaky: test.flaky.map(ToString::to_string),
            status: execution.status,
            duration_ms: execution.duration.as_millis(),
            error: execution.error,
            skip_reason: execution.skip_reason,
            metrics: execution.metrics,
        }
    }
    .instrument(span)
    .await
}

async fn execute(test: &'static AcceptanceTest, ctx: Arc<TestCtx>) -> Execution {
    let started = Instant::now();
    let task_ctx = ctx.clone();
    let joined = tokio::spawn(async move { (test.run)(task_ctx).await }).await;
    let duration = started.elapsed();
    let metrics = ctx.take_metrics();

    match joined {
        Ok(outcome) => Execution::from_result(outcome, duration, metrics),
        Err(join_err) => Execution::failed(duration, panic_message(join_err), metrics),
    }
}

fn log_execution(test: &AcceptanceTest, execution: &Execution) {
    match execution.status {
        Status::Passed => {
            info!(test = test.name, elapsed = ?execution.duration, "acceptance test passed")
        }
        Status::Failed => warn!(
            test = test.name,
            elapsed = ?execution.duration,
            error = execution.error.as_deref().unwrap_or_default(),
            "acceptance test failed"
        ),
        Status::Skipped => info!(
            test = test.name,
            reason = execution.skip_reason.as_deref().unwrap_or_default(),
            "acceptance test skipped"
        ),
    }
}

fn fail_flaky_tests() -> bool {
    std::env::var(FAIL_FLAKY_TESTS_ENV)
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

fn panic_message(join_err: tokio::task::JoinError) -> String {
    match join_err.try_into_panic() {
        Ok(panic) => {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "test panicked".to_string());
            format!("panicked: {msg}")
        }
        Err(err) => format!("test task was cancelled: {err}"),
    }
}
