//! The acceptance report produced by a run.

use std::{collections::BTreeMap, fmt::Write as _};

use serde::Serialize;

use crate::{context::Metric, env::EnvSummary};

/// Outcome of a single test.
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

/// Result of running one test.
#[derive(Debug, Serialize)]
pub struct TestResult {
    pub name: String,
    pub package: String,
    pub location: String,
    /// Matrix cell label this result belongs to (e.g. `jovian+flashblocks`).
    /// `None` for a non-matrix single-cell run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flaky: Option<String>,
    pub status: Status,
    pub duration_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<Metric>,
}

/// Aggregate pass/fail/skip counts.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Totals {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

impl Totals {
    fn tally(&mut self, status: Status) {
        self.total += 1;
        match status {
            Status::Passed => self.passed += 1,
            Status::Failed => self.failed += 1,
            Status::Skipped => self.skipped += 1,
        }
    }
}

/// Full acceptance report for a single run.
#[derive(Debug, Serialize)]
pub struct Report {
    pub generated_at: String,
    pub environment: EnvSummary,
    /// Matrix cell labels executed, in order. Empty for a single-cell run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cells: Vec<String>,
    pub totals: Totals,
    pub results: Vec<TestResult>,
}

impl Report {
    /// Assemble a report from per-test results.
    pub fn new(environment: EnvSummary, results: Vec<TestResult>) -> Self {
        let mut totals = Totals::default();
        for result in &results {
            totals.tally(result.status);
        }

        let mut cells: Vec<String> = Vec::new();
        for result in &results {
            if let Some(cell) = &result.cell
                && !cells.contains(cell)
            {
                cells.push(cell.clone());
            }
        }

        Self {
            generated_at: chrono::Utc::now().to_rfc3339(),
            environment,
            cells,
            totals,
            results,
        }
    }

    /// Whether every executed test passed (skips do not fail the run).
    pub fn passed(&self) -> bool {
        self.totals.failed == 0
    }

    /// Per-cell totals, in cell order. Empty for a single-cell run.
    fn per_cell_totals(&self) -> Vec<(String, Totals)> {
        let mut by_cell: BTreeMap<&str, Totals> = BTreeMap::new();
        for result in &self.results {
            if let Some(cell) = result.cell.as_deref() {
                by_cell.entry(cell).or_default().tally(result.status);
            }
        }
        // Preserve declared cell order.
        self.cells
            .iter()
            .filter_map(|cell| by_cell.get(cell.as_str()).map(|t| (cell.clone(), *t)))
            .collect()
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
        let _ = writeln!(
            out,
            "- result: **{} passed, {} failed, {} skipped** of {}\n",
            self.totals.passed, self.totals.failed, self.totals.skipped, self.totals.total
        );

        let per_cell = self.per_cell_totals();
        if !per_cell.is_empty() {
            let _ = writeln!(out, "## Cells\n");
            let _ = writeln!(out, "| Cell | Passed | Failed | Skipped | Total |");
            let _ = writeln!(out, "| --- | --- | --- | --- | --- |");
            for (cell, totals) in &per_cell {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} |",
                    cell, totals.passed, totals.failed, totals.skipped, totals.total
                );
            }
            let _ = writeln!(out);
        }

        let cell_col = !per_cell.is_empty();
        if cell_col {
            let _ = writeln!(
                out,
                "| Status | Cell | Package | Test | Location | Duration | Details |"
            );
            let _ = writeln!(out, "| --- | --- | --- | --- | --- | --- | --- |");
        } else {
            let _ = writeln!(
                out,
                "| Status | Package | Test | Location | Duration | Details |"
            );
            let _ = writeln!(out, "| --- | --- | --- | --- | --- | --- |");
        }
        for result in &self.results {
            let details = result
                .error
                .as_deref()
                .or(result.skip_reason.as_deref())
                .or(result.flaky.as_deref())
                .map(|d| d.replace('|', "\\|").replace('\n', " "))
                .unwrap_or_else(|| metrics_inline(&result.metrics));
            if cell_col {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} ms | {} |",
                    result.status.symbol(),
                    result.cell.as_deref().unwrap_or(""),
                    result.package,
                    result.name,
                    result.location,
                    result.duration_ms,
                    details
                );
            } else {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} ms | {} |",
                    result.status.symbol(),
                    result.package,
                    result.name,
                    result.location,
                    result.duration_ms,
                    details
                );
            }
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
                _ if result.flaky.is_some() => {
                    format!("  [flaky: {}]", result.flaky.as_deref().unwrap_or_default())
                }
                _ if !result.metrics.is_empty() => {
                    format!("  [{}]", metrics_inline(&result.metrics))
                }
                _ => String::new(),
            };
            let cell = result
                .cell
                .as_deref()
                .map(|c| format!("{c} :: "))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "  {:<4} [{:^36}] {}{} {} ms{}",
                result.status.symbol(),
                result.package,
                cell,
                result.name,
                result.duration_ms,
                detail
            );
        }
        let _ = writeln!(
            out,
            "  ----\n  {} passed, {} failed, {} skipped of {}",
            self.totals.passed, self.totals.failed, self.totals.skipped, self.totals.total
        );
        out
    }

    /// Render the report as JUnit XML for CI consumption. One `<testsuite>` per
    /// matrix cell (or a single suite for a non-matrix run).
    pub fn to_junit(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
        let _ = writeln!(
            out,
            r#"<testsuites name="world-chain-acceptance" tests="{}" failures="{}" skipped="{}">"#,
            self.totals.total, self.totals.failed, self.totals.skipped
        );

        // Group results by cell label (or one anonymous suite).
        let mut suites: BTreeMap<&str, Vec<&TestResult>> = BTreeMap::new();
        for result in &self.results {
            suites
                .entry(result.cell.as_deref().unwrap_or(&self.environment.network))
                .or_default()
                .push(result);
        }

        for (suite, results) in suites {
            let mut totals = Totals::default();
            for r in &results {
                totals.tally(r.status);
            }
            let _ = writeln!(
                out,
                r#"  <testsuite name="{}" tests="{}" failures="{}" skipped="{}">"#,
                xml_escape(suite),
                totals.total,
                totals.failed,
                totals.skipped
            );
            for r in results {
                let time = r.duration_ms as f64 / 1000.0;
                let _ = writeln!(
                    out,
                    r#"    <testcase classname="{}" name="{}" time="{:.3}">"#,
                    xml_escape(&r.package),
                    xml_escape(&r.name),
                    time
                );
                match r.status {
                    Status::Failed => {
                        let _ = writeln!(
                            out,
                            r#"      <failure message="{}"></failure>"#,
                            xml_escape(r.error.as_deref().unwrap_or("test failed"))
                        );
                    }
                    Status::Skipped => {
                        let _ = writeln!(
                            out,
                            r#"      <skipped message="{}"></skipped>"#,
                            xml_escape(r.skip_reason.as_deref().unwrap_or("skipped"))
                        );
                    }
                    Status::Passed => {}
                }
                let _ = writeln!(out, "    </testcase>");
            }
            let _ = writeln!(out, "  </testsuite>");
        }

        let _ = writeln!(out, "</testsuites>");
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

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
