use crate::{
    error::DefenderError,
    types::{DefenderSubmission, GameMetadata},
};
use alloy_primitives::{Address, Bytes};
use async_trait::async_trait;
use world_chain_proofs::LineageProvider;

#[async_trait]
pub trait DefenderClient: LineageProvider {
    /// Reads the immutable game data needed to monitor and defend its root claim.
    async fn game_metadata(&self, game: Address) -> Result<GameMetadata, DefenderError>;
    /// Get the bitmap of proof lanes already proven for the provided game.
    async fn proof_bitmap(&self, game: Address) -> Result<u8, DefenderError>;
    /// Submits a proof to support a proposed or challenged game.
    async fn submit_proof(
        &self,
        game: Address,
        lane: u8,
        proof: Bytes,
    ) -> Result<DefenderSubmission, DefenderError>;
}
