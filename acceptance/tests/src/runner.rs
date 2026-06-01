//! Executes the registered checks against an [`Env`] and builds a [`Report`].
//!
//! Selection, manifest gating, and report assembly live in [`run`]. *How* an
//! individual check is executed is abstracted behind the [`TestExecutor`]
//! trait, so the isolation/timeout/parallelism strategy is pluggable. The
//! default [`PanicIsolatedExecutor`] runs each check on its own task so a panic
//! is captured as a failure rather than aborting the run.

use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, Instant},
};

use tracing::{info, warn};
use world_chain_manifest::Requirement;

use crate::{
    context::{Metric, Skipped, TestCtx},
    env::Env,
    registry::{AcceptanceTest, Category},
    report::{Report, Status, TestResult},
};

/// Filters applied when selecting which registered checks to run.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    /// Restrict to these categories. `None` runs every category.
    pub categories: Option<BTreeSet<Category>>,
    /// Run only checks whose name contains this substring.
    pub name_filter: Option<String>,
}

impl RunOptions {
    fn selects(&self, test: &AcceptanceTest) -> bool {
        if let Some(categories) = &self.categories
            && !categories.contains(&test.category)
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
}

/// The outcome of executing a single check, before report assembly.
#[derive(Debug)]
pub struct Execution {
    /// Pass/fail/skip status.
    pub status: Status,
    /// Wall-clock time spent executing the check.
    pub duration: Duration,
    /// Failure detail, when the check failed.
    pub error: Option<String>,
    /// Skip reason, when the check skipped itself at run time.
    pub skip_reason: Option<String>,
    /// Metrics the check recorded.
    pub metrics: Vec<Metric>,
}

impl Execution {
    /// Interpret a check's result (and harvested metrics) into an [`Execution`].
    ///
    /// Shared by executors so they agree on how a [`Skipped`] error is mapped to
    /// a skip rather than a failure.
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
}

/// Strategy for executing a single acceptance check.
///
/// Implementors invoke `test.run(ctx)`, time it, harvest the context metrics,
/// and return an [`Execution`]. The default [`PanicIsolatedExecutor`] isolates
/// each check on its own task; an [`InlineExecutor`] runs it in place (useful in
/// unit tests or single-threaded contexts).
pub trait TestExecutor {
    /// Execute one check against its context.
    fn execute(
        &self,
        test: &'static AcceptanceTest,
        ctx: Arc<TestCtx>,
    ) -> impl Future<Output = Execution> + Send;
}

/// Default executor: runs each check on its own task so a panic is captured as
/// a failure instead of aborting the whole run.
#[derive(Clone, Copy, Debug, Default)]
pub struct PanicIsolatedExecutor;

impl TestExecutor for PanicIsolatedExecutor {
    async fn execute(&self, test: &'static AcceptanceTest, ctx: Arc<TestCtx>) -> Execution {
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
}

/// Executor that runs each check inline on the current task (no panic
/// isolation). A panic here aborts the run.
#[derive(Clone, Copy, Debug, Default)]
pub struct InlineExecutor;

impl TestExecutor for InlineExecutor {
    async fn execute(&self, test: &'static AcceptanceTest, ctx: Arc<TestCtx>) -> Execution {
        let started = Instant::now();
        let outcome = (test.run)(ctx.clone()).await;
        let duration = started.elapsed();
        let metrics = ctx.take_metrics();
        Execution::from_result(outcome, duration, metrics)
    }
}

/// Run the selected acceptance checks against `env` using the default
/// [`PanicIsolatedExecutor`].
pub async fn run(env: Env, options: &RunOptions) -> Report {
    run_with(env, options, &PanicIsolatedExecutor).await
}

/// Run the selected acceptance checks against `env` with a custom executor.
///
/// A check runs only when the manifest commits to every key in its `requires`
/// list; otherwise it is skipped. The report also flags committed requirements
/// that no registered check exercises.
pub async fn run_with<E: TestExecutor>(env: Env, options: &RunOptions, executor: &E) -> Report {
    let env = Arc::new(env);

    let mut results = Vec::new();
    for test in inventory::iter::<AcceptanceTest> {
        if !options.selects(test) {
            continue;
        }
        results.push(evaluate(test, &env, executor).await);
    }

    results.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then_with(|| a.name.cmp(&b.name))
    });

    let uncovered = uncovered_commitments(&env);
    Report::new(env.summary(), uncovered, results)
}

async fn evaluate<E: TestExecutor>(
    test: &'static AcceptanceTest,
    env: &Arc<Env>,
    executor: &E,
) -> TestResult {
    let requires: Vec<String> = test.requires.iter().map(Requirement::label).collect();

    // A feature requirement naming no known feature is a declaration bug.
    let unknown = env.unknown(test.requires);
    if !unknown.is_empty() {
        warn!(
            check = test.name,
            ?unknown,
            "check declares unknown requirements"
        );
        return skipped_or_failed(
            test,
            requires,
            Status::Failed,
            Some(format!("unknown requirement(s): {}", unknown.join(", "))),
            None,
        );
    }

    // Skip checks the manifest does not commit to.
    let missing = env.missing(test.requires);
    if !missing.is_empty() {
        let reason = format!("manifest does not commit to: {}", missing.join(", "));
        info!(check = test.name, %reason, "skipping acceptance check");
        return skipped_or_failed(test, requires, Status::Skipped, None, Some(reason));
    }

    info!(check = test.name, category = %test.category, "running acceptance check");
    let ctx = TestCtx::new(env.clone());
    let execution = executor.execute(test, ctx).await;
    log_execution(test, &execution);

    TestResult {
        name: test.name.to_string(),
        category: test.category,
        status: execution.status,
        duration_ms: execution.duration.as_millis(),
        requires,
        error: execution.error,
        skip_reason: execution.skip_reason,
        metrics: execution.metrics,
    }
}

fn log_execution(test: &AcceptanceTest, execution: &Execution) {
    match execution.status {
        Status::Passed => {
            info!(check = test.name, elapsed = ?execution.duration, "acceptance check passed")
        }
        Status::Failed => warn!(
            check = test.name,
            elapsed = ?execution.duration,
            error = execution.error.as_deref().unwrap_or_default(),
            "acceptance check failed"
        ),
        Status::Skipped => info!(
            check = test.name,
            reason = execution.skip_reason.as_deref().unwrap_or_default(),
            "acceptance check skipped at run time"
        ),
    }
}

/// Build a result for a check that was resolved without executing it.
fn skipped_or_failed(
    test: &AcceptanceTest,
    requires: Vec<String>,
    status: Status,
    error: Option<String>,
    skip_reason: Option<String>,
) -> TestResult {
    TestResult {
        name: test.name.to_string(),
        category: test.category,
        status,
        duration_ms: 0,
        requires,
        error,
        skip_reason,
        metrics: Vec::new(),
    }
}

/// Committed requirements not referenced by any registered check.
fn uncovered_commitments(env: &Env) -> Vec<String> {
    let referenced: BTreeSet<&str> = inventory::iter::<AcceptanceTest>
        .into_iter()
        .flat_map(|test| test.requires.iter().map(Requirement::name))
        .collect();

    env.commitments()
        .committed_keys()
        .into_iter()
        .filter(|key| !referenced.contains(key.as_str()))
        .collect()
}

fn panic_message(join_err: tokio::task::JoinError) -> String {
    match join_err.try_into_panic() {
        Ok(panic) => {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "check panicked".to_string());
            format!("panicked: {msg}")
        }
        Err(err) => format!("check task was cancelled: {err}"),
    }
}
