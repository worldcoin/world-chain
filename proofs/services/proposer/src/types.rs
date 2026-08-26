use alloy_primitives::{Address, B256, TxHash};
use world_chain_proof_protocol::{InvalidationReason, ProposalCommitment, SelectedLineage};

/// The selected lineage discovered by the proposer and the action available at its tip.
#[derive(Debug)]
pub struct ProposerScan {
    lineage: SelectedLineage,
    next_action: NextProposalAction,
}

impl ProposerScan {
    pub(crate) const fn new(lineage: SelectedLineage, next_action: NextProposalAction) -> Self {
        Self {
            lineage,
            next_action,
        }
    }

    /// Returns the valid selected lineage found by the scan.
    #[must_use]
    pub const fn lineage(&self) -> &SelectedLineage {
        &self.lineage
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
    /// Resolve the selected game with its determined negative outcome.
    ResolveNegative {
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
