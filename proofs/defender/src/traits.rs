use crate::{
    error::DefenderError,
    types::{DefenderSubmission, GameMetadata, ResolveSubmission},
};
use alloy_primitives::{Address, Bytes};
use async_trait::async_trait;
use world_chain_proofs::ResolutionStatus;

#[async_trait]
pub trait DefenderClient: Send + Sync {
    /// Returns the total number of games indexed by the dispute-game factory, across all
    /// game types.
    async fn game_count(&self) -> Result<u64, DefenderError>;
    /// Returns the WIP-1006 game at the provided factory index, or `None` when that index
    /// holds a game of a different type.
    async fn game_address_at(&self, index: u64) -> Result<Option<Address>, DefenderError>;
    /// Returns the creation timestamp of any game at the provided factory index.
    async fn game_created_at(&self, index: u64) -> Result<u64, DefenderError>;
    /// Reads the immutable game data needed to monitor and defend its root claim.
    async fn game_metadata(&self, game: Address) -> Result<GameMetadata, DefenderError>;
    /// Returns the current resolution evaluation for the provided game.
    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, DefenderError>;
    /// Get the bitmap of proof lanes already proven for the provided game.
    async fn proof_bitmap(&self, game: Address) -> Result<u8, DefenderError>;
    /// Resolves a game whose outcome is already determined.
    async fn resolve_game(&self, game: Address) -> Result<ResolveSubmission, DefenderError>;
    /// Submits a proof to support a proposed or challenged game.
    async fn submit_proof(
        &self,
        game: Address,
        lane: u8,
        proof: Bytes,
    ) -> Result<DefenderSubmission, DefenderError>;
}
