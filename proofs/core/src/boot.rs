//! Public boot value ABI used by range proofs and the aggregation program.

use alloy_primitives::{B256, BlockNumber};
use alloy_sol_types::sol;
use kona_genesis::RollupConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::range::WorldRangeHardforkConfig;

/// Error returned when a rollup config cannot be serialized for hashing.
#[derive(Debug, thiserror::Error)]
pub enum RollupConfigHashError {
    #[error("failed to serialize rollup config for hashing: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Hashes a rollup config as pretty JSON then SHA-256, matching OP Succinct Lite.
pub fn hash_rollup_config<T: Serialize + ?Sized>(
    config: &T,
) -> Result<B256, RollupConfigHashError> {
    Ok(sha256_b256(
        serde_json::to_string_pretty(config)?.as_bytes(),
    ))
}

sol! {
    /// Range proof public values.
    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct TransitionPublicValues {
        bytes32 l1Head;

        bytes32 l2PreRoot;
        uint64 l2PreBlockNumber;

        bytes32 l2PostRoot;
        uint64 l2PostBlockNumber;

        bytes32 rollupConfigHash;
    }
}

impl TransitionPublicValues {
    /// Converts Kona boot info into the on-chain public values.
    ///
    /// The rollup config hash is computed from Kona's rollup config plus the
    /// World-only Tropo/Strato schedule fields used during execution. Returns an
    /// error if the rollup config cannot be serialized for hashing.
    pub fn try_from_kona_boot_info(
        boot_info: kona_proof::BootInfo,
        world_schedule: &WorldRangeHardforkConfig,
        l2_pre_block_number: BlockNumber,
    ) -> Result<Self, RollupConfigHashError> {
        let rollup_config_hash =
            hash_world_rollup_config(&boot_info.rollup_config, world_schedule)?;
        Ok(Self {
            l1Head: boot_info.l1_head,
            l2PreRoot: boot_info.agreed_l2_output_root,
            l2PreBlockNumber: l2_pre_block_number,
            l2PostRoot: boot_info.claimed_l2_output_root,
            l2PostBlockNumber: boot_info.claimed_l2_block_number,
            rollupConfigHash: rollup_config_hash,
        })
    }
}

#[derive(Serialize)]
struct WorldRollupConfigHashInput<'a, T: Serialize + ?Sized> {
    #[serde(flatten)]
    rollup_config: &'a T,
    #[serde(skip_serializing_if = "Option::is_none")]
    tropo_time: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    strato_time: Option<u64>,
}

/// Hashes the rollup config committed by World range proofs.
///
/// This mirrors OP Succinct's pretty-JSON-then-SHA256 hash, but appends World-only fork fields
/// that are not represented in upstream Kona's `RollupConfig`. Delegates to
/// [`hash_world_rollup_config_generic`] and propagates serialization errors instead of
/// panicking on malformed input.
///
/// # Cross-chain replay resistance
///
/// Kona's `RollupConfig` contains both `l1_chain_id` and `l2_chain_id` (see
/// `kona-genesis::RollupConfig`), and both are part of the serde-serialized JSON
/// blob hashed here. The resulting `rollupConfigHash` is therefore an implicit
/// domain separator: a Nitro signature whose payload commits to this hash
/// cannot be replayed on a different chain id. The on-chain
/// `NitroProofVerifier` commitment over `TransitionPublicValues` inherits the
/// same property without needing an explicit `chainId` field.
pub fn hash_world_rollup_config(
    rollup_config: &RollupConfig,
    world_schedule: &WorldRangeHardforkConfig,
) -> Result<B256, RollupConfigHashError> {
    hash_world_rollup_config_generic(rollup_config, world_schedule)
}

/// Generic, fallible variant of [`hash_world_rollup_config`] for use with arbitrary config types.
pub fn hash_world_rollup_config_generic<T: Serialize + ?Sized>(
    rollup_config: &T,
    world_schedule: &WorldRangeHardforkConfig,
) -> Result<B256, RollupConfigHashError> {
    let serialized = serde_json::to_string_pretty(&WorldRollupConfigHashInput {
        rollup_config,
        tropo_time: world_schedule.tropo_time,
        strato_time: world_schedule.strato_time,
    })?;
    Ok(sha256_b256(serialized.as_bytes()))
}

fn sha256_b256(bytes: &[u8]) -> B256 {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    B256::from_slice(hasher.finalize().as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kona_genesis::RollupConfig;

    #[test]
    fn world_rollup_hash_changes_when_world_fork_schedule_changes() {
        let rollup_config = RollupConfig::default();
        let before = WorldRangeHardforkConfig {
            tropo_time: Some(20),
            strato_time: Some(30),
            ..Default::default()
        };
        let after = WorldRangeHardforkConfig {
            tropo_time: Some(21),
            strato_time: Some(30),
            ..Default::default()
        };
        assert_ne!(
            hash_world_rollup_config(&rollup_config, &before).unwrap(),
            hash_world_rollup_config(&rollup_config, &after).unwrap()
        );
    }

    #[test]
    fn world_rollup_hash_matches_op_hash_without_world_forks() {
        let rollup_config = RollupConfig::default();
        let serialized_config = serde_json::to_string_pretty(&rollup_config).unwrap();
        assert_eq!(
            hash_world_rollup_config(&rollup_config, &WorldRangeHardforkConfig::default()).unwrap(),
            sha256_b256(serialized_config.as_bytes())
        );
    }
}
