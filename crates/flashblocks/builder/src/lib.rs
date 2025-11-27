pub mod access_list;
pub mod assembler;
pub mod block_builder;
pub mod coordinator;
pub mod diagnostics;
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
    use std::path::PathBuf;

    use alloy_primitives::Address;
    use flashblocks_primitives::access_list::FlashblockAccessList;
    use reth::revm::db::BundleAccount;
    use serde::{Deserialize, Serialize};

    /// Base directory for test results output.
    const TEST_RESULTS_DIR: &str = ".report";

    /// Individual block context containing state bundle and access list.
    #[derive(Serialize, Deserialize, Debug, Clone)]
    pub struct BlockContext {
        pub index: u32,
        /// Block number
        pub number: u64,
        /// State changes from block execution
        pub bundle: alloy_primitives::map::HashMap<Address, BundleAccount>,
        /// Access list for the flashblock
        pub access_list: FlashblockAccessList,
        /// Provided Access List
        pub provided_access_list: FlashblockAccessList,
    }

    impl BlockContext {
        pub fn dump(self) -> eyre::Result<()> {
            let res = serde_json::to_string_pretty(&self)?;
            let dir = PathBuf::from(TEST_RESULTS_DIR).join(format!("failure_{}.json", self.number));

            std::fs::create_dir_all(dir.parent().unwrap_or(&PathBuf::from(".")))?;
            std::fs::write(&dir, res)?;
            Ok(())
        }
    }
}
