use crate::error::DefenderError;
use alloy_primitives::Address;
use std::time::Duration;

/// Default number of games processed concurrently.
pub const DEFAULT_MAX_GAME_CONCURRENCY: usize = 10;

/// Default maximum number of new factory games discovered per defender tick.
pub const DEFAULT_MAX_GAMES_PER_TICK: u64 = 100;

/// Default number of proving attempts per lane before abandoning it.
pub const DEFAULT_MAX_PROOF_ATTEMPTS: u32 = 3;

/// Configuration for the defender.
#[derive(Debug, Clone)]
pub struct DefenderConfig {
    /// The only proposer whose games this defender is allowed to defend.
    pub allowed_proposer: Address,
    /// Delay between periodic scan attempts.
    pub poll_interval: Duration,
    /// Maximum number of games to process concurrently.
    pub max_game_concurrency: usize,
    /// Maximum number of newly created games discovered during one tick.
    pub max_games_per_tick: u64,
    /// Maximum number of proving attempts per lane before abandoning it.
    pub max_proof_attempts: u32,
}

impl DefenderConfig {
    pub(crate) fn validate(&self) -> Result<(), DefenderError> {
        if self.allowed_proposer.is_zero() {
            return Err(DefenderError::InvalidConfig(
                "allowed_proposer must not be the zero address",
            ));
        }
        if self.poll_interval.is_zero() {
            return Err(DefenderError::InvalidConfig(
                "poll_interval must be greater than zero",
            ));
        }
        if self.max_game_concurrency == 0 {
            return Err(DefenderError::InvalidConfig(
                "max_game_concurrency must be greater than zero",
            ));
        }
        if self.max_games_per_tick == 0 {
            return Err(DefenderError::InvalidConfig(
                "max_games_per_tick must be greater than zero",
            ));
        }
        if self.max_proof_attempts == 0 {
            return Err(DefenderError::InvalidConfig(
                "max_proof_attempts must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl Default for DefenderConfig {
    fn default() -> Self {
        Self {
            allowed_proposer: Address::ZERO,
            poll_interval: Duration::from_mins(1),
            max_game_concurrency: DEFAULT_MAX_GAME_CONCURRENCY,
            max_games_per_tick: DEFAULT_MAX_GAMES_PER_TICK,
            max_proof_attempts: DEFAULT_MAX_PROOF_ATTEMPTS,
        }
    }
}
