pub mod access_list;
pub mod assembler;
pub mod block_builder;
pub mod coordinator;
pub mod executor;
pub mod payload_builder;
pub mod payload_txns;
pub mod traits;

/// Test harness for validating flashblock payload execution.
///
/// This module provides infrastructure for comparing executed vs computed block contexts
/// during flashblock building. It captures state differences and writes detailed reports
/// to a structured directory.
///
/// # Directory Structure
///
/// Test results are written to `{CARGO_MANIFEST_DIR}/test_results/`:
///
/// ```text
/// test_results/
/// ├── report.txt              # Summary report updated after each validation
/// ├── contexts/               # All block context JSON files
/// │   └── block-{number}-{index}.json
/// └── failures/               # Detailed diff reports for mismatches
///     └── block-{number}-{index}.diff.txt
/// ```
///
/// # Usage
///
/// ```ignore
/// // Record executed state after block execution
/// test::record_executed(block_number, flashblock_index, Some(context));
///
/// // Record computed/predicted state
/// test::record_computed(block_number, flashblock_index, Some(context));
///
/// // When both are recorded, validation runs automatically
/// ```
#[cfg(any(feature = "test", test))]
pub mod test {
    use std::{
        collections::{BTreeMap, HashMap},
        fmt::Write as FmtWrite,
        path::{Path, PathBuf},
        sync::LazyLock,
    };

    use alloy_primitives::Address;
    use flashblocks_primitives::access_list::FlashblockAccessList;
    use parking_lot::RwLock;
    use reth::revm::db::BundleAccount;
    use reth_evm::env;
    use serde::{Deserialize, Serialize};
    use tracing::{error, info};

    /// Base directory for test results output.
    const TEST_RESULTS_DIR: &str = ".report";

    /// Container for test contexts and statistics.
    #[derive(Debug, Default)]
    pub struct TestHarness {
        /// Block contexts indexed by (block_number, flashblock_index)
        contexts: HashMap<u64, HashMap<u64, BlockContexts>>,
        /// Running statistics
        stats: TestStats,
    }

    /// Statistics tracked across test runs.
    #[derive(Debug, Default)]
    struct TestStats {
        total_recorded: u64,
        validations_complete: u64,
        validations_passed: u64,
        validations_failed: u64,
    }

    /// Pair of executed and computed contexts for comparison.
    #[derive(Serialize, Deserialize, Debug, Clone, Default)]
    pub struct BlockContexts {
        /// Context from actual block execution
        pub executed: Option<BlockContext>,
        /// Context from computed/predicted state
        pub computed: Option<BlockContext>,
    }

    impl BlockContexts {
        /// Returns true if both executed and computed contexts are present.
        pub fn is_complete(&self) -> bool {
            self.executed.is_some() && self.computed.is_some()
        }

        /// Returns true if executed and computed contexts match.
        pub fn matches(&self) -> bool {
            self.executed == self.computed
        }

        /// Generate a human-readable diff between executed and computed contexts.
        fn generate_diff(&self) -> Option<String> {
            let (executed, computed) = match (&self.executed, &self.computed) {
                (Some(e), Some(c)) => (e, c),
                _ => return None,
            };

            let mut diff = String::new();

            // Bundle diff
            writeln!(&mut diff, "BUNDLE STATE DIFFERENCES").ok();
            writeln!(&mut diff, "------------------------").ok();

            let exec_addrs: std::collections::HashSet<_> = executed.bundle.keys().collect();
            let comp_addrs: std::collections::HashSet<_> = computed.bundle.keys().collect();

            // Addresses only in executed
            let exec_only: Vec<_> = exec_addrs.difference(&comp_addrs).collect();
            if !exec_only.is_empty() {
                writeln!(&mut diff, "\nAddresses only in EXECUTED:").ok();
                for addr in exec_only {
                    writeln!(&mut diff, "  - {addr}").ok();
                }
            }

            // Addresses only in computed
            let comp_only: Vec<_> = comp_addrs.difference(&exec_addrs).collect();
            if !comp_only.is_empty() {
                writeln!(&mut diff, "\nAddresses only in COMPUTED:").ok();
                for addr in comp_only {
                    writeln!(&mut diff, "  + {addr}").ok();
                }
            }

            // Addresses with different values
            let common: Vec<_> = exec_addrs.intersection(&comp_addrs).collect();
            let mut mismatches = Vec::new();
            for addr in common {
                let exec_acc = &executed.bundle[*addr];
                let comp_acc = &computed.bundle[*addr];
                if exec_acc.info != comp_acc.info {
                    mismatches.push((**addr, exec_acc, comp_acc));
                }
            }

            if !mismatches.is_empty() {
                writeln!(&mut diff, "\nAddresses with MISMATCHED state:").ok();
                for (addr, exec, comp) in &mismatches {
                    writeln!(&mut diff, "\n  Address: {addr}").ok();

                    if exec.info != comp.info {
                        writeln!(&mut diff, "    Account info differs:").ok();
                        writeln!(&mut diff, "      executed: {:?}", exec.info).ok();
                        writeln!(&mut diff, "      computed: {:?}", comp.info).ok();
                    }

                    if exec.status != comp.status {
                        writeln!(&mut diff, "    Status differs:").ok();
                        writeln!(&mut diff, "      executed: {:?}", exec.status).ok();
                        writeln!(&mut diff, "      computed: {:?}", comp.status).ok();
                    }

                    // Storage diff
                    let exec_storage: BTreeMap<_, _> = exec.storage.iter().collect();
                    let comp_storage: BTreeMap<_, _> = comp.storage.iter().collect();
                    if exec_storage != comp_storage {
                        writeln!(&mut diff, "    Storage differs:").ok();
                        for (slot, exec_val) in &exec_storage {
                            match comp_storage.get(slot) {
                                Some(comp_val) if exec_val != comp_val => {
                                    writeln!(&mut diff, "      slot {slot}:").ok();
                                    writeln!(&mut diff, "        executed: {exec_val:?}").ok();
                                    writeln!(&mut diff, "        computed: {comp_val:?}").ok();
                                }
                                None => {
                                    writeln!(&mut diff, "      slot {slot}: only in executed").ok();
                                }
                                _ => {}
                            }
                        }
                        for slot in comp_storage.keys() {
                            if !exec_storage.contains_key(slot) {
                                writeln!(&mut diff, "      slot {slot}: only in computed").ok();
                            }
                        }
                    }
                }
            }

            // Access list diff
            if executed.access_list != computed.access_list {
                writeln!(&mut diff, "\nACCESS LIST DIFFERENCES").ok();
                writeln!(&mut diff, "-----------------------").ok();
                writeln!(&mut diff, "Executed:").ok();
                if let Ok(json) = serde_json::to_string_pretty(&executed.access_list) {
                    for line in json.lines() {
                        writeln!(&mut diff, "  {line}").ok();
                    }
                }
                writeln!(&mut diff, "Computed:").ok();
                if let Ok(json) = serde_json::to_string_pretty(&computed.access_list) {
                    for line in json.lines() {
                        writeln!(&mut diff, "  {line}").ok();
                    }
                }
            }

            Some(diff)
        }
    }

