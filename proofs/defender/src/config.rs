use crate::error::DefenderError;
use std::{path::PathBuf, time::Duration};

/// Default number of games processed concurrently.
pub const DEFAULT_MAX_GAME_CONCURRENCY: usize = 10;

/// Default ceiling on one tick. Generous: a tick fans out over every active game, so this is
/// a stuck-detector, not a latency budget.
pub const DEFAULT_TICK_TIMEOUT: Duration = Duration::from_secs(300);

/// Default number of L1 confirmations required for defender transactions.
pub const DEFAULT_L1_TX_CONFIRMATIONS: u64 = 5;

/// Configuration for the defender.
#[derive(Debug, Clone)]
pub struct DefenderConfig {
    /// Delay between periodic scan attempts.
    pub poll_interval: Duration,
    /// Maximum number of games to process concurrently.
    pub max_game_concurrency: usize,
    /// Ceiling on one tick. A tick that blocks forever — an RPC call with no timeout of its
    /// own — stops the loop without stopping the process: it stays Ready, logs nothing, and
    /// submits nothing until someone restarts the pod. Observed twice on alphanet
    /// 2026-08-05, both times immediately after a failed submission.
    pub tick_timeout: Duration,
    /// File touched after every tick. Lets a liveness probe distinguish "loop running" from
    /// "process alive", which is otherwise invisible from outside.
    pub heartbeat_file: Option<PathBuf>,
}

impl DefenderConfig {
    pub(crate) fn validate(&self) -> Result<(), DefenderError> {
        if self.poll_interval.is_zero() {
            return Err(DefenderError::InvalidConfig(
                "poll_interval must be greater than zero",
            ));
        }
        if self.tick_timeout.is_zero() {
            return Err(DefenderError::InvalidConfig(
                "tick_timeout must be greater than zero",
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
            tick_timeout: DEFAULT_TICK_TIMEOUT,
            heartbeat_file: None,
        }
    }
}
