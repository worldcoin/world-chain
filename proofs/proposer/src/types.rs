use alloy_primitives::{Address, B256, TxHash, U256};
use world_chain_proofs::{ANCHOR_PARENT_INDEX, InvalidationReason, extra_data};

/// The canonical lineage discovered by the proposer and the action available at its tip.
#[derive(Debug)]
pub struct CanonicalScan {
    canonical_line: CanonicalLine,
    next_action: NextProposalAction,
}

impl CanonicalScan {
    pub(crate) const fn new(
        canonical_line: CanonicalLine,
        next_action: NextProposalAction,
    ) -> Self {
        Self {
            canonical_line,
            next_action,
        }
    }

    /// Returns the valid canonical lineage found by the scan.
    #[must_use]
    pub const fn canonical_line(&self) -> &CanonicalLine {
        &self.canonical_line
    }

    /// Returns the action available at the tip of the canonical lineage.
    #[must_use]
    pub const fn next_action(&self) -> &NextProposalAction {
        &self.next_action
    }
}

/// The action the proposer may take after scanning the canonical lineage.
#[derive(Debug, PartialEq, Eq)]
pub enum NextProposalAction {
    /// Submit a new transition for which no game exists.
    Propose(Proposal),
    /// Replace a game invalidated by a direct proof timeout.
    RetryTimedOut {
        /// Proposal data for the replacement game.
        proposal: Proposal,
        /// Invalidated game that this proposal replaces.
        invalidated_game: Address,
    },
    /// Wait for the challenger to resolve a game whose outcome is negative.
    AwaitNegativeResolution {
        /// Game that is ready to resolve negatively.
        game: Address,
        /// Negative outcome reported by the game.
        reason: InvalidationReason,
    },
    /// Stop because the factory does not permit retrying this invalidated transition.
    BlockedByInvalidation {
        /// Invalidated game occupying the proposal key.
        game: Address,
        /// Reason the transition cannot be retried automatically.
        reason: InvalidationReason,
    },
    /// No transition can be proposed beyond the current finalized L2 head.
    CaughtUp {
        /// Next L2 block the proposer would target.
        target_block: u64,
        /// Current finalized L2 block reported by the consensus client.
        finalized_block: u64,
    },
}

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

/// Canonical games that have reached the finalized state and may advance the anchor.
#[derive(Debug, Default)]
pub struct FinalizedGames {
    /// Finalized games ordered by increasing L2 block number.
    pub games: Vec<ParentRef>,
}

impl FinalizedGames {
    /// Appends a finalized game to the ordered collection.
    pub fn push(&mut self, game: ParentRef) {
        self.games.push(game);
    }

    /// Returns the finalized game with the highest L2 block number, if any.
    #[must_use]
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
    /// `parentIndex` a child proposal must reference: the parent game's factory index, or
    /// [`ANCHOR_PARENT_INDEX`] when the parent is the current anchor.
    pub parent_index: U256,
}

impl ParentRef {
    /// Returns whether this parent is the anchor sentinel.
    #[must_use]
    pub fn is_anchor(&self) -> bool {
        self.parent_index == ANCHOR_PARENT_INDEX
    }
}

/// Candidate proposal data supplied to `DisputeGameFactory.create`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proposal {
    /// Factory index of the parent game, or [`ANCHOR_PARENT_INDEX`].
    pub parent_index: U256,
    /// Address of the anchor registry or parent game (informational; the on-chain identity is
    /// `parent_index`).
    pub parent_ref: Address,
    /// Claimed OP Stack output root.
    pub root_claim: B256,
    /// L2 block number for `root_claim`.
    pub l2_block_number: u64,
    /// Retry nonce; attempt N requires attempt N-1 to have timed out on proofs.
    pub attempt: U256,
}

impl Proposal {
    /// ABI-encoded `extraData` identifying this proposal in the factory.
    #[must_use]
    pub fn extra_data(&self) -> Vec<u8> {
        extra_data(self.l2_block_number, self.parent_index, self.attempt)
    }
}

/// Outcome of one bond-claim step against a game (two-phase DelayedWETH flow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// The game is not yet claimable (unresolved, inside the finality airgap, or inside the
    /// DelayedWETH withdrawal delay).
    NotReady,
    /// Phase 1 executed: credit unlocked in DelayedWETH.
    Unlocked {
        /// Transaction hash for the unlock submission.
        tx_hash: TxHash,
        /// Amount unlocked.
        amount: U256,
    },
    /// Phase 2 executed: funds withdrawn and transferred to the proposer.
    Claimed {
        /// Transaction hash for the withdrawal submission.
        tx_hash: TxHash,
        /// Amount transferred.
        amount: U256,
    },
    /// The game holds no credit for the proposer; nothing to claim now or later.
    NoCredit,
}

/// Result of a submitted proposal transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposalSubmission {
    /// Transaction hash for the proposal submission.
    pub tx_hash: TxHash,
    /// Address of the proof-system game created by the proposal.
    pub game_address: Address,
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
