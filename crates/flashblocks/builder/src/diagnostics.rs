//! Diagnostic infrastructure for comparing FlashblocksBlockBuilder and BalBlockValidator execution.
//!
//! This module provides a global diagnostic storage system that tracks execution state
//! across payload builds, enabling detailed comparison and debugging when validation errors occur.
//!
//! # Feature Flags
//!
//! Diagnostics are automatically enabled when:
//! - `#[cfg(test)]` - Running tests
//! - `--features diagnostics` - Explicitly enabled via feature flag
//!
//! # Directory Structure
//!
//! Diagnostic reports are written to `diagnostics/{run_id}_report/` in the project root:
//!
//! ```text
//! diagnostics/{run_id}_report/
//! ├── summary.json              # Overview of all recorded states
//! ├── benchmark.json            # Timing data for all operations
//! ├── payload_{id}/
//! │   ├── flashblock_{index}/
//! │   │   ├── builder_state.json     # BundleState from FlashblocksBlockBuilder
//! │   │   ├── builder_access_list.json
//! │   │   ├── validator_state.json   # BundleState from BalBlockValidator
//! │   │   ├── validator_access_list.json
//! │   │   └── diff.json              # Computed differences (if any)
//! │   └── ...
//! └── failures/
//!     └── {error_type}_{timestamp}.json
//! ```

use alloy_primitives::{map::HashMap as AlloyHashMap, Address};
use alloy_rpc_types_engine::PayloadId;
use flashblocks_primitives::access_list::FlashblockAccessList;
use parking_lot::RwLock;
use revm::database::BundleAccount;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::LazyLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Global diagnostic storage for tracking execution states.
pub static DIAGNOSTIC_STORE: LazyLock<RwLock<DiagnosticStore>> =
    LazyLock::new(|| RwLock::new(DiagnosticStore::default()));

/// Unique identifier for a test run.
pub type RunId = String;

/// Index of a flashblock within a payload.
pub type FlashblockIndex = u32;

/// Captured execution state from either builder or validator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionState {
    /// State changes from execution
    pub bundle_state: AlloyHashMap<Address, BundleAccount>,
    /// Computed access list
    pub access_list: FlashblockAccessList,
    /// Timestamp when this state was captured
    pub captured_at: u64,
}

impl ExecutionState {
    /// Create a new execution state with the current timestamp.
    pub fn new(bundle_state: AlloyHashMap<Address, BundleAccount>, access_list: FlashblockAccessList) -> Self {
        let captured_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            bundle_state,
            access_list,
            captured_at,
        }
    }
}

/// Paired execution states from builder and validator for a single flashblock.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlashblockDiagnostic {
    /// State computed by FlashblocksBlockBuilder
    pub builder_state: Option<ExecutionState>,
    /// State computed by BalBlockValidator
    pub validator_state: Option<ExecutionState>,
    /// Expected access list provided in the diff
    pub expected_access_list: Option<FlashblockAccessList>,
}

impl FlashblockDiagnostic {
    /// Check if both builder and validator states are present.
    pub fn is_complete(&self) -> bool {
        self.builder_state.is_some() && self.validator_state.is_some()
    }

    /// Compute differences between builder and validator states.
    pub fn compute_diff(&self) -> Option<StateDiff> {
        let builder = self.builder_state.as_ref()?;
        let validator = self.validator_state.as_ref()?;

        let mut bundle_diff = BundleDiff::default();

        // Find accounts only in builder
        for addr in builder.bundle_state.keys() {
            if !validator.bundle_state.contains_key(addr) {
                bundle_diff.only_in_builder.push(*addr);
            }
        }

        // Find accounts only in validator
        for addr in validator.bundle_state.keys() {
            if !builder.bundle_state.contains_key(addr) {
                bundle_diff.only_in_validator.push(*addr);
            }
        }

        // Find accounts with different states
        for (addr, builder_account) in &builder.bundle_state {
            if let Some(validator_account) = validator.bundle_state.get(addr)
                && builder_account != validator_account {
                    bundle_diff.different.push(AccountDiff {
                        address: *addr,
                        builder: builder_account.clone(),
                        validator: validator_account.clone(),
                    });
                }
        }

        let access_list_matches = builder.access_list == validator.access_list;

        Some(StateDiff {
            bundle_diff,
            access_list_matches,
            builder_access_list: builder.access_list.clone(),
            validator_access_list: validator.access_list.clone(),
        })
    }
}

