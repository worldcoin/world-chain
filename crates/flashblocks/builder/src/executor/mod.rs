use std::fmt::Debug;

use alloy_primitives::FixedBytes;
use reth_evm::block::BlockExecutionError;

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
        // #[source]
        // diagnostic_report: Option<DiagnosticHandle>,
    },
    #[error("Incorrect state root after executing block. Expected {expected:x}, got {got:x}")]
    StateRootMismatch {
        expected: FixedBytes<32>,
        got: FixedBytes<32>,
        // #[source]
        // diagnostic_report: Option<DiagnosticHandle>,
    },
}

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
