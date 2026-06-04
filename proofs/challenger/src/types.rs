use alloy_primitives::{Address, B256, BlockNumber, TxHash};

/// A game root state.
#[derive(Debug, PartialEq, Eq)]
pub enum RootState {
    Proposed,
    Challenged,
    Finalized,
    Invalidated,
}

/// A WorldChainProofSystemGame view for the challenger.
#[derive(Debug)]
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

#[derive(Debug)]
pub struct GameCreated {
    pub proposal_key: B256,
    pub root_it: B256,
    pub game: Address,
    pub proposer: Address,
    pub root_claim: B256,
    pub l2_block_number: BlockNumber,
    pub parent_ref: Address,
    pub l1_origin_hash: B256,
    pub l1_origin_number: BlockNumber,
}