    /// Individual block context containing state bundle and access list.
    #[derive(Serialize, Deserialize, Debug, Clone)]
    pub struct BlockContext {
        /// State changes from block execution
        pub bundle: alloy_primitives::map::HashMap<Address, BundleAccount>,
        /// Access list for the flashblock
        pub access_list: FlashblockAccessList,
    }

    impl PartialEq for BlockContext {
        fn eq(&self, other: &Self) -> bool {
            for (addr, acc) in &self.bundle {
                match other.bundle.get(addr) {
                    Some(other_acc) if acc.info != other_acc.info => {
                        return false;
                    }
                    None => return false,
                    _ => {}
                }
            }

            self.access_list == other.access_list
        }
    }

    /// Global test harness instance.
    pub static TEST_HARNESS: LazyLock<RwLock<TestHarness>> =
        LazyLock::new(|| RwLock::new(TestHarness::default()));

    /// Record the context from actual block execution.
    pub fn record_executed(number: u64, index: u64, executed: Option<BlockContext>) {
        let mut harness = TEST_HARNESS.write();
        harness.stats.total_recorded += 1;

        let block_map = harness.contexts.entry(number).or_default();
        block_map
            .entry(index)
            .and_modify(|ctx| ctx.executed = executed.clone())
            .or_insert(BlockContexts {
                executed,
                computed: None,
            });
    }

    /// Record the context from computed/predicted state.
    pub fn record_computed(number: u64, index: u64, computed: Option<BlockContext>) {
        let mut harness = TEST_HARNESS.write();
        harness.stats.total_recorded += 1;

        let block_map = harness.contexts.entry(number).or_default();
        let entry = block_map
            .entry(index)
            .and_modify(|ctx| ctx.computed = computed.clone())
            .or_insert(BlockContexts {
                executed: None,
                computed,
            });

        write_context_and_report(number, index, entry);
    }

