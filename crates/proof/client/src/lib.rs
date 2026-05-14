//! Client-side public-value helpers for World OP Succinct Lite proofs.
//!
//! The full SP1 range program should use the same execution path as OP Succinct/Base. This crate
//! contains the World-specific public-value construction and validation that wraps that execution:
//! the active World proof spec and the rollup config hash that binds Tropo/Strato.

use alloy_primitives::B256;
use serde::{Deserialize, Serialize};
use world_chain_proof_protocol::{
    BootInfoPublicValues, RollupConfigHashError, WorldHardforkConfig, WorldSpecId,
    hash_rollup_config,
};

/// Claimed transition proven by the World range program.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldProofClaim {
    /// L1 head used for the derivation pipeline.
    pub l1_head: B256,
    /// Agreed pre-state output root.
    pub agreed_l2_output_root: B256,
    /// Claimed post-state output root.
    pub claimed_l2_output_root: B256,
    /// Claimed post-state L2 block number.
    pub claimed_l2_block_number: u64,
}

impl WorldProofClaim {
    /// Converts the claim into OP Succinct-compatible public boot values.
    pub const fn boot_info(self, rollup_config_hash: B256) -> BootInfoPublicValues {
        BootInfoPublicValues::new(
            self.l1_head,
            self.agreed_l2_output_root,
            self.claimed_l2_output_root,
            self.claimed_l2_block_number,
            rollup_config_hash,
        )
    }
}

/// Input needed to build World public values after executing the range proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldProofInput {
    /// World hardfork schedule extracted from the rollup config.
    pub schedule: WorldHardforkConfig,
    /// Claimed transition.
    pub claim: WorldProofClaim,
    /// Timestamp of the claimed post-state L2 block.
    pub claimed_l2_timestamp: u64,
    /// Hash of the full rollup config, computed with OP Succinct's hashing method.
    pub rollup_config_hash: B256,
}

impl WorldProofInput {
    /// Builds proof input from a full rollup config object.
    pub fn from_rollup_config<T: Serialize + ?Sized>(
        schedule: WorldHardforkConfig,
        claim: WorldProofClaim,
        claimed_l2_timestamp: u64,
        rollup_config: &T,
    ) -> Result<Self, RollupConfigHashError> {
        Ok(Self {
            schedule,
            claim,
            claimed_l2_timestamp,
            rollup_config_hash: hash_rollup_config(rollup_config)?,
        })
    }

    /// Builds the public values expected from the range proof.
    pub fn public_values(self) -> WorldProofPublicValues {
        let active_fork = self.schedule.active_fork_at(
            self.claim.claimed_l2_block_number,
            self.claimed_l2_timestamp,
        );
        let world_spec_id = WorldSpecId::from_hardfork(active_fork);
        WorldProofPublicValues {
            boot_info: self.claim.boot_info(self.rollup_config_hash),
            active_fork,
            world_spec_id,
        }
    }
}

/// Public values emitted by the World range proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldProofPublicValues {
    /// Public values that match OP Succinct's fault-proof contract inputs.
    pub boot_info: BootInfoPublicValues,
    /// Latest active World hardfork at the claimed L2 block.
    pub active_fork: world_chain_proof_protocol::WorldChainHardfork,
    /// EVM spec id used by the proof.
    pub world_spec_id: WorldSpecId,
}

/// Validation error for mismatched proof public values.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorldProofValidationError {
    /// L1 head does not match.
    #[error("l1 head mismatch: expected {expected:?}, got {actual:?}")]
    L1Head { expected: B256, actual: B256 },
    /// Pre-state output root does not match.
    #[error("pre-state output root mismatch: expected {expected:?}, got {actual:?}")]
    L2PreRoot { expected: B256, actual: B256 },
    /// Post-state output root does not match.
    #[error("post-state output root mismatch: expected {expected:?}, got {actual:?}")]
    L2PostRoot { expected: B256, actual: B256 },
    /// Claimed L2 block number does not match.
    #[error("l2 block number mismatch: expected {expected}, got {actual}")]
    L2BlockNumber { expected: u64, actual: u64 },
    /// Rollup config hash does not match.
    #[error("rollup config hash mismatch: expected {expected:?}, got {actual:?}")]
    RollupConfigHash { expected: B256, actual: B256 },
    /// Active World fork does not match.
    #[error("active fork mismatch: expected {expected:?}, got {actual:?}")]
    ActiveFork {
        expected: world_chain_proof_protocol::WorldChainHardfork,
        actual: world_chain_proof_protocol::WorldChainHardfork,
    },
    /// Active World proof spec does not match.
    #[error("world spec id mismatch: expected {expected:?}, got {actual:?}")]
    WorldSpecId {
        expected: WorldSpecId,
        actual: WorldSpecId,
    },
}

