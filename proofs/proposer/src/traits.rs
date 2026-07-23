use alloy_primitives::{Address, B256, U256};
use async_trait::async_trait;
use world_chain_proofs::{ProposalCommitment, ResolutionStatus};

use crate::{
    ParentRef, Proposal, ProposalSubmission, ProposerError,
    types::{CloseGameSubmission, ResolveSubmission, WithdrawSubmission},
};

/// Contract surface needed by the asynchronous bond manager.
#[async_trait]
pub trait BondManagerClient: Send + Sync {
    /// Returns the address whose proposal credits are managed.
    fn proposer_address(&self) -> Address;

    /// Returns the number of games created by the factory.
    async fn game_count(&self) -> Result<u64, ProposerError>;

    /// Returns the game at the provided factory creation index.
    async fn game_at(&self, index: u64) -> Result<Address, ProposerError>;

    /// Returns the proposer that created the provided game.
    async fn game_proposer(&self, game: Address) -> Result<Address, ProposerError>;

    /// Returns the resolution status of the provided game.
    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError>;

    /// Returns the claimable amount for the managed proposer.
    async fn claimable(&self, game: Address) -> Result<U256, ProposerError>;

    /// Withdraws credits for the managed proposer.
    async fn withdraw(&self, game: Address) -> Result<WithdrawSubmission, ProposerError>;
}

/// Minimal contract surface needed by the proposer.
#[async_trait]
pub trait ProposerClient: Send + Sync {
    /// Reads the parent state from the anchor registry.
    async fn anchor_parent(&self) -> Result<ParentRef, ProposerError>;

    /// Computes the deterministic proposal key used by the factory lookup.
    async fn proposal_key(&self, commitment: ProposalCommitment) -> Result<B256, ProposerError>;

    /// Returns an existing game for `commitment`, if one exists.
    async fn game_for_proposal(
        &self,
        commitment: ProposalCommitment,
    ) -> Result<Option<Address>, ProposerError>;

    /// Returns the resolution status of the provided game, if game exists.
    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError>;

    /// Submits a resolve transaction to the provided game.
    async fn resolve_game(&self, game: Address) -> Result<ResolveSubmission, ProposerError>;

    /// Submits a closeGame transaction to the provided game.
    async fn close_game(&self, game: Address) -> Result<CloseGameSubmission, ProposerError>;

    /// Returns the claimable amount the proposer can withdraw from the provided game.
    async fn claimable(&self, game: Address) -> Result<U256, ProposerError>;

    /// Submits a withdraw transaction to the provided game.
    async fn withdraw(&self, game: Address) -> Result<WithdrawSubmission, ProposerError>;

    /// Reads the proposer bond from the factory contract.
    async fn proposer_bond(&self) -> Result<U256, ProposerError>;

    /// Submits a proposal transaction to the factory.
    async fn submit_proposal(
        &self,
        proposal: &Proposal,
        proposer_bond: U256,
    ) -> Result<ProposalSubmission, ProposerError>;
}
