use crate::{
    error::DefenderError,
    types::{DefenderSubmission, GameMetadata},
};
use alloy_primitives::{Address, Bytes};
use async_trait::async_trait;
use world_chain_proofs::ResolutionStatus;

#[async_trait]
pub trait DefenderClient: Send + Sync {
    /// Returns the number of games created by the factory.
    async fn game_count(&self) -> Result<u64, DefenderError>;
    /// Returns the game address at a factory creation index.
    async fn game_address_at(&self, index: u64) -> Result<Address, DefenderError>;
    /// Reads the proposer recorded by the provided game.
    async fn game_proposer(&self, game: Address) -> Result<Address, DefenderError>;
    /// Reads the immutable game data needed to monitor and defend its root claim.
    async fn game_metadata(&self, game: Address) -> Result<GameMetadata, DefenderError>;
    /// Reads the proof deadline used by startup cursor initialization.
    async fn proof_deadline(&self, game: Address) -> Result<u64, DefenderError>;
    /// Returns the current resolution evaluation for the provided game.
    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, DefenderError>;
    /// Get the bitmap of proof lanes already proven for the provided game.
    async fn proof_bitmap(&self, game: Address) -> Result<u8, DefenderError>;
    /// Submit a proof to support a challenged game.
    async fn submit_proof(
        &self,
        game: Address,
        lane: u8,
        proof: Bytes,
    ) -> Result<DefenderSubmission, DefenderError>;
}