/// Differences in bundle state between builder and validator.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BundleDiff {
    /// Addresses only present in builder state
    pub only_in_builder: Vec<Address>,
    /// Addresses only present in validator state
    pub only_in_validator: Vec<Address>,
    /// Addresses with different account states
    pub different: Vec<AccountDiff>,
}

/// Detailed diff for a single account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountDiff {
    pub address: Address,
    pub builder: BundleAccount,
    pub validator: BundleAccount,
}

/// Complete diff between builder and validator for a flashblock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateDiff {
    pub bundle_diff: BundleDiff,
    pub access_list_matches: bool,
    pub builder_access_list: FlashblockAccessList,
    pub validator_access_list: FlashblockAccessList,
}

/// Diagnostics for all flashblocks within a payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PayloadDiagnostic {
    /// Flashblock index -> diagnostic data
    pub flashblocks: BTreeMap<FlashblockIndex, FlashblockDiagnostic>,
    /// Timing data for this payload
    pub timing: PayloadTiming,
}

/// Timing data for payload operations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PayloadTiming {
    /// Time spent in builder execution (nanoseconds)
    pub builder_execution_ns: Vec<u64>,
    /// Time spent in validator execution (nanoseconds)
    pub validator_execution_ns: Vec<u64>,
    /// Time spent computing state roots (nanoseconds)
    pub state_root_ns: Vec<u64>,
    /// Total payload build time (nanoseconds)
    pub total_build_ns: Option<u64>,
}

/// Global diagnostic storage.
#[derive(Debug)]
pub struct DiagnosticStore {
    /// Current test run identifier
    pub run_id: Option<RunId>,
    /// PayloadId -> diagnostic data
    pub payloads: HashMap<PayloadId, PayloadDiagnostic>,
    /// Global benchmark data
    pub benchmarks: BenchmarkData,
    /// Whether diagnostic capture is enabled
    pub enabled: bool,
}

impl Default for DiagnosticStore {
    fn default() -> Self {
        // Auto-generate a run_id based on timestamp
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let run_id = format!("run_{}", timestamp);

        Self {
            run_id: Some(run_id),
            payloads: HashMap::default(),
            benchmarks: BenchmarkData::default(),
            // Always enable - the module is only used when diagnostics are needed
            enabled: true,
        }
    }
}

/// Aggregated benchmark data across all payloads.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BenchmarkData {
    /// Total number of flashblocks processed
    pub total_flashblocks: u64,
    /// Total builder execution time (nanoseconds)
    pub total_builder_ns: u64,
    /// Total validator execution time (nanoseconds)
    pub total_validator_ns: u64,
    /// Total state root computation time (nanoseconds)
    pub total_state_root_ns: u64,
    /// Number of validation failures
    pub validation_failures: u64,
    /// Number of successful validations
    pub validation_successes: u64,
}

impl DiagnosticStore {
    /// Start a new diagnostic run with a unique identifier.
    pub fn start_run(&mut self, run_id: impl Into<String>) {
        self.run_id = Some(run_id.into());
        self.payloads.clear();
        self.benchmarks = BenchmarkData::default();
        self.enabled = true;
    }

    /// Enable or disable diagnostic capture.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Record builder execution state for a flashblock.
    pub fn record_builder_state(
        &mut self,
        payload_id: PayloadId,
        flashblock_index: FlashblockIndex,
        bundle_state: AlloyHashMap<Address, BundleAccount>,
        access_list: FlashblockAccessList,
    ) {
        if !self.enabled {
            return;
        }

        let payload = self.payloads.entry(payload_id).or_default();
        let flashblock = payload.flashblocks.entry(flashblock_index).or_default();
        flashblock.builder_state = Some(ExecutionState::new(bundle_state, access_list));
    }

    /// Record validator execution state for a flashblock.
    pub fn record_validator_state(
        &mut self,
        payload_id: PayloadId,
        flashblock_index: FlashblockIndex,
        bundle_state: AlloyHashMap<Address, BundleAccount>,
        access_list: FlashblockAccessList,
    ) {
        if !self.enabled {
            return;
        }

        let payload = self.payloads.entry(payload_id).or_default();
        let flashblock = payload.flashblocks.entry(flashblock_index).or_default();
        flashblock.validator_state = Some(ExecutionState::new(bundle_state, access_list));
    }

