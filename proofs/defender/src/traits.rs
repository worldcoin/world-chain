use crate::{error::DefenderError, types::DefenderSubmission};
use alloy_primitives::{Address, BlockNumber, Bytes};
use async_trait::async_trait;
use world_chain_proofs::{GameCreated, RootState};

#[async_trait]
pub trait DefenderClient {
    /// Reads the root state of the provided game.
    async fn root_state(&self, game: Address) -> Result<RootState, DefenderError>;
    /// Get the last finalized L1 block number.
    async fn finalized_l1_block_num(&self) -> Result<BlockNumber, DefenderError>;
    /// Get all `GameCreated` events between the provided block numbers.
    async fn games_created(
        &self,
        from: BlockNumber,
        to: BlockNumber,
    ) -> Result<Vec<GameCreated>, DefenderError>;
    /// Get the challenge deadline of the provided game.
    async fn challenge_deadline(&self, game: Address) -> Result<u64, DefenderError>;
    /// Submit a proof to support a challenged game.
    async fn submit_proof(
        &self,
        game: Address,
        lane: u8,
        proof: Bytes,
    ) -> Result<DefenderSubmission, DefenderError>;
}
