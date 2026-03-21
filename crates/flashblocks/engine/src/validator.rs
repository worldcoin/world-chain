//! The [`PendingPayloadValidator`] trait and associated error/outcome types.
//!
//! Mirrors reth's `EngineValidator::validate_payload` → `ValidationOutcome<N>` pattern
//! but for flashblock diffs rather than full payloads.

use std::sync::Arc;

use alloy_consensus::Header;
use alloy_primitives::B256;
use alloy_rpc_types_engine::PayloadId;
use flashblocks_primitives::primitives::{
    ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1,
};
use reth_chain_state::ExecutedBlock;
use reth_evm::block::BlockExecutionError;
use reth_optimism_primitives::OpPrimitives;
use reth_primitives::SealedHeader;
use reth_primitives_traits::{NodePrimitives, RecoveredBlock};
use reth_provider::{BlockExecutionOutput, ProviderError};
use reth_trie_common::{HashedPostState, updates::TrieUpdates};

/// Structured error for flashblock diff validation.
///
/// Mirrors reth's `InsertBlockErrorKind` to provide typed error handling in the
/// processor — enabling different reactions to execution failures vs. provider
/// errors vs. validation mismatches.
#[derive(Debug, thiserror::Error)]
pub enum FlashblockValidationError {
    /// Block execution failed.
    #[error(transparent)]
    Execution(#[from] BlockExecutionError),
    /// Provider error (header not found, state unavailable).
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// Validation mismatch (BAL hash, state root, receipts root, block hash).
    #[error("{0}")]
    Validation(Box<dyn core::error::Error + Send + Sync + 'static>),
    /// Other errors.
    #[error(transparent)]
    Other(Box<dyn core::error::Error + Send + Sync + 'static>),
}

impl FlashblockValidationError {
    /// Wrap any error as a validation mismatch.
    pub fn validation(err: impl core::error::Error + Send + Sync + 'static) -> Self {
        Self::Validation(Box::new(err))
    }

    /// Wrap any error as an "other" error.
    pub fn other(err: impl core::error::Error + Send + Sync + 'static) -> Self {
        Self::Other(Box::new(err))
    }
}

/// The output of a successful flashblock diff validation.
///
/// Returns the block and execution output separately from trie inputs so that
/// the processor can construct [`DeferredTrieData::pending`] with the correct
/// chain of ancestor handles — enabling non-blocking trie resolution.
pub struct ValidatedFlashblock {
    /// The recovered block produced by execution.
    pub recovered_block: Arc<RecoveredBlock<<OpPrimitives as NodePrimitives>::Block>>,
    /// The execution output (bundle state, receipts, gas used, etc.).
    pub execution_output: Arc<BlockExecutionOutput<<OpPrimitives as NodePrimitives>::Receipt>>,
    /// Unsorted hashed post-state from execution.
    pub hashed_state: Arc<HashedPostState>,
    /// Unsorted trie updates from state root computation.
    pub trie_updates: Arc<TrieUpdates>,
    /// The persisted ancestor hash this trie input is anchored to.
    pub anchor_hash: B256,
}

/// Output of flashblock diff validation — mirrors reth's `ValidationOutcome<N>`.
pub type DiffValidationOutcome = Result<ValidatedFlashblock, FlashblockValidationError>;

/// Validates a single flashblock diff against accumulated state, producing a
/// [`ValidatedFlashblock`].
///
/// Implementations encapsulate all execution details (BAL parallel execution,
/// legacy sequential build, etc.). The processor only sees the trait — it
/// orchestrates epochs, caching, and broadcast without knowing *how* validation
/// works.
///
/// The trait returns [`ValidatedFlashblock`] (block + trie inputs) rather than
/// [`ExecutedBlock`] directly, so the processor can construct the
/// [`DeferredTrieData`] ancestor chain across flashblocks in the epoch.
pub trait PendingPayloadValidator: Send + Sync + 'static {
    /// Validate a flashblock diff against committed state.
    ///
    /// # Arguments
    ///
    /// * `committed` — Accumulated `ExecutedBlock` from prior flashblocks in
    ///   this epoch. `None` on the first flashblock of a new epoch.
    /// * `diff` — The flashblock delta to validate/execute.
    /// * `parent` — Sealed parent header for this epoch's block.
    /// * `payload_id` — Payload ID for this epoch.
    /// * `base` — Base execution payload configuration (parent_hash, timestamp,
    ///   fee_recipient, prev_randao, etc.).
    ///
    /// # Returns
    ///
    /// `DiffValidationOutcome` — a typed `Result<ValidatedFlashblock,
    /// FlashblockValidationError>`.
    fn validate_diff(
        &self,
        committed: Option<&ExecutedBlock<OpPrimitives>>,
        diff: ExecutionPayloadFlashblockDeltaV1,
        parent: &SealedHeader<Header>,
        payload_id: PayloadId,
        base: &ExecutionPayloadBaseV1,
    ) -> DiffValidationOutcome;
}