    /// Record the expected access list from the diff.
    pub fn record_expected_access_list(
        &mut self,
        payload_id: PayloadId,
        flashblock_index: FlashblockIndex,
        access_list: FlashblockAccessList,
    ) {
        if !self.enabled {
            return;
        }

        let payload = self.payloads.entry(payload_id).or_default();
        let flashblock = payload.flashblocks.entry(flashblock_index).or_default();
        flashblock.expected_access_list = Some(access_list);
    }

    /// Record builder execution timing.
    pub fn record_builder_timing(&mut self, payload_id: PayloadId, duration: Duration) {
        if !self.enabled {
            return;
        }

        let ns = duration.as_nanos() as u64;
        self.benchmarks.total_builder_ns += ns;

        let payload = self.payloads.entry(payload_id).or_default();
        payload.timing.builder_execution_ns.push(ns);
    }

    /// Record validator execution timing.
    pub fn record_validator_timing(&mut self, payload_id: PayloadId, duration: Duration) {
        if !self.enabled {
            return;
        }

        let ns = duration.as_nanos() as u64;
        self.benchmarks.total_validator_ns += ns;

        let payload = self.payloads.entry(payload_id).or_default();
        payload.timing.validator_execution_ns.push(ns);
    }

    /// Record state root computation timing.
    pub fn record_state_root_timing(&mut self, payload_id: PayloadId, duration: Duration) {
        if !self.enabled {
            return;
        }

        let ns = duration.as_nanos() as u64;
        self.benchmarks.total_state_root_ns += ns;

        let payload = self.payloads.entry(payload_id).or_default();
        payload.timing.state_root_ns.push(ns);
    }

    /// Record a validation success.
    pub fn record_validation_success(&mut self) {
        self.benchmarks.validation_successes += 1;
        self.benchmarks.total_flashblocks += 1;
    }

    /// Record a validation failure.
    pub fn record_validation_failure(&mut self) {
        self.benchmarks.validation_failures += 1;
        self.benchmarks.total_flashblocks += 1;
    }

    /// Get the output directory for this run.
    ///
    /// When running in test mode or with the `diagnostics` feature enabled,
    /// outputs to `diagnostics/{run_id}_report/` in the project root.
    pub fn output_dir(&self) -> PathBuf {
        let run_id = self.run_id.as_deref().unwrap_or("unknown");

        // Use CARGO_MANIFEST_DIR to find project root, fallback to current directory
        let base_dir = std::env::var("CARGO_MANIFEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));

        // Navigate up from crate directory to workspace root
        // CARGO_MANIFEST_DIR points to crates/flashblocks/builder
        let workspace_root = base_dir
            .ancestors()
            .find(|p| p.join("Cargo.toml").exists() && p.join("crates").exists())
            .map(PathBuf::from)
            .unwrap_or(base_dir);

        workspace_root.join("diagnostics").join(format!("{}_report", run_id))
    }
}

/// RAII guard for timing operations.
pub struct TimingGuard {
    start: Instant,
    payload_id: PayloadId,
    operation: TimingOperation,
}

/// Types of timed operations.
#[derive(Debug, Clone, Copy)]
pub enum TimingOperation {
    BuilderExecution,
    ValidatorExecution,
    StateRootComputation,
}

impl TimingGuard {
    /// Create a new timing guard.
    pub fn new(payload_id: PayloadId, operation: TimingOperation) -> Self {
        Self {
            start: Instant::now(),
            payload_id,
            operation,
        }
    }
}

impl Drop for TimingGuard {
    fn drop(&mut self) {
        let duration = self.start.elapsed();
        let mut store = DIAGNOSTIC_STORE.write();

        match self.operation {
            TimingOperation::BuilderExecution => {
                store.record_builder_timing(self.payload_id, duration);
            }
            TimingOperation::ValidatorExecution => {
                store.record_validator_timing(self.payload_id, duration);
            }
            TimingOperation::StateRootComputation => {
                store.record_state_root_timing(self.payload_id, duration);
            }
        }
    }
}

/// Start timing a builder execution.
pub fn time_builder(payload_id: PayloadId) -> TimingGuard {
    TimingGuard::new(payload_id, TimingOperation::BuilderExecution)
}

/// Start timing a validator execution.
pub fn time_validator(payload_id: PayloadId) -> TimingGuard {
    TimingGuard::new(payload_id, TimingOperation::ValidatorExecution)
}

/// Start timing a state root computation.
pub fn time_state_root(payload_id: PayloadId) -> TimingGuard {
    TimingGuard::new(payload_id, TimingOperation::StateRootComputation)
}

/// Diagnostic report generated when a validation error occurs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticReport {
    /// Unique identifier for this report
    pub report_id: String,
    /// Test run identifier
    pub run_id: String,
    /// Payload that failed validation
    pub payload_id: PayloadId,
    /// Flashblock index that failed
    pub flashblock_index: FlashblockIndex,
    /// Error type that triggered the report
    pub error_type: String,
    /// Error message
    pub error_message: String,
    /// Computed state diff (if available)
    pub state_diff: Option<StateDiff>,
    /// Builder state snapshot
    pub builder_state: Option<ExecutionState>,
    /// Validator state snapshot
    pub validator_state: Option<ExecutionState>,
    /// Expected access list from diff
    pub expected_access_list: Option<FlashblockAccessList>,
    /// Timing data
    pub timing: PayloadTiming,
    /// Timestamp when report was generated
    pub generated_at: u64,
}