/// Validates actual public values against expected public values.
pub fn validate_public_values(
    expected: &WorldProofPublicValues,
    actual: &WorldProofPublicValues,
) -> Result<(), WorldProofValidationError> {
    if expected.boot_info.l1_head != actual.boot_info.l1_head {
        return Err(WorldProofValidationError::L1Head {
            expected: expected.boot_info.l1_head,
            actual: actual.boot_info.l1_head,
        });
    }
    if expected.boot_info.l2_pre_root != actual.boot_info.l2_pre_root {
        return Err(WorldProofValidationError::L2PreRoot {
            expected: expected.boot_info.l2_pre_root,
            actual: actual.boot_info.l2_pre_root,
        });
    }
    if expected.boot_info.l2_post_root != actual.boot_info.l2_post_root {
        return Err(WorldProofValidationError::L2PostRoot {
            expected: expected.boot_info.l2_post_root,
            actual: actual.boot_info.l2_post_root,
        });
    }
    if expected.boot_info.l2_block_number != actual.boot_info.l2_block_number {
        return Err(WorldProofValidationError::L2BlockNumber {
            expected: expected.boot_info.l2_block_number,
            actual: actual.boot_info.l2_block_number,
        });
    }
    if expected.boot_info.rollup_config_hash != actual.boot_info.rollup_config_hash {
        return Err(WorldProofValidationError::RollupConfigHash {
            expected: expected.boot_info.rollup_config_hash,
            actual: actual.boot_info.rollup_config_hash,
        });
    }
    if expected.active_fork != actual.active_fork {
        return Err(WorldProofValidationError::ActiveFork {
            expected: expected.active_fork,
            actual: actual.active_fork,
        });
    }
    if expected.world_spec_id != actual.world_spec_id {
        return Err(WorldProofValidationError::WorldSpecId {
            expected: expected.world_spec_id,
            actual: actual.world_spec_id,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use world_chain_proof_protocol::{WorldChainHardfork, hash_rollup_config};

    fn claim() -> WorldProofClaim {
        WorldProofClaim {
            l1_head: B256::from([1; 32]),
            agreed_l2_output_root: B256::from([2; 32]),
            claimed_l2_output_root: B256::from([3; 32]),
            claimed_l2_block_number: 42,
        }
    }

    #[test]
    fn public_values_use_world_active_fork() {
        let schedule = WorldHardforkConfig {
            jovian_time: Some(10),
            tropo_time: Some(20),
            strato_time: Some(30),
            ..Default::default()
        };
        let rollup_config = json!({
            "jovian_time": 10,
            "tropo_time": 20,
            "strato_time": 30
        });

        let values = WorldProofInput::from_rollup_config(schedule, claim(), 30, &rollup_config)
            .unwrap()
            .public_values();

        assert_eq!(values.active_fork, WorldChainHardfork::Strato);
        assert_eq!(values.world_spec_id, WorldSpecId::STRATO);
        assert_eq!(
            values.boot_info.rollup_config_hash,
            hash_rollup_config(&rollup_config).unwrap()
        );
    }

    #[test]
    fn validates_matching_public_values() {
        let boot_info = BootInfoPublicValues::new(
            B256::from([1; 32]),
            B256::from([2; 32]),
            B256::from([3; 32]),
            42,
            B256::from([4; 32]),
        );
        let expected = WorldProofPublicValues {
            boot_info,
            active_fork: WorldChainHardfork::Tropo,
            world_spec_id: WorldSpecId::TROPO,
        };

        assert_eq!(validate_public_values(&expected, &expected), Ok(()));
    }

    #[test]
    fn rejects_rollup_config_hash_mismatch() {
        let mut expected = WorldProofPublicValues {
            boot_info: BootInfoPublicValues::new(
                B256::from([1; 32]),
                B256::from([2; 32]),
                B256::from([3; 32]),
                42,
                B256::from([4; 32]),
            ),
            active_fork: WorldChainHardfork::Tropo,
            world_spec_id: WorldSpecId::TROPO,
        };
        let actual = expected.clone();
        expected.boot_info.rollup_config_hash = B256::from([9; 32]);

        assert!(matches!(
            validate_public_values(&expected, &actual),
            Err(WorldProofValidationError::RollupConfigHash { .. })
        ));
    }
}
