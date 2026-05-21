use alloy_chains::{Chain, NamedChain};
use alloy_primitives::{Address, B256, U256, address, b256};

/// Placeholder WIP-1001 activation parameters used while Strato values are being finalized.
///
/// These values are intentionally complete and non-zero so development nodes can exercise the
/// Strato-gated code paths before final production constants exist. Do not treat this as the
/// canonical mainnet or sepolia parameter set.
pub const STRATO_WIP1001_PLACEHOLDER_CONFIG: Wip1001ActivationConfig = Wip1001ActivationConfig {
    world_chain_account_manager: address!("0x4200000000000000000000000000000000000100"),
    world_chain_account_router_code_hash: b256!(
        "0x1111111111111111111111111111111111111111111111111111111111111111"
    ),
    eip1271_validation_gas_limit: 1_000_000,
    execution_trace_validation_gas_limit: 5_000_000,
    block_validation_gas_budget: 20_000_000,
    min_validation_failure_fee: U256::from_limbs([1_000_000_000_000_000, 0, 0, 0]),
    max_execution_trace_bytes: 1_048_576,
    max_payload_data_bytes: 131_072,
    max_access_list_entries: 1_024,
    max_session_signature_bytes: 16_384,
    max_admin_authorization_bytes: 65_536,
    max_verifier_install_data_bytes: 65_536,
};

/// World mainnet WIP-1001 activation parameters.
///
/// Keep this unset until final production values are available. Placeholder values must only be
/// used by generated dev/test chain specs.
pub const STRATO_WIP1001_WORLD_MAINNET_CONFIG: Option<Wip1001ActivationConfig> = None;

/// World Sepolia WIP-1001 activation parameters.
///
/// Keep this unset until final production values are available. Placeholder values must only be
/// used by generated dev/test chain specs.
pub const STRATO_WIP1001_WORLD_SEPOLIA_CONFIG: Option<Wip1001ActivationConfig> = None;

/// Consensus parameters required before WIP-1001 can activate.
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wip1001ActivationConfig {
    /// Predeploy address for the account manager.
    pub world_chain_account_manager: Address,
    /// Runtime code hash required at every World Chain account address.
    pub world_chain_account_router_code_hash: B256,
    /// Fixed gas forwarded to admin and session signature validation.
    pub eip1271_validation_gas_limit: u64,
    /// Fixed gas forwarded to session policy validation.
    pub execution_trace_validation_gas_limit: u64,
    /// Per-block gas budget reserved for WIP-1001 validation calls.
    pub block_validation_gas_budget: u64,
    /// Minimum wei charged to the account on validation failure.
    pub min_validation_failure_fee: U256,
    /// Maximum ABI-encoded trace size passed to a session verifier.
    pub max_execution_trace_bytes: u32,
    /// Maximum length of `data` in a WIP-1001 envelope.
    pub max_payload_data_bytes: u32,
    /// Maximum number of EIP-2930 access-list entries in a WIP-1001 envelope.
    pub max_access_list_entries: u32,
    /// Maximum length of the session signature field.
    pub max_session_signature_bytes: u32,
    /// Maximum length of admin authorization calldata.
    pub max_admin_authorization_bytes: u32,
    /// Maximum length of verifier installation bytes.
    pub max_verifier_install_data_bytes: u32,
}

/// Returns finalized WIP-1001 parameters for production World Chain networks.
pub fn strato_wip1001_config_for_chain(chain: Chain) -> Option<Wip1001ActivationConfig> {
    match chain.named() {
        Some(NamedChain::World) => STRATO_WIP1001_WORLD_MAINNET_CONFIG,
        Some(NamedChain::WorldSepolia) => STRATO_WIP1001_WORLD_SEPOLIA_CONFIG,
        _ => None,
    }
}

