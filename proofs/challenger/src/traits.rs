use crate::{
    error::ChallengerError,
    types::{ChallengeSubmission, Game, RootState},
};
use alloy_primitives::{Address, U256};
use async_trait::async_trait;

#[async_trait]
pub trait GameWatcher {
    /// Reads the root state of the provided game.
    fn root_state(&self, game: Address) -> RootState;
}

#[async_trait]
pub trait ChallengeSubmitter {
    fn submit_challenge(
        &self,
        game: Game,
        challenger_bond: U256,
    ) -> Result<ChallengeSubmission, ChallengerError>;
}
