use alloy_primitives::{map::HashMap as AlloyHashMap, Address, FixedBytes};
use alloy_rpc_types_engine::PayloadId;
use flashblocks_primitives::access_list::FlashblockAccessList;
use reth_evm::block::BlockExecutionError;
use revm::database::BundleAccount;

use crate::diagnostics::{self, DiagnosticReport, FlashblockIndex};

pub mod bal_builder;
pub mod bal_executor;
pub mod factory;
pub mod temporal_db;
pub mod temporal_map;
pub mod utils;

/// Validation errors that occur when comparing builder and validator states.
///
/// When a validation error is created with diagnostic context, a detailed report
/// is automatically written to `target/test/{run_id}_report/failures/`.
#[derive(thiserror::Error, Debug)]
pub enum BalValidationError {
    #[error("Final access list hash mismatch. Expected {expected:x}, got {got:x}")]
    AccessListHashMismatch {
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
        #[source]
        diagnostic_report: Option<DiagnosticReportHandle>,
    },
    #[error("Incorrect state root after executing block. Expected {expected:x}, got {got:x}")]
    StateRootMismatch {
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
        #[source]
        diagnostic_report: Option<DiagnosticReportHandle>,
    },
}

impl BalValidationError {
    /// Create an access list hash mismatch error with diagnostic context.
    pub fn access_list_mismatch(
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
        payload_id: Option<PayloadId>,
        flashblock_index: Option<FlashblockIndex>,
    ) -> Self {
        let diagnostic_report = if let (Some(pid), Some(idx)) = (payload_id, flashblock_index) {
            let report = diagnostics::record_validation_failure(
                pid,
                idx,
                "AccessListHashMismatch",
                format!("Expected {expected:x}, got {got:x}"),
            );
            Some(DiagnosticReportHandle(report))
        } else {
            None
        };

        Self::AccessListHashMismatch {
            expected,
            got,
            diagnostic_report,
        }
    }

    /// Create a state root mismatch error with diagnostic context.
    pub fn state_root_mismatch(
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
        payload_id: Option<PayloadId>,
        flashblock_index: Option<FlashblockIndex>,
    ) -> Self {
        let diagnostic_report = if let (Some(pid), Some(idx)) = (payload_id, flashblock_index) {
            let report = diagnostics::record_validation_failure(
                pid,
                idx,
                "StateRootMismatch",
                format!("Expected {expected:x}, got {got:x}"),
            );
            Some(DiagnosticReportHandle(report))
        } else {
            None
        };

        Self::StateRootMismatch {
            expected,
            got,
            diagnostic_report,
        }
    }

    /// Create a state root mismatch error with explicit state data for diagnostics.
    /// This bypasses the global store lookup and directly includes the state in the report.
    pub fn state_root_mismatch_with_state(
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
        payload_id: PayloadId,
        flashblock_index: FlashblockIndex,
        validator_bundle: AlloyHashMap<Address, BundleAccount>,
        validator_access_list: FlashblockAccessList,
        expected_access_list: Option<FlashblockAccessList>,
    ) -> Self {
        // Record the validation failure counter
        {
            let mut store = diagnostics::DIAGNOSTIC_STORE.write();
            store.record_validation_failure();
        }

        // Create report with explicit state data
        let report = DiagnosticReport::with_states(
            payload_id,
            flashblock_index,
            "StateRootMismatch",
            format!("Expected {expected:x}, got {got:x}"),
            validator_bundle,
            validator_access_list,
            expected_access_list,
        );

        // Write to disk
        if let Err(e) = report.write_to_disk() {
            tracing::error!("Failed to write diagnostic report: {}", e);
        }

        Self::StateRootMismatch {
            expected,
            got,
            diagnostic_report: Some(DiagnosticReportHandle(report)),
        }
    }

    /// Get the diagnostic report if available.
    pub fn diagnostic_report(&self) -> Option<&DiagnosticReport> {
        match self {
            Self::AccessListHashMismatch { diagnostic_report, .. } => {
                diagnostic_report.as_ref().map(|h| &h.0)
            }
            Self::StateRootMismatch { diagnostic_report, .. } => {
                diagnostic_report.as_ref().map(|h| &h.0)
            }
        }
    }

    /// Get the path where the diagnostic report was written.
    pub fn report_path(&self) -> Option<std::path::PathBuf> {
        self.diagnostic_report().map(|r| {
            let store = diagnostics::DIAGNOSTIC_STORE.read();
            store.output_dir().join("failures").join(format!("{}.json", r.report_id))
        })
    }
}

/// Wrapper for DiagnosticReport to implement Error trait.
#[derive(Debug)]
pub struct DiagnosticReportHandle(pub DiagnosticReport);

impl std::fmt::Display for DiagnosticReportHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Diagnostic report: {}", self.0.report_id)
    }
}

impl std::error::Error for DiagnosticReportHandle {}

#[derive(thiserror::Error, Debug)]
pub enum BalExecutorError {
    #[error("BAL validation error: {0}")]
    BalValidationError(#[from] BalValidationError),
    #[error("Block execution error: {0}")]
    BlockExecutionError(#[from] BlockExecutionError),
    #[error("Missing block access list")]
    MissingBlockAccessList,
    #[error("Transaction decoding error: {0}")]
    TransactionDecodingError(#[from] eyre::Report),
    #[error("Missing Executed Block on Payload")]
    MissingExecutedBlock,
}

impl BalExecutorError {
    /// Get the diagnostic report if this is a validation error with diagnostics.
    pub fn diagnostic_report(&self) -> Option<&DiagnosticReport> {
        match self {
            Self::BalValidationError(e) => e.diagnostic_report(),
            _ => None,
        }
    }
}