impl DiagnosticReport {
    /// Create a new diagnostic report from the current store state.
    pub fn from_validation_error(
        payload_id: PayloadId,
        flashblock_index: FlashblockIndex,
        error_type: impl Into<String>,
        error_message: impl Into<String>,
    ) -> Self {
        let store = DIAGNOSTIC_STORE.read();
        let run_id = store.run_id.clone().unwrap_or_else(|| "unknown".to_string());

        let (state_diff, builder_state, validator_state, expected_access_list, timing) =
            if let Some(payload) = store.payloads.get(&payload_id) {
                let flashblock = payload.flashblocks.get(&flashblock_index);
                let state_diff = flashblock.and_then(|f| f.compute_diff());
                let builder_state = flashblock.and_then(|f| f.builder_state.clone());
                let validator_state = flashblock.and_then(|f| f.validator_state.clone());
                let expected = flashblock.and_then(|f| f.expected_access_list.clone());
                (state_diff, builder_state, validator_state, expected, payload.timing.clone())
            } else {
                (None, None, None, None, PayloadTiming::default())
            };

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let error_type_str = error_type.into();
        let report_id = format!("{}_{}", error_type_str, timestamp);

        Self {
            report_id: report_id.clone(),
            run_id,
            payload_id,
            flashblock_index,
            error_type: error_type_str,
            error_message: error_message.into(),
            state_diff,
            builder_state,
            validator_state,
            expected_access_list,
            timing,
            generated_at: timestamp,
        }
    }

    /// Create a diagnostic report with explicit validator state data.
    /// This method also looks up the builder state from the diagnostic store
    /// if it was previously recorded via `record_builder_state`.
    pub fn with_states(
        payload_id: PayloadId,
        flashblock_index: FlashblockIndex,
        error_type: impl Into<String>,
        error_message: impl Into<String>,
        validator_bundle: AlloyHashMap<Address, BundleAccount>,
        validator_access_list: FlashblockAccessList,
        expected_access_list: Option<FlashblockAccessList>,
    ) -> Self {
        let store = DIAGNOSTIC_STORE.read();
        let run_id = store.run_id.clone().unwrap_or_else(|| "unknown".to_string());

        // Look up builder state from the store (recorded when payload was published)
        let (builder_state, timing) = if let Some(payload) = store.payloads.get(&payload_id) {
            let flashblock = payload.flashblocks.get(&flashblock_index);
            let builder_state = flashblock.and_then(|f| f.builder_state.clone());
            (builder_state, payload.timing.clone())
        } else {
            (None, PayloadTiming::default())
        };

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let error_type_str = error_type.into();
        let report_id = format!("{}_{}", error_type_str, timestamp);

        let validator_state = Some(ExecutionState::new(
            validator_bundle.clone(),
            validator_access_list.clone(),
        ));

        // Compute state diff if we have both states
        let state_diff = if let Some(ref builder) = builder_state {
            let mut bundle_diff = BundleDiff::default();

            // Find accounts only in builder
            for addr in builder.bundle_state.keys() {
                if !validator_bundle.contains_key(addr) {
                    bundle_diff.only_in_builder.push(*addr);
                }
            }

            // Find accounts only in validator
            for addr in validator_bundle.keys() {
                if !builder.bundle_state.contains_key(addr) {
                    bundle_diff.only_in_validator.push(*addr);
                }
            }

            // Find accounts with different states
            for (addr, builder_account) in &builder.bundle_state {
                if let Some(validator_account) = validator_bundle.get(addr)
                    && builder_account != validator_account {
                        bundle_diff.different.push(AccountDiff {
                            address: *addr,
                            builder: builder_account.clone(),
                            validator: validator_account.clone(),
                        });
                    }
            }

            let access_list_matches = builder.access_list == validator_access_list;

            Some(StateDiff {
                bundle_diff,
                access_list_matches,
                builder_access_list: builder.access_list.clone(),
                validator_access_list,
            })
        } else {
            None
        };

        Self {
            report_id: report_id.clone(),
            run_id,
            payload_id,
            flashblock_index,
            error_type: error_type_str,
            error_message: error_message.into(),
            state_diff,
            builder_state,
            validator_state,
            expected_access_list,
            timing,
            generated_at: timestamp,
        }
    }

