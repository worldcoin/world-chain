#![cfg_attr(not(test), warn(unused_crate_dependencies))]

//! Crate containing World Chain payload building primitives.
//!
//! A payload builder for Optimism-based chains with [Flashblocks](https://www.flashbots.net/flashblocks)
//! support and Block Access List (BAL) construction.
//!
//! This crate provides the infrastructure for building block payloads that support flashblocks -
//! sub-block pre-confirmations that enable faster transaction inclusion and state updates before
//! full block finalization.
//!
//! ## Overview
//!
//! The flashblocks builder extends the standard Optimism payload builder with:
//!
//! - **Block Access List (BAL) Generation**: Tracks all state reads and writes during execution
//!   to enable parallel transaction validation
//! - **Incremental Payload Building**: Supports building payloads incrementally as new
//!   transactions arrive, reusing previously committed state
//!
//! ## Architecture
//!
//! The crate is organized into the following modules:
//!
//! ### Core Components
//!
//! - [`payload_builder`] - Main payload builder implementing [`FlashblocksPayloadBuilder`]
//! - `world_chain_evm::execution` - Block execution builders
//!
//! ### BAL Execution
//!
//! - Upstream `revm_database::State` BAL builders construct flashblock BAL sidecars
//! - Upstream `revm::state::bal::Bal` powers BAL-driven validation workers
//!
//! ### Supporting Modules
//!
//! - `world_chain_primitives::access_list` - Flashblock BAL sidecar serialization utilities
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
//! The BAL enables validators to install upstream revm BAL state for speculative transaction
//! execution. Canonical validation state rebuilds the BAL and commits ordered worker outputs.
//!
//! [`PayloadBuilder`]: reth_basic_payload_builder::PayloadBuilder
//! [`FlashblocksPayloadBuilder`]: payload_builder::FlashblocksPayloadBuilder
//! [`FlashblockPayloadBuilder::try_build_with_precommit`]: traits::payload_builder::FlashblockPayloadBuilder::try_build_with_precommit
//! [`BalBlockBuilder`]: world_chain_evm::execution::bal::BalBlockBuilder

mod execution_context;

/// Main payload builder implementation.
pub mod payload_builder;

/// Iterator over `PayloadTransactions`
///
/// See [`reth_payload_util::PayloadTransactions`]
pub mod payload_txns;

/// Traits for payload building abstractions.
pub mod traits;

/// Payload builder metrics.
pub mod payload_builder_metrics;

pub use execution_context::{WorldChainPayloadBuilderCtx, WorldChainPayloadBuilderCtxBuilder};
