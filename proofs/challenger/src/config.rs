use crate::error::ChallengerError;
use alloy_primitives::{Address, U256};
use std::time::Duration;

/// Configuration for the challenger.
#[derive(Debug, Clone)]
pub struct ChallengerConfig {
    /// Bond sent with `WorldChainProofSystemGame.challenge`.
    pub challenger_bond: U256,
    /// The `WorldChainProofSystemFactory` contract address.
    pub factory_contract: Address,
    /// Delay between periodic scan attempts.
    pub poll_interval: Duration,
}

impl ChallengerConfig {
    pub(crate) fn validate(&self) -> Result<(), ChallengerError> {
        if self.factory_contract.is_zero() {
            return Err(ChallengerError::InvalidConfig(
                "factory contract cannot be zero",
            ));
        }
        if self.poll_interval.is_zero() {
            return Err(ChallengerError::InvalidConfig(
                "poll_interval must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl Default for ChallengerConfig {
    fn default() -> Self {
        Self {
            challenger_bond: U256::ZERO,
            factory_contract: Address::random(),
            poll_interval: Duration::from_mins(1),
        }
    }
}
