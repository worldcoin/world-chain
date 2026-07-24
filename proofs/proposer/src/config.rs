use std::time::Duration;

use alloy_primitives::U256;

use crate::ProposerError;

/// Default delay between bond-manager scans.
pub const DEFAULT_BOND_MANAGER_POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Default number of recent games inspected when the bond manager starts.
pub const DEFAULT_BOND_MANAGER_INITIAL_SCAN_LIMIT: u64 = 1_000;

/// Configuration for asynchronous proposer-bond management.
#[derive(Debug, Clone)]
pub struct BondManagerConfig {
    /// Delay between factory scans and withdrawal attempts.
    pub poll_interval: Duration,
    /// Number of most recently created games scanned on startup.
    pub initial_scan_limit: u64,
}

impl BondManagerConfig {
    pub(crate) fn validate(&self) -> Result<(), ProposerError> {
        if self.poll_interval.is_zero() {
            return Err(ProposerError::InvalidConfig(
                "bond_manager.poll_interval must be greater than zero",
            ));
        }
        if self.initial_scan_limit == 0 {
            return Err(ProposerError::InvalidConfig(
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

/// Configuration for the proposer loop.
#[derive(Debug, Clone)]
pub struct ProposerConfig {
    /// Number of L2 blocks between proposals.
    pub block_interval: u64,
    /// Bond sent with `DisputeGameFactory.create` for game type 1006.
    pub proposer_bond: U256,
    /// Delay between periodic proposal attempts.
    pub poll_interval: Duration,
    /// Maximum number of game-resolution transactions submitted per proposer tick.
    pub max_resolutions_per_tick: usize,
}

impl ProposerConfig {
    pub(crate) fn validate(&self) -> Result<(), ProposerError> {
        if self.block_interval == 0 {
            return Err(ProposerError::InvalidConfig(
                "block_interval must be greater than zero",
            ));
        }
        if self.poll_interval.is_zero() {
            return Err(ProposerError::InvalidConfig(
                "poll_interval must be greater than zero",
            ));
        }
        if self.max_resolutions_per_tick == 0 {
            return Err(ProposerError::InvalidConfig(
                "max_resolutions_per_tick must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl Default for ProposerConfig {
    fn default() -> Self {
        Self {
            block_interval: 6,
            proposer_bond: U256::ZERO,
            poll_interval: Duration::from_secs(12),
            max_resolutions_per_tick: 1,
        }
    }
}
