//! Validated Flashblock publication for preconfirmation consumers.

use alloy_eips::BlockNumHash;
use alloy_rpc_types_engine::PayloadId;
use reth_chain_state::ExecutedBlock;
use reth_optimism_primitives::OpPrimitives;
use std::{sync::Arc, time::Instant};

/// A cumulative executed payload accepted against one canonical checkpoint.
#[derive(Debug, Clone)]
pub struct ValidatedFlashblock {
    pub sequence: u64,
    pub canonical_generation: u64,
    pub payload_id: PayloadId,
    pub index: u64,
    pub executed: Arc<ExecutedBlock<OpPrimitives>>,
    /// Original execution/publication time, retained across duplicate deliveries.
    pub validated_at: Instant,
}

/// An atomic checkpoint and current validated payload.
#[derive(Debug, Clone, Default)]
pub struct ValidatedSnapshot {
    pub canonical_tip: Option<BlockNumHash>,
    pub canonical_generation: u64,
    pub current: Option<ValidatedFlashblock>,
}

/// Bounded notifications; receivers must handle lag by obtaining a fresh snapshot.
#[derive(Debug, Clone)]
pub enum ValidatedEvent {
    Payload(ValidatedFlashblock),
    /// A source checkpoint change, not proof of canonical log inclusion.
    Reset {
        canonical_tip: Option<BlockNumHash>,
        canonical_generation: u64,
    },
}
