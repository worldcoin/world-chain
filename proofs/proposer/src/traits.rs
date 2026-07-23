use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use world_chain_proofs::ResolutionStatus;

use crate::{
    ParentRef, Proposal, ProposalSubmission, ProposerError,
    types::{ClaimOutcome, CloseGameSubmission, ResolveSubmission},
};

/// Contract surface needed by the asynchronous bond manager.
#[async_trait]
pub trait BondManagerClient: Send + Sync {
    /// Returns the address whose proposal credits are managed.
    fn proposer_address(&self) -> Address;

    /// Returns the number of games created by the factory.
    async fn game_count(&self) -> Result<u64, ProposerError>;

    /// Returns the game at the provided factory creation index, or `None` when the game is of
    /// a different game type.
    async fn game_at(&self, index: u64) -> Result<Option<Address>, ProposerError>;

    /// Returns the proposer that created the provided game.
    async fn game_proposer(&self, game: Address) -> Result<Address, ProposerError>;

    /// Returns the resolution status of the provided game.
    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError>;

    /// Advances the two-phase DelayedWETH claim for the managed proposer by at most one step.
    async fn claim_credits(&self, game: Address) -> Result<ClaimOutcome, ProposerError>;
}

/// Minimal contract surface needed by the proposer.
#[async_trait]
pub trait ProposerClient: Send + Sync {
    /// Reads the current anchor parents: the anchor sentinel, plus the anchor game as an
    /// index-addressed parent when one exists (children created before the anchor advanced
    /// reference it by factory index).
    async fn anchor_parents(&self) -> Result<Vec<ParentRef>, ProposerError>;

    /// Returns the game created for exactly this proposal (game type, root claim, extraData),
    /// if one exists.
    async fn find_game(&self, proposal: &Proposal) -> Result<Option<Address>, ProposerError>;

    /// Returns the factory creation index of the provided game.
    async fn game_index(&self, game: Address) -> Result<U256, ProposerError>;

    /// Returns the resolution status of the provided game, if game exists.
    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError>;

    /// Returns whether the registry considers the game finalized (resolved and past the
    /// finality airgap).
    async fn is_game_finalized(&self, game: Address) -> Result<bool, ProposerError>;

    /// Submits a resolve transaction to the provided game.
    async fn resolve_game(&self, game: Address) -> Result<ResolveSubmission, ProposerError>;

    /// Submits a closeGame transaction to the provided game.
    async fn close_game(&self, game: Address) -> Result<CloseGameSubmission, ProposerError>;

    /// Reads the proposer bond for the World Chain game type from the factory.
    async fn proposer_bond(&self) -> Result<U256, ProposerError>;

    /// Submits a `DisputeGameFactory.create` transaction for the proposal.
    async fn submit_proposal(
        &self,
        proposal: &Proposal,
        proposer_bond: U256,
    ) -> Result<ProposalSubmission, ProposerError>;
}