    /// Write the report to disk.
    pub fn write_to_disk(&self) -> std::io::Result<PathBuf> {
        let store = DIAGNOSTIC_STORE.read();
        let base_dir = store.output_dir();
        let failures_dir = base_dir.join("failures");

        std::fs::create_dir_all(&failures_dir)?;

        let filename = format!("{}.json", self.report_id);
        let path = failures_dir.join(&filename);

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        std::fs::write(&path, json)?;

        // Also write a human-readable summary
        let summary_path = failures_dir.join(format!("{}_summary.txt", self.report_id));
        let summary = self.generate_summary();
        std::fs::write(&summary_path, summary)?;

        Ok(path)
    }

    /// Generate a human-readable summary of the report.
    pub fn generate_summary(&self) -> String {
        let mut summary = String::new();

        summary.push_str(&format!("=== Diagnostic Report: {} ===\n\n", self.report_id));
        summary.push_str(&format!("Run ID: {}\n", self.run_id));
        summary.push_str(&format!("Payload ID: {:?}\n", self.payload_id));
        summary.push_str(&format!("Flashblock Index: {}\n", self.flashblock_index));
        summary.push_str(&format!("Error Type: {}\n", self.error_type));
        summary.push_str(&format!("Error Message: {}\n\n", self.error_message));

        if let Some(diff) = &self.state_diff {
            summary.push_str("=== State Differences ===\n\n");

            if !diff.bundle_diff.only_in_builder.is_empty() {
                summary.push_str("Accounts only in builder:\n");
                for addr in &diff.bundle_diff.only_in_builder {
                    summary.push_str(&format!("  - {}\n", addr));
                }
                summary.push('\n');
            }

            if !diff.bundle_diff.only_in_validator.is_empty() {
                summary.push_str("Accounts only in validator:\n");
                for addr in &diff.bundle_diff.only_in_validator {
                    summary.push_str(&format!("  - {}\n", addr));
                }
                summary.push('\n');
            }

            if !diff.bundle_diff.different.is_empty() {
                summary.push_str(&format!("Accounts with differences: {}\n", diff.bundle_diff.different.len()));
                for account_diff in &diff.bundle_diff.different {
                    summary.push_str(&format!("  - {} (see JSON for details)\n", account_diff.address));
                }
                summary.push('\n');
            }

            summary.push_str(&format!("Access lists match: {}\n\n", diff.access_list_matches));
        }

        summary.push_str("=== Timing Data ===\n\n");
        if !self.timing.builder_execution_ns.is_empty() {
            let avg_builder = self.timing.builder_execution_ns.iter().sum::<u64>()
                / self.timing.builder_execution_ns.len() as u64;
            summary.push_str(&format!("Avg builder execution: {:.3}ms\n", avg_builder as f64 / 1_000_000.0));
        }
        if !self.timing.validator_execution_ns.is_empty() {
            let avg_validator = self.timing.validator_execution_ns.iter().sum::<u64>()
                / self.timing.validator_execution_ns.len() as u64;
            summary.push_str(&format!("Avg validator execution: {:.3}ms\n", avg_validator as f64 / 1_000_000.0));
        }
        if !self.timing.state_root_ns.is_empty() {
            let avg_state_root = self.timing.state_root_ns.iter().sum::<u64>()
                / self.timing.state_root_ns.len() as u64;
            summary.push_str(&format!("Avg state root computation: {:.3}ms\n", avg_state_root as f64 / 1_000_000.0));
        }

        summary
    }
}

