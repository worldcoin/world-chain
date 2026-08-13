use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use world_chain_proof_protocol::{LineageProvider, ResolutionStatus};

use crate::{
    Proposal, ProposalSubmission, ProposerError,
    types::{ClaimSubmission, CloseGameSubmission, PendingWithdrawal, ResolveSubmission},
};

/// Contract surface needed by the asynchronous bond manager.
#[async_trait]
pub trait BondManagerClient: Send + Sync {
    /// Returns the address whose proposal credits are managed.
    fn proposer_address(&self) -> Address;

    /// Returns the total number of games indexed by the dispute-game factory, across all
    /// game types.
    async fn game_count(&self) -> Result<u64, ProposerError>;

    /// Returns the WIP-1006 game at the provided factory index, or `None` when that index
    /// holds a game of a different type.
    async fn game_at(&self, index: u64) -> Result<Option<Address>, ProposerError>;

    /// Returns the account that created the provided game.
    async fn game_creator(&self, game: Address) -> Result<Address, ProposerError>;

    /// Returns the resolution status of the provided game.
    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError>;

    /// Resolves a proposer-owned game invalidated by its parent.
    async fn resolve_game(&self, game: Address) -> Result<ResolveSubmission, ProposerError>;

    /// Returns whether the registry's finality airgap has elapsed for the provided game.
    ///
    /// `claimCredit` calls `closeGame`, which reverts until this holds.
    async fn is_game_finalized(&self, game: Address) -> Result<bool, ProposerError>;

    /// Returns the credit the managed proposer can unlock from the provided game.
    async fn credit(&self, game: Address) -> Result<U256, ProposerError>;

    /// Returns the managed proposer's pending `DelayedWETH` withdrawal for the provided game.
    async fn pending_withdrawal(&self, game: Address) -> Result<PendingWithdrawal, ProposerError>;

    /// Returns the latest L1 block timestamp used by `DelayedWETH`.
    async fn latest_l1_timestamp(&self) -> Result<u64, ProposerError>;

    /// Advances the managed proposer's two-phase bond claim on the provided game.
    async fn claim_credit(&self, game: Address) -> Result<ClaimSubmission, ProposerError>;
}

/// Minimal contract surface needed by the proposer.
#[async_trait]
pub trait ProposerClient: LineageProvider {
    /// Submits a resolve transaction to the provided game.
    async fn resolve_game(&self, game: Address) -> Result<ResolveSubmission, ProposerError>;

    /// Returns whether the registry's finality airgap has elapsed for the provided game.
    async fn is_game_finalized(&self, game: Address) -> Result<bool, ProposerError>;

    /// Submits a closeGame transaction to the provided game.
    async fn close_game(&self, game: Address) -> Result<CloseGameSubmission, ProposerError>;

    /// Creates the proposal's game through the dispute-game factory.
    async fn submit_proposal(
        &self,
        proposal: &Proposal,
    ) -> Result<ProposalSubmission, ProposerError>;
}
