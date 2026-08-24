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

    /// Regression test for a class of bug where a rollup config hash is computed from the raw
    /// source JSON (e.g. via [`hash_rollup_config`]) instead of from the same
    /// parsed-then-reserialized [`RollupConfig`] the enclave/guest commits to via
    /// [`hash_world_rollup_config`].
    ///
    /// Kona's `RollupConfig` fills in a default for `granite_channel_timeout` (and reorders
    /// fields to its own struct declaration order) even when the source JSON omits that key
    /// entirely — which is exactly what happens for rollup.json files not authored by Kona's own
    /// serializer (e.g. one produced by `op-node` instead of `kona-node`). Hashing the raw JSON
    /// therefore silently diverges from the value the enclave actually computes, causing spurious
    /// "enclave rollup config hash != expected" failures. Callers MUST hash the parsed
    /// [`RollupConfig`] (via [`hash_world_rollup_config`]), never the raw source JSON.
    #[test]
    fn raw_json_hash_diverges_from_parsed_rollup_config_hash_when_fields_are_omitted() {
        // A minimal rollup config JSON, as might be produced by a non-Kona tool (e.g. op-node),
        // that omits `granite_channel_timeout` entirely.
        let raw = serde_json::json!({
            "genesis": {
                "l1": { "hash": format!("0x{}", "11".repeat(32)), "number": 1 },
                "l2": { "hash": format!("0x{}", "22".repeat(32)), "number": 0 },
                "l2_time": 0,
                "system_config": null,
            },
            "block_time": 2,
            "max_sequencer_drift": 600,
            "seq_window_size": 3600,
            "channel_timeout": 300,
            "l1_chain_id": 11155111,
            "l2_chain_id": 5496749,
            "regolith_time": 0,
            "canyon_time": 0,
            "delta_time": 0,
            "ecotone_time": 0,
            "fjord_time": 0,
            "granite_time": 0,
            "holocene_time": 0,
            "isthmus_time": 0,
            "batch_inbox_address": format!("0x{}", "33".repeat(20)),
            "deposit_contract_address": format!("0x{}", "44".repeat(20)),
            "l1_system_config_address": format!("0x{}", "55".repeat(20)),
        });

        let raw_hash = hash_rollup_config(&raw).unwrap();

        let parsed: RollupConfig = serde_json::from_value(raw).unwrap();
        let schedule = WorldRangeHardforkConfig::default();
        let parsed_hash = hash_world_rollup_config(&parsed, &schedule).unwrap();

        // These MUST differ given the missing `granite_channel_timeout` field (Kona fills in a
        // default of 50 when re-serializing `parsed`), demonstrating why any code path computing
        // an "expected" rollup config hash must hash the parsed `RollupConfig`, not the raw JSON.
        assert_ne!(
            raw_hash, parsed_hash,
            "raw-JSON and parsed-RollupConfig hashes were expected to diverge for a JSON \
             document missing granite_channel_timeout — if this now passes, re-check whether \
             kona_genesis::RollupConfig still fills in defaults for fields absent from the \
             source JSON before relying on raw-JSON hashing anywhere in the proving pipeline"
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
