use crate::{
    error::DefenderError,
    types::{DefenderSubmission, GameMetadata},
};
use alloy_primitives::{Address, Bytes};
use async_trait::async_trait;
use world_chain_proof_protocol::{ClaimData, LineageProvider, ProofLane};

#[async_trait]
pub trait DefenderClient: LineageProvider {
    /// Reads the immutable game data needed to monitor and defend its root claim.
    async fn game_metadata(&self, game: Address) -> Result<GameMetadata, DefenderError>;
    /// Reads the proposal's mutable state: challenge status and the accepted proof lanes.
    async fn claim_data(&self, game: Address) -> Result<ClaimData, DefenderError>;
    /// Submits a lane's verifier proof to support a proposed or challenged game.
    ///
    /// `proof` is the verifier payload; the compact lane header is added by the implementation,
    /// which owns the reward recipient.
    async fn submit_proof(
        &self,
        game: Address,
        lane: ProofLane,
        proof: Bytes,
    ) -> Result<DefenderSubmission, DefenderError>;
}