impl Wip1001ActivationConfig {
    /// Gas that must be reserved from the per-block validation budget for one WIP-1001 transaction.
    pub fn validation_gas_budget_per_transaction(
        &self,
    ) -> Result<u64, Wip1001ActivationConfigError> {
        self.eip1271_validation_gas_limit
            .checked_add(self.execution_trace_validation_gas_limit)
            .ok_or(Wip1001ActivationConfigError::ValidationGasLimitOverflow)
    }

    /// Returns `Ok(())` if every activation parameter is assigned and internally consistent.
    pub fn validate(&self) -> Result<(), Wip1001ActivationConfigError> {
        if self.world_chain_account_manager.is_zero() {
            return Err(Wip1001ActivationConfigError::ZeroAddress(
                "world_chain_account_manager",
            ));
        }
        if self.world_chain_account_router_code_hash.is_zero() {
            return Err(Wip1001ActivationConfigError::ZeroHash(
                "world_chain_account_router_code_hash",
            ));
        }
        if self.eip1271_validation_gas_limit == 0 {
            return Err(Wip1001ActivationConfigError::ZeroLimit(
                "eip1271_validation_gas_limit",
            ));
        }
        if self.execution_trace_validation_gas_limit == 0 {
            return Err(Wip1001ActivationConfigError::ZeroLimit(
                "execution_trace_validation_gas_limit",
            ));
        }
        if self.block_validation_gas_budget == 0 {
            return Err(Wip1001ActivationConfigError::ZeroLimit(
                "block_validation_gas_budget",
            ));
        }
        let minimum_validation_budget = self.validation_gas_budget_per_transaction()?;
        if self.block_validation_gas_budget < minimum_validation_budget {
            return Err(Wip1001ActivationConfigError::ValidationBudgetTooLow {
                minimum: minimum_validation_budget,
                actual: self.block_validation_gas_budget,
            });
        }
        if self.min_validation_failure_fee.is_zero() {
            return Err(Wip1001ActivationConfigError::ZeroLimit(
                "min_validation_failure_fee",
            ));
        }
        if self.max_execution_trace_bytes == 0 {
            return Err(Wip1001ActivationConfigError::ZeroLimit(
                "max_execution_trace_bytes",
            ));
        }
        if self.max_payload_data_bytes == 0 {
            return Err(Wip1001ActivationConfigError::ZeroLimit(
                "max_payload_data_bytes",
            ));
        }
        if self.max_access_list_entries == 0 {
            return Err(Wip1001ActivationConfigError::ZeroLimit(
                "max_access_list_entries",
            ));
        }
        if self.max_session_signature_bytes == 0 {
            return Err(Wip1001ActivationConfigError::ZeroLimit(
                "max_session_signature_bytes",
            ));
        }
        if self.max_admin_authorization_bytes == 0 {
            return Err(Wip1001ActivationConfigError::ZeroLimit(
                "max_admin_authorization_bytes",
            ));
        }
        if self.max_verifier_install_data_bytes == 0 {
            return Err(Wip1001ActivationConfigError::ZeroLimit(
                "max_verifier_install_data_bytes",
            ));
        }

        Ok(())
    }
}

