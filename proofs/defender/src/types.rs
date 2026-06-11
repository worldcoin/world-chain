use alloy_primitives::TxHash;

/// Result of a submitted `submitProofLane`` transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefenderSubmission {
    /// Transaction hash for the challenge submission.
    pub tx_hash: TxHash,
}
