use crate::error::ChallengerError;
use alloy_primitives::U256;
use std::time::Duration;

/// Default number of games processed concurrently.
pub const DEFAULT_MAX_GAME_CONCURRENCY: usize = 10;
/// Default maximum number of new factory games discovered per challenger tick.
pub const DEFAULT_MAX_GAMES_PER_TICK: u64 = 100;
/// Default delay between challenger-owned game resolution passes.
pub const DEFAULT_RESOLUTION_POLL_INTERVAL: Duration = Duration::from_secs(30);
/// Default maximum number of resolution transactions submitted per pass.
pub const DEFAULT_MAX_RESOLUTIONS_PER_TICK: usize = 1;
/// Default delay between challenger-bond discovery and withdrawal passes.
pub const DEFAULT_BOND_MANAGER_POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Default number of recent factory games inspected for challenger ownership.
pub const DEFAULT_BOND_MANAGER_INITIAL_SCAN_LIMIT: u64 = 1_000;

/// Configuration for the challenger.
#[derive(Debug, Clone)]
pub struct ChallengerConfig {
    /// Bond read from the factory and sent with `WorldChainProofSystemGame.challenge`.
    pub challenger_bond: U256,
    /// Delay between periodic scan attempts.
    pub poll_interval: Duration,
    /// Maximum number of games to process concurrently.
    pub max_game_concurrency: usize,
    /// Maximum number of newly created games discovered during one tick.
    pub max_games_per_tick: u64,
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
        if self.max_games_per_tick == 0 {
            return Err(ChallengerError::InvalidConfig(
                "max_games_per_tick must be greater than zero",
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
            max_games_per_tick: DEFAULT_MAX_GAMES_PER_TICK,
        }
    }
}

/// Configuration for asynchronous resolution of challenger-owned games.
#[derive(Debug, Clone)]
pub struct ResolutionManagerConfig {
    /// Delay between resolution passes.
    pub poll_interval: Duration,
    /// Maximum number of resolution transactions submitted per pass.
    pub max_resolutions_per_tick: usize,
}

impl ResolutionManagerConfig {
    pub(crate) fn validate(&self) -> Result<(), ChallengerError> {
        if self.poll_interval.is_zero() {
            return Err(ChallengerError::InvalidConfig(
                "resolution_manager.poll_interval must be greater than zero",
            ));
        }
        if self.max_resolutions_per_tick == 0 {
            return Err(ChallengerError::InvalidConfig(
                "resolution_manager.max_resolutions_per_tick must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl Default for ResolutionManagerConfig {
    fn default() -> Self {
        Self {
            poll_interval: DEFAULT_RESOLUTION_POLL_INTERVAL,
            max_resolutions_per_tick: DEFAULT_MAX_RESOLUTIONS_PER_TICK,
        }
    }
}

/// Configuration for asynchronous challenger-bond management.
#[derive(Debug, Clone)]
pub struct BondManagerConfig {
    /// Delay between factory scans and withdrawal attempts.
    pub poll_interval: Duration,
    /// Number of most recently created games scanned on startup.
    pub initial_scan_limit: u64,
}

impl BondManagerConfig {
    pub(crate) fn validate(&self) -> Result<(), ChallengerError> {
        if self.poll_interval.is_zero() {
            return Err(ChallengerError::InvalidConfig(
                "bond_manager.poll_interval must be greater than zero",
            ));
        }
        if self.initial_scan_limit == 0 {
            return Err(ChallengerError::InvalidConfig(
                "bond_manager.initial_scan_limit must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl Default for BondManagerConfig {
    fn default() -> Self {
        Self {
            poll_interval: DEFAULT_BOND_MANAGER_POLL_INTERVAL,
            initial_scan_limit: DEFAULT_BOND_MANAGER_INITIAL_SCAN_LIMIT,
        }
    }
}
