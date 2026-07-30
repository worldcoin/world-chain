use alloy_primitives::{Address, B256, TxHash};

/// Immutable game data needed to monitor and defend an output-root claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameMetadata {
    pub address: Address,
    pub domain_hash: B256,
    pub parent_ref: Address,
    pub root_claim: B256,
    pub l2_block_number: u64,
    pub l1_origin_hash: B256,
    pub l1_origin_number: u64,
    pub challenge_deadline: u64,
    pub proof_deadline: u64,
    pub proof_threshold: u8,
}

/// Result of a submitted `submitProofLane` transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefenderSubmission {
    /// Transaction hash for the proof submission.
    pub tx_hash: TxHash,
}

/// Result of a submitted `resolve` transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolveSubmission {
    pub tx_hash: TxHash,
}
