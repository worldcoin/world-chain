#![cfg_attr(not(any(test, feature = "bench")), warn(unused_crate_dependencies))]

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

use std::sync::Arc;

use alloy_consensus::{Block, transaction::SignerRecoverable};
use alloy_eips::{Decodable2718, eip2718::WithEncoded};
use alloy_op_evm::{OpBlockExecutionCtx, OpBlockExecutor, OpEvmFactory};
use alloy_rpc_types_engine::ExecutionData;
use flashblocks_primitives::{
    ed25519_dalek::ed25519::signature::rand_core::le, primitives::FlashblocksPayloadV1,
};
use op_alloy_rpc_types_engine::OpExecutionData;
use reth::{
    core::primitives::SealedBlock, network::test_utils::transactions,
    revm::database::StateProviderDatabase,
};
use reth_chain_state::{ComputedTrieData, ExecutedBlock};
use reth_evm::{
    Evm, EvmEnv, EvmFactory,
    block::{BlockExecutionError, BlockExecutor, CommitChanges, ExecutableTx, StateDB},
    execute::{BlockBuilder, BlockBuilderOutcome, ExecutorTx},
    op_revm::OpSpecId,
};
use reth_node_api::{NodePrimitives, PayloadTypes};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::OpPayloadPrimitives;
use reth_optimism_payload_builder::{OpExecutionPayloadValidator, config::OpBuilderConfig};
use reth_optimism_primitives::{OpPrimitives, OpTransactionSigned};
use reth_primitives::{Recovered, SealedHeader};
use reth_provider::{BlockExecutionOutput, StateProvider};
use revm::handler::execution;
use revm_database::BundleState;

use crate::{bal_validator::decode_transactions_with_indices, executor::FlashblocksBlockBuilder};

/// Utilities for constructing and serializing Block Access Lists (BAL).
pub mod access_list;

pub mod processor;

/// Underlying block executor and builder with BAL construction.
pub mod bal_executor;

/// Flashblock validation with parallel execution support.
pub mod validator;

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

/// Standard Flashblock Block Builder (without BAL support).
pub mod executor;

/// Block building utilities
pub mod utils;

/// Metric name constants.
pub mod metrics;
/// Test utilities shared between integration tests and benchmarks.
///
/// Available when:
/// - `cfg(test)` is set (unit tests within this crate).
/// - The `test-utils` feature is enabled (integration tests activate this
///   via a self-referencing dev-dependency; benchmarks use the `bench` feature
///   which implies `test-utils`).
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

/// Configuration for the flashblocks payload builder.
#[derive(Default, Debug, Clone)]
pub struct FlashblocksPayloadBuilderConfig {
    /// Inner Optimism payload builder configuration.
    ///
    /// Contains settings for data availability, compute gas limits, and other
    /// OP-stack specific options.
    pub inner: OpBuilderConfig,

    /// Whether to enable Block Access List (BAL) generation.
    ///
    /// When enabled, the builder will track all state accesses during execution
    /// and include the BAL in flashblock payloads. This enables parallel
    /// validation by recipients.
    ///
    /// Defaults to `false`.
    pub bal_enabled: bool,
}

pub trait BlockBuilderExt: BlockBuilder {
    /// Completes the block building process and returns the [`BlockBuilderOutcome`], and [`BundleState`].
    fn finish_with_bundle(
        self,
        state_provider: impl StateProvider,
        metrics: Option<&mut metrics::PayloadBuildAttemptMetrics>,
    ) -> Result<(BlockBuilderOutcome<Self::Primitives>, BundleState), BlockExecutionError>;

    fn set_committed_state(&mut self, state: &ExecutedBlock<Self::Primitives>);

    fn chain_spec(&self) -> impl OpHardforks;
}

pub struct FlashblockBlockValidator {
    chain_spec: Arc<OpChainSpec>,
    execution_context: OpBlockExecutionCtx,
    sealed_header: SealedHeader,
    evm_env: EvmEnv<OpSpecId>,
}

impl FlashblockBlockValidator {
    pub fn new(
        chain_spec: Arc<OpChainSpec>,
        execution_context: OpBlockExecutionCtx,
        sealed_header: SealedHeader,
        evm_env: EvmEnv<OpSpecId>,
    ) -> Self {
        Self {
            chain_spec,
            execution_context,
            sealed_header,
            evm_env,
        }
    }

    pub fn builder(
        &self,
        provider: impl StateProvider + Clone + 'static,
        state: ExecutedBlock<OpPrimitives>,
    ) -> impl BlockBuilderExt<Primitives = OpPrimitives> {
        let receipts = state.execution_output.receipts.clone();

        let transactions = state.recovered_block().clone_transactions_recovered();

        let database = StateProviderDatabase::new(provider.clone());
        let db = revm::database::State::builder()
            .with_database(database)
            .with_bundle_update()
            .with_bundle_prestate(state.execution_outcome().state.clone())
            .build();

        let evm = OpEvmFactory::default().create_evm(db, self.evm_env.clone());

        let mut executor = OpBlockExecutor::new(
            evm,
            self.execution_context.clone(),
            (*self.chain_spec).clone(),
            OpRethReceiptBuilder::default(),
        );
        executor.receipts = receipts;
        executor.gas_used = state.execution_outcome().gas_used;

        FlashblocksBlockBuilder::<OpPrimitives, _, _>::new(
            self.execution_context.clone(),
            &self.sealed_header,
            executor,
            transactions.collect(),
            self.chain_spec.clone(),
        )
    }

    pub fn validate_payload_with_state(
        self,
        state_provider: impl StateProvider + Clone + 'static,
        transactions: &[alloy_primitives::Bytes],
        pending: Option<ExecutedBlock<OpPrimitives>>,
    ) -> Result<ExecutedBlock<OpPrimitives>, BlockExecutionError> {
        let mut builder = self.builder(
            state_provider.clone(),
            pending.clone().unwrap_or_default().clone(),
        );

        decode_transactions_with_indices(
            transactions,
            pending
                .as_ref()
                .map_or(0_usize, |b| b.recovered_block().transaction_count()) as u16,
        )
        .map_err(BlockExecutionError::other)?
        .into_iter()
        .map(|(_, tx)| builder.execute_transaction(tx));

        let (outcome, bundle) = builder.finish_with_bundle(state_provider)?;

        let computed_trie_data = ComputedTrieData::without_trie_input(
            outcome.hashed_state.into_sorted().into(),
            outcome.trie_updates.into_sorted().into(),
        );

        let executed = ExecutedBlock::new(
            outcome.block.into(),
            BlockExecutionOutput {
                result: outcome.execution_result,
                state: bundle,
            }
            .into(),
            computed_trie_data,
        );

        Ok(executed)
    }
}