    /// Write context to disk and update report. Validates if both contexts are present.
    fn write_context_and_report(number: u64, index: u64, ctx: &BlockContexts) {
        let cargo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        let results_dir = cargo_root
            .ancestors()
            .nth(3)
            .unwrap()
            .join(TEST_RESULTS_DIR);

        info!(
            target: "flashblocks::test",
            block = number,
            index = index,
            dir = %results_dir.display(),
            "Recording context"
        );

        let failures_path = format!("{}/failures", results_dir.to_str().unwrap());

        if let Err(e) = std::fs::create_dir_all(&failures_path) {
            error!(target: "flashblocks::test", "Failed to create failures directory: {e}");
            return;
        }

        // Update stats
        {
            let mut harness = TEST_HARNESS.write();
            harness.stats.validations_complete += 1;
            if ctx.matches() {
                harness.stats.validations_passed += 1;
            } else {
                harness.stats.validations_failed += 1;
            }
        }

        // Write failure diff if validation failed
        if !ctx.matches() {
            if let Some(diff) = ctx.generate_diff() {
                let diff_path = format!("{}/block_{}/diff_{}.json", failures_path, number, index);
                let raw = format!("{}/block_{}/raw_{}.json", failures_path, number, index);

                std::fs::create_dir_all(Path::new(&diff_path).parent().unwrap()).ok();

                let mut report = String::new();
                writeln!(
                    &mut report,
                    "═══════════════════════════════════════════════════════════"
                )
                .ok();
                writeln!(&mut report, "  FLASHBLOCK VALIDATION FAILURE").ok();
                writeln!(
                    &mut report,
                    "═══════════════════════════════════════════════════════════"
                )
                .ok();
                writeln!(&mut report).ok();
                writeln!(&mut report, "Block Number:      {}", number).ok();
                writeln!(&mut report, "Flashblock Index:  {}", index).ok();
                writeln!(&mut report).ok();
                writeln!(
                    &mut report,
                    "───────────────────────────────────────────────────────────"
                )
                .ok();
                writeln!(&mut report).ok();
                writeln!(&mut report, "{}", diff).ok();

                if let Err(e) = std::fs::write(&diff_path, &report) {
                    error!(target: "flashblocks::test", "Failed to write diff: {e}");
                }

                // Also write raw contexts for reference
                if let Ok(raw_json) = serde_json::to_string_pretty(&ctx) {
                    if let Err(e) = std::fs::write(&raw, &raw_json) {
                        error!(target: "flashblocks::test", "Failed to write raw context: {e}");
                    }
                }
            }

            error!(
                target: "flashblocks::test",
                block = number,
                index = index,
                "VALIDATION FAILED - see {}/block_{}/diff_{}.json",
                failures_path, number, index
            );
        }

        // Always write summary report
        write_report(&results_dir.to_str().unwrap());
    }

    /// Write the summary report file.
    fn write_report(base_path: &str) {
        let harness = TEST_HARNESS.read();
        let report_path = format!("{}/report.txt", base_path);

        let mut report = String::new();

        writeln!(
            &mut report,
            "╔═══════════════════════════════════════════════════════════╗"
        )
        .ok();
        writeln!(
            &mut report,
            "║           FLASHBLOCK TEST HARNESS REPORT                  ║"
        )
        .ok();
        writeln!(
            &mut report,
            "╚═══════════════════════════════════════════════════════════╝"
        )
        .ok();
        writeln!(&mut report).ok();

        // Statistics
        writeln!(&mut report, "STATISTICS").ok();
        writeln!(&mut report, "──────────").ok();
        writeln!(
            &mut report,
            "  Total contexts recorded:  {}",
            harness.stats.total_recorded
        )
        .ok();
        writeln!(
            &mut report,
            "  Validations complete:     {}",
            harness.stats.validations_complete
        )
        .ok();
        writeln!(
            &mut report,
            "  Passed:                   {}",
            harness.stats.validations_passed
        )
        .ok();
        writeln!(
            &mut report,
            "  Failed:                   {}",
            harness.stats.validations_failed
        )
        .ok();

        if harness.stats.validations_complete > 0 {
            let pass_rate = (harness.stats.validations_passed as f64
                / harness.stats.validations_complete as f64)
                * 100.0;
            writeln!(&mut report, "  Pass rate:                {:.1}%", pass_rate).ok();
        }

        writeln!(&mut report).ok();

        // List all validated blocks
        writeln!(&mut report, "VALIDATED BLOCKS").ok();
        writeln!(&mut report, "────────────────").ok();

        let mut blocks: Vec<_> = harness.contexts.keys().collect();
        blocks.sort();

        for block_num in blocks {
            if let Some(indices) = harness.contexts.get(block_num) {
                let mut indices_sorted: Vec<_> = indices.keys().collect();
                indices_sorted.sort();

                for idx in indices_sorted {
                    if let Some(ctx) = indices.get(idx) {
                        if ctx.is_complete() {
                            let status = if ctx.matches() {
                                "✓ PASS"
                            } else {
                                "✗ FAIL"
                            };
                            writeln!(
                                &mut report,
                                "  Block {} Index {}: {}",
                                block_num, idx, status
                            )
                            .ok();
                        }
                    }
                }
            }
        }

        writeln!(&mut report).ok();
        writeln!(
            &mut report,
            "───────────────────────────────────────────────────────────"
        )
        .ok();
        writeln!(&mut report, "Results directory: {}", base_path).ok();

        if let Err(e) = std::fs::write(&report_path, &report) {
            error!(target: "flashblocks::test", "Failed to write report: {e}");
        }
    }

    /// Clear all test state. Useful for test setup.
    pub fn clear() {
        let mut harness = TEST_HARNESS.write();
        harness.contexts.clear();
        harness.stats = TestStats::default();
    }

    /// Get current statistics as (total_recorded, complete, passed, failed).
    pub fn get_stats() -> (u64, u64, u64, u64) {
        let harness = TEST_HARNESS.read();
        (
            harness.stats.total_recorded,
            harness.stats.validations_complete,
            harness.stats.validations_passed,
            harness.stats.validations_failed,
        )
    }
}
