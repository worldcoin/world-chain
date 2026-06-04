use crate::{
    error::ChallengerError,
    types::{ChallengeSubmission, GameCreated, RootState},
};
use alloy_primitives::{Address, BlockNumber, U256};
use async_trait::async_trait;

#[async_trait]
pub trait ChallengerClient {
    /// Reads the root state of the provided game.
    async fn root_state(&self, game: Address) -> RootState;
    /// Get the last finalized L1 block number.
    async fn finalized_l1_block_num(&self) -> BlockNumber;
    /// Get all `GameCreated` events between the provided block numbers.
    async fn games_created(&self, from: BlockNumber, to: BlockNumber) -> Vec<GameCreated>;
    /// Get the challenge deadline of the provided game.
    async fn challenge_deadline(&self, game: Address) -> u64;
    /// Submit a challenge against an invalid game.
    async fn submit_challenge(
        &self,
        game: Address,
        challenger_bond: U256,
    ) -> Result<ChallengeSubmission, ChallengerError>;
}
