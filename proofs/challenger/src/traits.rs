use crate::{
    ChallengerError,
    types::{ChallengeSubmission, GameMetadata, ResolveSubmission, WithdrawSubmission},
};
use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use world_chain_proofs::{ResolutionStatus, RootState};

/// Contract surface needed by the output-root challenger.
#[async_trait]
pub trait ChallengerClient: Send + Sync {
    /// Reads the challenge bond configured by the factory.
    async fn challenger_bond(&self) -> Result<U256, ChallengerError>;
    /// Returns the number of games created by the factory.
    async fn game_count(&self) -> Result<u64, ChallengerError>;
    /// Returns the WIP-1006 game at a global factory index, or `None` for another game type.
    async fn game_address_at(&self, index: u64) -> Result<Option<Address>, ChallengerError>;
    /// Reads the immutable game data needed to validate its root claim.
    async fn game_metadata(&self, game: Address) -> Result<GameMetadata, ChallengerError>;
    /// Reads the root state of the provided game.
    async fn root_state(&self, game: Address) -> Result<RootState, ChallengerError>;
    /// Reads the challenge deadline of the provided game.
    async fn challenge_deadline(&self, game: Address) -> Result<u64, ChallengerError>;
    /// Submits a challenge against an invalid game.
    async fn submit_challenge(
        &self,
        game: Address,
        challenger_bond: U256,
    ) -> Result<ChallengeSubmission, ChallengerError>;
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
    /// Returns the number of games created by the factory.
    async fn game_count(&self) -> Result<u64, ChallengerError>;
    /// Returns the WIP-1006 game at a global factory index, or `None` for another game type.
    async fn game_address_at(&self, index: u64) -> Result<Option<Address>, ChallengerError>;
    /// Returns the challenger recorded by the provided game.
    async fn game_challenger(&self, game: Address) -> Result<Address, ChallengerError>;
    /// Returns the amount claimable by the managed challenger.
    async fn claimable(&self, game: Address) -> Result<U256, ChallengerError>;
    /// Withdraws credits for the managed challenger.
    async fn withdraw(&self, game: Address) -> Result<WithdrawSubmission, ChallengerError>;
}