/// Write the complete diagnostic summary for a test run.
pub fn write_run_summary() -> std::io::Result<PathBuf> {
    let store = DIAGNOSTIC_STORE.read();
    let base_dir = store.output_dir();

    std::fs::create_dir_all(&base_dir)?;

    // Write benchmark data
    let benchmark_path = base_dir.join("benchmark.json");
    let benchmark_json = serde_json::to_string_pretty(&store.benchmarks)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&benchmark_path, benchmark_json)?;

    // Write summary
    let summary_path = base_dir.join("summary.json");

    #[derive(Serialize)]
    struct Summary<'a> {
        run_id: &'a str,
        total_payloads: usize,
        benchmarks: &'a BenchmarkData,
    }

    let summary = Summary {
        run_id: store.run_id.as_deref().unwrap_or("unknown"),
        total_payloads: store.payloads.len(),
        benchmarks: &store.benchmarks,
    };

    let summary_json = serde_json::to_string_pretty(&summary)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&summary_path, summary_json)?;

    // Write human-readable summary
    let text_summary_path = base_dir.join("summary.txt");
    let text_summary = generate_text_summary(&store);
    std::fs::write(&text_summary_path, text_summary)?;

    Ok(summary_path)
}

fn generate_text_summary(store: &DiagnosticStore) -> String {
    let mut summary = String::new();

    summary.push_str(&format!("=== Test Run Summary: {} ===\n\n",
        store.run_id.as_deref().unwrap_or("unknown")));

    summary.push_str("=== Benchmark Results ===\n\n");
    summary.push_str(&format!("Total flashblocks processed: {}\n", store.benchmarks.total_flashblocks));
    summary.push_str(&format!("Validation successes: {}\n", store.benchmarks.validation_successes));
    summary.push_str(&format!("Validation failures: {}\n", store.benchmarks.validation_failures));

    if store.benchmarks.total_flashblocks > 0 {
        let success_rate = (store.benchmarks.validation_successes as f64
            / store.benchmarks.total_flashblocks as f64) * 100.0;
        summary.push_str(&format!("Success rate: {:.2}%\n\n", success_rate));
    }

    summary.push_str(&format!("Total builder execution time: {:.3}ms\n",
        store.benchmarks.total_builder_ns as f64 / 1_000_000.0));
    summary.push_str(&format!("Total validator execution time: {:.3}ms\n",
        store.benchmarks.total_validator_ns as f64 / 1_000_000.0));
    summary.push_str(&format!("Total state root computation time: {:.3}ms\n",
        store.benchmarks.total_state_root_ns as f64 / 1_000_000.0));

    if store.benchmarks.total_flashblocks > 0 {
        let avg_builder = store.benchmarks.total_builder_ns as f64
            / store.benchmarks.total_flashblocks as f64 / 1_000_000.0;
        let avg_validator = store.benchmarks.total_validator_ns as f64
            / store.benchmarks.total_flashblocks as f64 / 1_000_000.0;
        let avg_state_root = store.benchmarks.total_state_root_ns as f64
            / store.benchmarks.total_flashblocks as f64 / 1_000_000.0;

        summary.push_str(&"\nPer-flashblock averages:\n".to_string());
        summary.push_str(&format!("  Builder: {:.3}ms\n", avg_builder));
        summary.push_str(&format!("  Validator: {:.3}ms\n", avg_validator));
        summary.push_str(&format!("  State root: {:.3}ms\n", avg_state_root));
    }

    summary
}

/// Initialize diagnostics for a test run.
pub fn init_diagnostics(run_id: impl Into<String>) {
    let mut store = DIAGNOSTIC_STORE.write();
    store.start_run(run_id);
}

/// Enable or disable diagnostic capture.
pub fn set_diagnostics_enabled(enabled: bool) {
    let mut store = DIAGNOSTIC_STORE.write();
    store.set_enabled(enabled);
}

