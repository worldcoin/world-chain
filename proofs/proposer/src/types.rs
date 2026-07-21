use alloy_primitives::{Address, B256, TxHash};
use world_chain_proofs::ProposalCommitment;

/// The current anchor checkpoint and the canonical games built on top of it.
#[derive(Debug)]
pub struct CanonicalLine {
    anchor: ParentRef,
    games: Vec<ParentRef>,
}

impl CanonicalLine {
    /// Creates an empty canonical line rooted at `anchor`.
    #[must_use]
    pub const fn new(anchor: ParentRef) -> Self {
        Self {
            anchor,
            games: Vec::new(),
        }
    }

    /// Returns the checkpoint this canonical line is rooted at.
    #[must_use]
    pub const fn anchor(&self) -> ParentRef {
        self.anchor
    }

    /// Appends a canonical game built on the current tip.
    pub fn push_game(&mut self, game: ParentRef) {
        self.games.push(game);
    }

    /// Returns the canonical games built on top of the anchor.
    #[must_use]
    pub fn games(&self) -> &[ParentRef] {
        &self.games
    }

    /// Returns the last canonical game, or the anchor when no game exists yet.
    #[must_use]
    pub fn tip(&self) -> ParentRef {
        self.games.last().copied().unwrap_or(self.anchor)
    }
}

#[derive(Debug, Default)]
pub struct FinalizedGames {
    pub games: Vec<ParentRef>,
}

impl FinalizedGames {
    pub fn push(&mut self, game: ParentRef) {
        self.games.push(game);
    }

    pub fn last(&self) -> Option<ParentRef> {
        self.games.last().copied()
    }
}

/// A parent reference that the next proposal should build on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParentRef {
    /// Address of the anchor registry or parent game.
    pub address: Address,
    /// L2 block number of the parent output root.
    pub l2_block_number: u64,
}

/// Candidate proposal data supplied to the proof-system factory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proposal {
    /// Address of the anchor registry or parent game.
    pub parent_ref: Address,
    /// Claimed OP Stack output root.
    pub root_claim: B256,
    /// L2 block number for `root_claim`.
    pub l2_block_number: u64,
    /// Deterministic factory lookup key, excluding L1 origin.
    pub proposal_key: B256,
}

impl Proposal {
    /// Returns the proposal commitment used to compute the factory lookup key.
    #[must_use]
    pub const fn commitment(&self) -> ProposalCommitment {
        ProposalCommitment {
            parent_ref: self.parent_ref,
            root_claim: self.root_claim,
            l2_block_number: self.l2_block_number,
        }
    }
}

/// Result of a submitted proposal transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposalSubmission {
    /// Transaction hash for the proposal submission.
    pub tx_hash: TxHash,
}

/// Result of a resolve transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolveSubmission {
    /// Transaction hash for the resolve submission.
    pub tx_hash: TxHash,
}

/// Result of a closeGame transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseGameSubmission {
    /// Transaction hash for the closeGame submission.
    pub tx_hash: TxHash,
}
