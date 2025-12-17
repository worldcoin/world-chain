#![cfg_attr(not(test), warn(unused_crate_dependencies))]

//! Crate containing all World Chain Payload Building Primitives
//!
//! A payload builder for Optimism-based chains with [Flashblocks](https://www.flashbots.net/flashblocks)
//! support and Block Access List (BAL) construction.
//!
//! This crate provides the infrastructure for building block payloads that support flashblocks -
//! sub-block pre-confirmations that enable faster transaction inclusion and state updates before
//! full block finalization. Additionally this crate provides execution, and validation utilities for Block Access List (BAL) sidecars
//! on top of flashblocks.
//!
//! ## Overview
//!
//! The flashblocks builder extends the standard Optimism payload builder with:
//!
//! - **Block Access List (BAL) Generation**: Tracks all state reads and writes during execution
//!   to enable parallel transaction validation
//! - **Incremental Payload Building**: Supports building payloads incrementally as new
//!   transactions arrive, reusing previously committed state
//! - **Parallel Validation**: Validators can re-execute transactions in parallel using the BAL
//!   to verify state transitions without sequential dependency resolution
//!
//! ## Architecture
//!
//! The crate is organized into the following modules:
//!
//! ### Core Components
//!
//! - [`coordinator`] - Orchestrates flashblocks execution and P2P message handling
//! - [`payload_builder`] - Main payload builder implementing [`FlashblocksPayloadBuilder`]
//! - [`executor`] - Block executor with BAL construction ([`BalBlockExecutor`], [`BalBlockBuilder`])
//! - [`validator`] - Flashblock validation with parallel execution support
//!
//! ### Database Layer
//!
//! - [`database::bal_builder_db`] - Database wrapper that constructs BAL during execution
//! - [`database::temporal_db`] - Time-indexed database views for parallel validation
//! - [`database::bundle_db`] - Bundle state overlay for incremental execution
//!
//! ### Supporting Modules
//!
//! - [`access_list`] - BAL construction and serialization utilities
//! - [`assembler`] - Block assembly from execution results
//! - [`traits`] - Abstractions for payload building contexts and builders
//! - [`payload_txns`] - Transaction iteration with deduplication for incremental builds
//!
//! ## Usage
//!
//! ### Building Payloads
//!
//! The [`FlashblocksPayloadBuilder`] implements the standard reth [`PayloadBuilder`] trait
//! and can be used with the reth payload builder service:
//!
//! ```ignore
//! use flashblocks_builder::{FlashblocksPayloadBuilder, FlashblocksPayloadBuilderConfig};
//!
//! let config = FlashblocksPayloadBuilderConfig {
//!     bal_enabled: true,
//!     ..Default::default()
//! };
//!
//! let builder = FlashblocksPayloadBuilder {
//!     evm_config,
//!     pool,
//!     client,
//!     config,
//!     best_transactions: (),
//!     ctx_builder,
//! };
//! ```
//!
//! ### Incremental Building with Pre-commits
//!
//! For flashblocks, use [`FlashblockPayloadBuilder::try_build_with_precommit`] to build
//! incrementally on top of a previously committed payload:
//!
//! ```ignore
//! use flashblocks_builder::traits::payload_builder::FlashblockPayloadBuilder;
//!
//! // Build first flashblock
//! let (outcome, access_list) = builder.try_build_with_precommit(args, None)?;
//!
//! // Build next flashblock, reusing committed state
//! let (outcome2, access_list2) = builder.try_build_with_precommit(
//!     args2,
//!     Some(&outcome.payload()),
//! )?;
//! ```
//!
//! ### Parallel Validation
//!
//! Validators use [`FlashblocksBlockValidator`] to re-execute and verify flashblocks
//! using the BAL for parallel transaction execution:
//!
//! ```ignore
//! use flashblocks_builder::validator::{FlashblocksBlockValidator, FlashblockBlockValidator};
//!
//! let validator = FlashblocksBlockValidator::new(ctx);
//! let payload = validator.validate_flashblock_parallel(
//!     state_provider,
//!     diff,
//!     parent,
//!     payload_id,
//! )?;
//! ```
//!
//! ## Block Access List (BAL)
//!
//! The BAL is a data structure that records all state accesses during block execution,
//! indexed by transaction position. It contains:
//!
//! - **Storage changes**: Slot writes with transaction index and new value
//! - **Storage reads**: Slots read during execution (for dependency tracking)
//! - **Balance changes**: Account balance modifications per transaction
//! - **Nonce changes**: Account nonce updates per transaction
//! - **Code changes**: Contract deployments per transaction
//!
//! The BAL enables validators to construct a [`TemporalDb`] that provides each transaction
//! with a view of state as it existed at that point in execution, allowing parallel
//! re-execution without sequential dependency resolution.
//!
//! ## Coordinator
//!
//! The [`FlashblocksExecutionCoordinator`] manages the lifecycle of flashblocks:
//!
//! 1. Receives flashblock messages from the P2P network
//! 2. Validates and executes incoming flashblocks
//! 3. Publishes locally-built flashblocks to the network
//! 4. Maintains the current pending block state for RPC queries
//!
//! [`PayloadBuilder`]: reth_basic_payload_builder::PayloadBuilder
//! [`FlashblocksPayloadBuilder`]: payload_builder::FlashblocksPayloadBuilder
//! [`FlashblockPayloadBuilder::try_build_with_precommit`]: traits::payload_builder::FlashblockPayloadBuilder::try_build_with_precommit
//! [`FlashblocksBlockValidator`]: validator::FlashblocksBlockValidator
//! [`FlashblocksExecutionCoordinator`]: coordinator::FlashblocksExecutionCoordinator
//! [`BalBlockExecutor`]: executor::BalBlockExecutor
//! [`BalBlockBuilder`]: executor::BalBlockBuilder
//! [`TemporalDb`]: database::temporal_db::TemporalDb

/// Utilities for constructing and serializing Block Access Lists (BAL).
pub mod access_list;

/// Underlying block executor and builder with BAL construction.
pub mod bal_executor;

/// Flashblock validation with parallel execution support.
pub mod bal_validator;

/// Flashblocks execution coordinator.
///
/// The [`FlashblocksExecutionCoordinator`] orchestrates flashblock execution:
/// - Listens for flashblocks from the P2P network
/// - Validates and executes incoming flashblocks
/// - Publishes locally-built flashblocks
/// - Maintains pending block state for RPC queries
///
/// [`FlashblocksExecutionCoordinator`]: coordinator::FlashblocksExecutionCoordinator
pub mod coordinator;

/// Main payload builder implementation.
pub mod payload_builder;

/// Iterator over `PayloadTransactions`
///
/// See [`reth_payload_util::PayloadTransactions`]
pub mod payload_txns;

/// Traits for payload building abstractions.
pub mod traits;

/// Database abstractions for BAL construction and temporal state access.
pub mod database;

/// Configuration for the flashblocks payload builder.
#[derive(Default, Debug, Clone)]
pub struct FlashblocksPayloadBuilderConfig {
    /// Inner Optimism payload builder configuration.
    ///
    /// Contains settings for data availability, compute gas limits, and other
    /// OP-stack specific options.
    pub inner: reth_optimism_payload_builder::config::OpBuilderConfig,

    /// Whether to enable Block Access List (BAL) generation.
    ///
    /// When enabled, the builder will track all state accesses during execution
    /// and include the BAL in flashblock payloads. This enables parallel
    /// validation by recipients.
    ///
    /// Defaults to `false`.
    pub bal_enabled: bool,
}