/// Record builder state for a flashblock.
pub fn record_builder_state(
    payload_id: PayloadId,
    flashblock_index: FlashblockIndex,
    bundle_state: AlloyHashMap<Address, BundleAccount>,
    access_list: FlashblockAccessList,
) {
    let mut store = DIAGNOSTIC_STORE.write();
    store.record_builder_state(payload_id, flashblock_index, bundle_state, access_list);
}

/// Record validator state for a flashblock.
pub fn record_validator_state(
    payload_id: PayloadId,
    flashblock_index: FlashblockIndex,
    bundle_state: AlloyHashMap<Address, BundleAccount>,
    access_list: FlashblockAccessList,
) {
    let mut store = DIAGNOSTIC_STORE.write();
    store.record_validator_state(payload_id, flashblock_index, bundle_state, access_list);
}

/// Record expected access list from the diff.
pub fn record_expected_access_list(
    payload_id: PayloadId,
    flashblock_index: FlashblockIndex,
    access_list: FlashblockAccessList,
) {
    let mut store = DIAGNOSTIC_STORE.write();
    store.record_expected_access_list(payload_id, flashblock_index, access_list);
}

/// Record a validation success.
pub fn record_validation_success() {
    let mut store = DIAGNOSTIC_STORE.write();
    store.record_validation_success();
}

/// Record a validation failure and generate a report.
pub fn record_validation_failure(
    payload_id: PayloadId,
    flashblock_index: FlashblockIndex,
    error_type: impl Into<String>,
    error_message: impl Into<String>,
) -> DiagnosticReport {
    {
        let mut store = DIAGNOSTIC_STORE.write();
        store.record_validation_failure();
    }

    let report = DiagnosticReport::from_validation_error(
        payload_id,
        flashblock_index,
        error_type,
        error_message,
    );

    if let Err(e) = report.write_to_disk() {
        tracing::error!("Failed to write diagnostic report: {}", e);
    }

    report
}

/// Per-flashblock benchmark report written after each successful flashblock publish.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashblockBenchmark {
    /// Payload ID
    pub payload_id: PayloadId,
    /// Flashblock index within the payload
    pub flashblock_index: FlashblockIndex,
    /// Builder state snapshot
    pub builder_state: Option<ExecutionState>,
    /// Timestamp when this benchmark was recorded
    pub recorded_at: u64,
}

impl FlashblockBenchmark {
    /// Create a new flashblock benchmark with the current state.
    pub fn new(
        payload_id: PayloadId,
        flashblock_index: FlashblockIndex,
        bundle_state: AlloyHashMap<Address, BundleAccount>,
        access_list: FlashblockAccessList,
    ) -> Self {
        let recorded_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            payload_id,
            flashblock_index,
            builder_state: Some(ExecutionState::new(bundle_state, access_list)),
            recorded_at,
        }
    }

    /// Write this benchmark to disk.
    pub fn write_to_disk(&self) -> std::io::Result<PathBuf> {
        let store = DIAGNOSTIC_STORE.read();
        let base_dir = store.output_dir();
        let benchmarks_dir = base_dir.join("benchmarks");

        std::fs::create_dir_all(&benchmarks_dir)?;

        let filename = format!(
            "flashblock_{:?}_{}.json",
            self.payload_id, self.flashblock_index
        );
        let path = benchmarks_dir.join(&filename);

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        std::fs::write(&path, json)?;

        Ok(path)
    }
}

/// Record builder state and write a per-flashblock benchmark report.
/// This should be called every time a flashblock is successfully published.
pub fn record_and_write_flashblock_benchmark(
    payload_id: PayloadId,
    flashblock_index: FlashblockIndex,
    bundle_state: AlloyHashMap<Address, BundleAccount>,
    access_list: FlashblockAccessList,
) {
    // Record to the store for potential later correlation
    {
        let mut store = DIAGNOSTIC_STORE.write();
        store.record_builder_state(
            payload_id,
            flashblock_index,
            bundle_state.clone(),
            access_list.clone(),
        );
    }

    // Write per-flashblock benchmark
    let benchmark = FlashblockBenchmark::new(
        payload_id,
        flashblock_index,
        bundle_state,
        access_list,
    );

    if let Err(e) = benchmark.write_to_disk() {
        tracing::error!("Failed to write flashblock benchmark: {}", e);
    }
}
