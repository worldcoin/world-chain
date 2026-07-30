use alloy_primitives::{Address, B256, TxHash};

/// Immutable game data needed to monitor and defend an output-root claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameMetadata {
    pub address: Address,
    pub root_claim: B256,
    pub l2_block_number: u64,
    pub l1_origin_hash: B256,
    pub challenge_deadline: u64,
    pub proof_deadline: u64,
    pub proof_threshold: u8,
    /// Domain hash the game was created against. Part of every lane verifier's proof payload.
    pub domain_hash: B256,
    /// Parent game, or the anchor registry sentinel. Part of every lane verifier's payload.
    pub parent_ref: Address,
    /// L1 block number paired with `l1_origin_hash`. Part of every lane verifier's payload.
    pub l1_origin_number: u64,
}

/// Result of a submitted `submitProofLane` transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefenderSubmission {
    /// Transaction hash for the challenge submission.
    pub tx_hash: TxHash,
}
