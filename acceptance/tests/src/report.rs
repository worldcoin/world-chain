//! The acceptance report produced by a run.

use std::fmt::Write as _;

use serde::Serialize;

use crate::{context::Metric, env::EnvSummary, registry::Category};

/// Outcome of a single check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Passed,
    Failed,
    Skipped,
}

impl Status {
    fn symbol(self) -> &'static str {
        match self {
            Status::Passed => "PASS",
            Status::Failed => "FAIL",
            Status::Skipped => "SKIP",
        }
    }
}

/// Result of running one check.
#[derive(Debug, Serialize)]
pub struct TestResult {
    pub name: String,
    pub category: Category,
    pub status: Status,
    pub duration_ms: u128,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<Metric>,
}

/// Aggregate pass/fail/skip counts.
#[derive(Debug, Default, Serialize)]
pub struct Totals {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// Full acceptance report for a single run.
#[derive(Debug, Serialize)]
pub struct Report {
    pub generated_at: String,
    pub environment: EnvSummary,
    pub totals: Totals,
    /// Committed requirements that no registered check exercises.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uncovered_commitments: Vec<String>,
    pub results: Vec<TestResult>,
}

impl Report {
    /// Assemble a report from per-check results.
    pub fn new(
        environment: EnvSummary,
        uncovered_commitments: Vec<String>,
        results: Vec<TestResult>,
    ) -> Self {
        let mut totals = Totals {
            total: results.len(),
            ..Default::default()
        };
        for result in &results {
            match result.status {
                Status::Passed => totals.passed += 1,
                Status::Failed => totals.failed += 1,
                Status::Skipped => totals.skipped += 1,
            }
        }

        Self {
            generated_at: chrono::Utc::now().to_rfc3339(),
            environment,
            totals,
            uncovered_commitments,
            results,
        }
    }

    /// Whether every executed check passed (skips do not fail the run).
    pub fn passed(&self) -> bool {
        self.totals.failed == 0
    }

    /// Whether the run passed under strict coverage: every check passed and
    /// every committed requirement is exercised by at least one check.
    pub fn passed_strict(&self) -> bool {
        self.passed() && self.uncovered_commitments.is_empty()
    }

    /// Serialize the report to pretty JSON.
    pub fn to_json(&self) -> eyre::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Render the report as a Markdown document.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# World Chain Acceptance Report\n");
        let _ = writeln!(out, "- network: `{}`", self.environment.network);
        let _ = writeln!(out, "- chain source: `{}`", self.environment.chain_source);
        let _ = writeln!(out, "- generated: `{}`", self.generated_at);
        if let Some(chain_id) = self.environment.expected_chain_id {
            let _ = writeln!(out, "- expected chain id: `{chain_id}`");
        }
        let _ = writeln!(out, "- hardfork: `{}`", self.environment.hardfork);
        let _ = writeln!(
            out,
            "- features: {}",
            join_or_none(&self.environment.features)
        );
        if !self.uncovered_commitments.is_empty() {
            let _ = writeln!(
                out,
                "- ⚠️ uncovered commitments (no check exercises these): {}",
                self.uncovered_commitments.join(", ")
            );
        }
        let _ = writeln!(
            out,
            "- result: **{} passed, {} failed, {} skipped** of {}\n",
            self.totals.passed, self.totals.failed, self.totals.skipped, self.totals.total
        );

        let _ = writeln!(
            out,
            "| Status | Category | Check | Requires | Duration | Details |"
        );
        let _ = writeln!(out, "| --- | --- | --- | --- | --- | --- |");
        for result in &self.results {
            let details = result
                .error
                .as_deref()
                .or(result.skip_reason.as_deref())
                .map(|d| d.replace('|', "\\|").replace('\n', " "))
                .unwrap_or_else(|| metrics_inline(&result.metrics));
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} ms | {} |",
                result.status.symbol(),
                result.category,
                result.name,
                join_or_dash(&result.requires),
                result.duration_ms,
                details
            );
        }

        out
    }

    /// Render a compact summary suitable for stdout.
    pub fn to_summary(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "\nAcceptance report for network `{}` ({})",
            self.environment.network, self.environment.chain_source
        );
        for result in &self.results {
            let detail = match (&result.error, &result.skip_reason) {
                (Some(error), _) => format!("  ({error})"),
                (_, Some(reason)) => format!("  ({reason})"),
                _ if !result.metrics.is_empty() => {
                    format!("  [{}]", metrics_inline(&result.metrics))
                }
                _ => String::new(),
            };
            let _ = writeln!(
                out,
                "  {:<4} [{:^11}] {} {} ms{}",
                result.status.symbol(),
                result.category,
                result.name,
                result.duration_ms,
                detail
            );
        }
        if !self.uncovered_commitments.is_empty() {
            let _ = writeln!(
                out,
                "  !!   uncovered commitments: {}",
                self.uncovered_commitments.join(", ")
            );
        }
        let _ = writeln!(
            out,
            "  ----\n  {} passed, {} failed, {} skipped of {}",
            self.totals.passed, self.totals.failed, self.totals.skipped, self.totals.total
        );
        out
    }
}

fn metrics_inline(metrics: &[Metric]) -> String {
    metrics
        .iter()
        .map(|m| format!("{}={}", m.key, m.value))
        .collect::<Vec<_>>()
        .join(", ")
}

fn join_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

fn join_or_dash(items: &[String]) -> String {
    if items.is_empty() {
        "-".to_string()
    } else {
        items.join(", ")
    }
}
