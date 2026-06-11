use crate::error::DefenderError;
use std::time::Duration;

/// Default number of games processed concurrently.
pub const DEFAULT_MAX_GAME_CONCURRENCY: usize = 10;

/// Configuration for the defender.
#[derive(Debug, Clone)]
pub struct DefenderConfig {
    /// Delay between periodic scan attempts.
    pub poll_interval: Duration,
    /// Maximum number of games to process concurrently.
    pub max_game_concurrency: usize,
}

impl DefenderConfig {
    pub(crate) fn validate(&self) -> Result<(), DefenderError> {
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
        Ok(())
    }
}

impl Default for DefenderConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_mins(1),
            max_game_concurrency: DEFAULT_MAX_GAME_CONCURRENCY,
        }
    }
}