/// Errors that make a WIP-1001 activation parameter set unsafe to enable.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Wip1001ActivationConfigError {
    /// A required address is zero.
    #[error("{0} must not be the zero address")]
    ZeroAddress(&'static str),
    /// A required hash is zero.
    #[error("{0} must not be the zero hash")]
    ZeroHash(&'static str),
    /// A required numeric limit is zero.
    #[error("{0} must be non-zero")]
    ZeroLimit(&'static str),
    /// The fixed validation gas limits cannot be added safely.
    #[error("eip1271_validation_gas_limit + execution_trace_validation_gas_limit overflows u64")]
    ValidationGasLimitOverflow,
    /// The per-block validation budget cannot fit one full validation attempt.
    #[error("block_validation_gas_budget {actual} is below required validation budget {minimum}")]
    ValidationBudgetTooLow {
        /// Minimum budget required to fit one full validation attempt.
        minimum: u64,
        /// Configured budget.
        actual: u64,
    },
}

/// Errors that make a chain spec unsafe to run with WIP-1001 fork activation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Wip1001ActivationReadinessError {
    /// WIP-1001 activation parameters were configured through an invalid internal path.
    #[error("WIP-1001 activation parameters are incomplete or invalid")]
    InvalidActivationConfig,
    /// Placeholder values must not be enabled on production World Chain networks.
    #[error(
        "placeholder WIP-1001 activation parameters cannot be used for World mainnet or Sepolia"
    )]
    ProductionPlaceholderConfig,
    /// Production World Chain networks must use the built-in parameter set for their chain ID.
    #[error("production World Chain WIP-1001 activation parameters must match built-in constants")]
    ProductionConfigMismatch,
    /// Strato is scheduled, but the WIP-1001 activation parameter set is not assigned.
    #[error("Strato is scheduled but WIP-1001 activation parameters are unset")]
    StratoScheduledWithoutConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_config_is_complete() {
        STRATO_WIP1001_PLACEHOLDER_CONFIG
            .validate()
            .expect("placeholder config must be internally valid");
    }

    #[test]
    fn validation_budget_must_cover_one_validation_attempt() {
        let mut config = STRATO_WIP1001_PLACEHOLDER_CONFIG;
        config.block_validation_gas_budget =
            config.eip1271_validation_gas_limit + config.execution_trace_validation_gas_limit - 1;

        assert_eq!(
            config.validate(),
            Err(Wip1001ActivationConfigError::ValidationBudgetTooLow {
                minimum: config.eip1271_validation_gas_limit
                    + config.execution_trace_validation_gas_limit,
                actual: config.block_validation_gas_budget,
            })
        );
    }

    #[test]
    fn validation_gas_budget_per_transaction_matches_fixed_limits() {
        assert_eq!(
            STRATO_WIP1001_PLACEHOLDER_CONFIG.validation_gas_budget_per_transaction(),
            Ok(
                STRATO_WIP1001_PLACEHOLDER_CONFIG.eip1271_validation_gas_limit
                    + STRATO_WIP1001_PLACEHOLDER_CONFIG.execution_trace_validation_gas_limit
            )
        );
    }

    #[test]
    fn validation_gas_limit_sum_must_not_overflow() {
        let mut config = STRATO_WIP1001_PLACEHOLDER_CONFIG;
        config.eip1271_validation_gas_limit = u64::MAX;
        config.execution_trace_validation_gas_limit = 1;
        config.block_validation_gas_budget = u64::MAX;

        assert_eq!(
            config.validate(),
            Err(Wip1001ActivationConfigError::ValidationGasLimitOverflow)
        );
        assert_eq!(
            config.validation_gas_budget_per_transaction(),
            Err(Wip1001ActivationConfigError::ValidationGasLimitOverflow)
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_round_trip_preserves_activation_config_shape() {
        let json = serde_json::to_value(STRATO_WIP1001_PLACEHOLDER_CONFIG)
            .expect("activation config serializes");
        let object = json.as_object().expect("config serializes as an object");

        for field in [
            "world_chain_account_manager",
            "world_chain_account_router_code_hash",
            "eip1271_validation_gas_limit",
            "execution_trace_validation_gas_limit",
            "block_validation_gas_budget",
            "min_validation_failure_fee",
            "max_execution_trace_bytes",
            "max_payload_data_bytes",
            "max_access_list_entries",
            "max_session_signature_bytes",
            "max_admin_authorization_bytes",
            "max_verifier_install_data_bytes",
        ] {
            assert!(
                object.contains_key(field),
                "missing serialized field {field}"
            );
        }

        let decoded: Wip1001ActivationConfig =
            serde_json::from_value(json).expect("activation config deserializes");
        assert_eq!(decoded, STRATO_WIP1001_PLACEHOLDER_CONFIG);
    }
}
