use alloy_primitives::{Address, B256, TxHash, U256};
use world_chain_proofs::{InvalidationReason, ProposalCommitment};

/// The parent checkpoint selected from the anchor registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnchorRef {
    /// Current anchor game, or the registry sentinel before the first game is anchored.
    pub address: Address,
    /// L2 block number of the anchor output root.
    pub l2_block_number: u64,
}

impl AnchorRef {
    /// Returns the parent reference new proposals extending the anchor must use.
    #[must_use]
    pub const fn parent_ref(self) -> ParentRef {
        ParentRef {
            address: self.address,
            l2_block_number: self.l2_block_number,
        }
    }
}

/// A game already registered on the factory for a transition the proposer is scanning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionGame {
    /// Address of the game clone.
    pub address: Address,
    /// Retry nonce the game was created with.
    pub attempt: u64,
}

/// A pending `DelayedWETH` withdrawal opened by the first `claimCredit` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PendingWithdrawal {
    /// Amount unlocked in `DelayedWETH` and awaiting the withdrawal delay.
    pub amount: U256,
    /// Unix timestamp at which the unlocked amount becomes withdrawable.
    pub unlock_at: u64,
}

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
}

/// Candidate proposal data supplied to the dispute-game factory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proposal {
    /// Address of the anchor registry or parent game.
    pub parent_ref: Address,
    /// Claimed OP Stack output root.
    pub root_claim: B256,
    /// L2 block number for `root_claim`.
    pub l2_block_number: u64,
    /// Retry nonce. Non-zero only when replacing a game invalidated by a proof timeout.
    pub attempt: u64,
}

impl Proposal {
    /// Returns the commitment used to build the factory `extraData` and UUID.
    #[must_use]
    pub const fn commitment(&self) -> ProposalCommitment {
        ProposalCommitment {
            parent_ref: self.parent_ref,
            root_claim: self.root_claim,
            l2_block_number: self.l2_block_number,
            attempt: self.attempt,
        }
    }
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

/// Result of a `claimCredit` transaction.
///
/// Bond payout is two-phase: the first call unlocks the credit in `DelayedWETH`, the second
/// (after the WETH delay) withdraws and transfers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimSubmission {
    /// Transaction hash for the claim.
    pub tx_hash: TxHash,
    /// Amount moved by this phase.
    pub amount: U256,
    /// Whether this call finalized the withdrawal and transferred funds.
    pub withdrawn: bool,
}
