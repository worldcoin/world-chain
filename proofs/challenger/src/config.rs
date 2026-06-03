use crate::error::ChallengerError;
use alloy_primitives::{Address, U256};

/// Configuration for the challenger.
#[derive(Debug, Clone)]
pub struct ChallengerConfig {
    /// Bond sent with `WorldChainProofSystemGame.challenge`.
    pub challenger_bond: U256,
    /// The `WorldChainProofSystemFactory` contract address.
    pub factory_contract: Address,
}

impl ChallengerConfig {
    pub(crate) fn validate(&self) -> Result<(), ChallengerError> {
        if self.factory_contract.is_zero() {
            return Err(ChallengerError::InvalidConfig(
                "factory contract cannot be zero",
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
        }
    }
}
