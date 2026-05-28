#![cfg_attr(not(test), warn(unused_crate_dependencies))]

//! World Chain flashblock validation and coordination.

/// Flashblocks execution coordinator.
pub mod coordinator;

/// Execution and state-root strategy traits and concrete implementations.
pub mod execution_strategy;

mod flashblock_types;

/// Flashblock validation metrics.
pub mod flashblock_validation_metrics;

/// State-root strategy traits and implementations.
pub mod state_root_strategy;

/// Flashblock validation with parallel execution support.
pub mod validator;
