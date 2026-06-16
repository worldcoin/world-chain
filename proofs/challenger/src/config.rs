use crate::error::ChallengerError;
use alloy_primitives::U256;
use std::time::Duration;

/// Default number of games processed concurrently.
pub const DEFAULT_MAX_GAME_CONCURRENCY: usize = 10;

/// Configuration for the challenger.
#[derive(Debug, Clone)]
pub struct ChallengerConfig {
    /// Bond sent with `WorldChainProofSystemGame.challenge`.
    pub challenger_bond: U256,
    /// Delay between periodic scan attempts.
    pub poll_interval: Duration,
    /// Maximum number of games to process concurrently.
    pub max_game_concurrency: usize,
}

impl ChallengerConfig {
    pub(crate) fn validate(&self) -> Result<(), ChallengerError> {
        if self.poll_interval.is_zero() {
            return Err(ChallengerError::InvalidConfig(
                "poll_interval must be greater than zero",
            ));
        }
        if self.max_game_concurrency == 0 {
            return Err(ChallengerError::InvalidConfig(
                "max_game_concurrency must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl Default for ChallengerConfig {
    fn default() -> Self {
        Self {
            challenger_bond: U256::ZERO,
            poll_interval: Duration::from_mins(1),
            max_game_concurrency: DEFAULT_MAX_GAME_CONCURRENCY,
        }
    }
}
