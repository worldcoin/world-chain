use crate::{
    error::DefenderError,
    types::{DefenderSubmission, GameMetadata},
};
use alloy_primitives::{Address, Bytes};
use async_trait::async_trait;
use world_chain_proofs::{ClaimData, LineageProvider};

#[async_trait]
pub trait DefenderClient: LineageProvider {
    /// Reads the immutable game data needed to monitor and defend its root claim.
    async fn game_metadata(&self, game: Address) -> Result<GameMetadata, DefenderError>;
    /// Reads the claim state of the provided game, including its proof bitmap.
    async fn claim_data(&self, game: Address) -> Result<ClaimData, DefenderError>;
    /// Submits a proof to support a proposed or challenged game.
    async fn submit_proof(
        &self,
        game: Address,
        lane: u8,
        proof: Bytes,
    ) -> Result<DefenderSubmission, DefenderError>;
}
