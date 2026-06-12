use alloy_primitives::TxHash;
use world_chain_proofs::GameCreated;

/// Result of a submitted challenge transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChallengeSubmission {
    /// Transaction hash for the challenge submission.
    pub tx_hash: TxHash,
}

/// A game queued for retry after a transient scan failure.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryGame {
    pub game_created: GameCreated,
    pub challenge_deadline: Option<u64>,
    pub attempts: u32,
}

/// Result of processing a single game.
#[derive(Debug, Clone, Copy)]
pub(crate) enum GameScanOutcome {
    Valid,
    NeedsChallenge { challenge_deadline: u64 },
    Skip,
}
