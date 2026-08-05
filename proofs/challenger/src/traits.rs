use crate::{
    ChallengerError,
    types::{
        ChallengeSubmission, ClaimSubmission, GameMetadata, PendingWithdrawal, ResolveSubmission,
    },
};
use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use world_chain_proofs::{ClaimData, ResolutionStatus};

/// Contract surface needed by the output-root challenger.
#[async_trait]
pub trait ChallengerClient: Send + Sync {
    /// Returns the total number of games indexed by the dispute-game factory, across all
    /// game types.
    async fn game_count(&self) -> Result<u64, ChallengerError>;
    /// Returns the WIP-1006 game at the provided factory index, or `None` when that index
    /// holds a game of a different type.
    async fn game_address_at(&self, index: u64) -> Result<Option<Address>, ChallengerError>;
    /// Reads the immutable game data needed to validate its root claim.
    async fn game_metadata(&self, game: Address) -> Result<GameMetadata, ChallengerError>;
    /// Reads the claim state of the provided game.
    async fn claim_data(&self, game: Address) -> Result<ClaimData, ChallengerError>;
    /// Reads the challenge deadline of the provided game.
    async fn challenge_deadline(&self, game: Address) -> Result<u64, ChallengerError>;
    /// Submits a challenge against an invalid game, bonded with that game's own
    /// `challengerBond`.
    async fn submit_challenge(&self, game: Address)
    -> Result<ChallengeSubmission, ChallengerError>;
}

/// Contract surface needed to resolve challenger-owned games.
#[async_trait]
pub trait ResolutionManagerClient: Send + Sync {
    /// Returns the current resolution evaluation for the provided game.
    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ChallengerError>;
    /// Submits a resolution transaction.
    async fn resolve(&self, game: Address) -> Result<ResolveSubmission, ChallengerError>;
}

/// Contract surface needed by the asynchronous challenger bond manager.
#[async_trait]
pub trait BondManagerClient: ResolutionManagerClient {
    /// Returns the address whose challenge bonds are managed.
    fn challenger_address(&self) -> Address;
    /// Returns the total number of games indexed by the dispute-game factory, across all
    /// game types.
    async fn game_count(&self) -> Result<u64, ChallengerError>;
    /// Returns the WIP-1006 game at the provided factory index, or `None` when that index
    /// holds a game of a different type.
    async fn game_address_at(&self, index: u64) -> Result<Option<Address>, ChallengerError>;
    /// Returns the challenger recorded by the provided game.
    async fn game_challenger(&self, game: Address) -> Result<Address, ChallengerError>;
    /// Returns whether the registry's finality airgap has elapsed for the provided game.
    ///
    /// `claimCredit` calls `closeGame`, which reverts until this holds.
    async fn is_game_finalized(&self, game: Address) -> Result<bool, ChallengerError>;
    /// Returns the credit the managed challenger can unlock from the provided game.
    async fn credit(&self, game: Address) -> Result<U256, ChallengerError>;
    /// Returns the managed challenger's pending `DelayedWETH` withdrawal for the game.
    async fn pending_withdrawal(&self, game: Address)
    -> Result<PendingWithdrawal, ChallengerError>;
    /// Returns the latest L1 block timestamp used by `DelayedWETH`.
    async fn latest_l1_timestamp(&self) -> Result<u64, ChallengerError>;
    /// Advances the managed challenger's two-phase bond claim on the provided game.
    async fn claim_credit(&self, game: Address) -> Result<ClaimSubmission, ChallengerError>;
}
