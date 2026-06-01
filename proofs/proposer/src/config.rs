use std::time::Duration;

use alloy_primitives::U256;

use crate::ProposerError;

/// Configuration for the proposer loop.
#[derive(Debug, Clone)]
pub struct ProposerConfig {
    /// Number of L2 blocks between proposals.
    pub block_interval: u64,
    /// Bond sent with `WorldChainProofSystemFactory.propose`.
    pub proposer_bond: U256,
    /// Delay between periodic proposal attempts.
    pub poll_interval: Duration,
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
        Ok(())
    }
}

impl Default for ProposerConfig {
    fn default() -> Self {
        Self {
            block_interval: 0,
            proposer_bond: U256::ZERO,
            poll_interval: Duration::from_secs(12),
        }
    }
}
