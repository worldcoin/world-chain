use alloy_primitives::{Address, B256, BlockNumber, TxHash};

/// A game root state.
pub enum RootState {
    Proposed,
    Challenged,
    Finalized,
    Invalidated,
}

/// A WorldChainProofSystemGame view for the challenger.
pub struct Game {
    /// The game contract address.
    pub game: Address,
    /// The output root claim contained in this game.
    pub root_claim: B256,
    /// The L2 block number the `root_claim` refers to.
    pub l2_block_number: BlockNumber,
    /// The root state of this game.
    pub root_state: RootState,
    /// The deadline in UNIX timestamp this game must be challenged
    /// if `root_claim` is incorrect.
    pub challenge_deadline: u64,
}

/// Result of a submitted challenge transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChallengeSubmission {
    /// Transaction hash for the challenge submission.
    pub tx_hash: TxHash,
}
