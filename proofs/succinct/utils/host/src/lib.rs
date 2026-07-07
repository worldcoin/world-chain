//! Host-side helpers for preparing World Chain OP Succinct Lite proof requests.

use alloy_primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};
use world_chain_proof_core::{
    RollupConfigHashError,
    boot::{BootInfoStruct, hash_world_rollup_config_generic},
    hash_rollup_config,
    range::{WorldRangeHardforkConfig, WorldRangeProofPublicValues},
    types::AggregationInputs,
    witness::WorldRangeWitnessData,
};
use world_chain_proof_succinct_utils::{AggregationProofRequest, RangeProofRequest};

#[cfg(feature = "sp1")]
pub mod cpu_prover;
#[cfg(feature = "sp1")]
pub mod mock_prover;
#[cfg(feature = "sp1")]
pub mod network_prover;

/// Error returned while constructing host-side proof config.
#[derive(Debug, thiserror::Error)]
pub enum WorldSuccinctHostError {
    #[error("range end block {end} must be greater than start block {start}")]
    InvalidRange { start: u64, end: u64 },
    #[error("rollup config hash mismatch: expected {expected:?}, got {actual:?}")]
    RollupConfigHashMismatch { expected: B256, actual: B256 },
    #[error(transparent)]
    RollupConfigHash(#[from] RollupConfigHashError),
    #[error("range split count must be one of 1, 2, 4, 8, or 16, got {0}")]
    InvalidRangeSplitCount(u8),
    #[error("failed to rkyv-serialize range witness: {0}")]
    WitnessSerialization(#[from] rkyv::rancor::Error),
}

/// Number of independent range proofs before aggregation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
#[repr(u8)]
pub enum RangeSplitCount {
    One = 1,
    Two = 2,
    Four = 4,
    Eight = 8,
    Sixteen = 16,
}

impl RangeSplitCount {
    pub const fn get(self) -> u8 {
        self as u8
    }
}

impl From<RangeSplitCount> for u8 {
    fn from(value: RangeSplitCount) -> Self {
        value.get()
    }
}

impl TryFrom<u8> for RangeSplitCount {
    type Error = WorldSuccinctHostError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::One),
            2 => Ok(Self::Two),
            4 => Ok(Self::Four),
            8 => Ok(Self::Eight),
            16 => Ok(Self::Sixteen),
            other => Err(WorldSuccinctHostError::InvalidRangeSplitCount(other)),
        }
    }
}

/// Version tag for the World OP Succinct Lite prover identity.
pub const WORLD_OP_SUCCINCT_LITE_VERSION: &str = "world-op-succinct-lite-v1";

/// Prover/game identity checked before participating in a fault dispute game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldProverIdentity {
    pub aggregation_vkey: B256,
    pub range_vkey_commitment: B256,
    pub rollup_config_hash: B256,
}

impl WorldProverIdentity {
    pub const fn new(
        aggregation_vkey: B256,
        range_vkey_commitment: B256,
        rollup_config_hash: B256,
    ) -> Self {
        Self {
            aggregation_vkey,
            range_vkey_commitment,
            rollup_config_hash,
        }
    }

    pub fn from_rollup_config<T: Serialize + ?Sized>(
        aggregation_vkey: B256,
        range_vkey_commitment: B256,
        rollup_config: &T,
    ) -> Result<Self, RollupConfigHashError> {
        Ok(Self::new(
            aggregation_vkey,
            range_vkey_commitment,
            hash_rollup_config(rollup_config)?,
        ))
    }

    pub fn from_world_rollup_config<T: Serialize + ?Sized>(
        aggregation_vkey: B256,
        range_vkey_commitment: B256,
        rollup_config: &T,
        schedule: &WorldRangeHardforkConfig,
    ) -> Result<Self, RollupConfigHashError> {
        Ok(Self::new(
            aggregation_vkey,
            range_vkey_commitment,
            hash_world_rollup_config_generic(rollup_config, schedule)?,
        ))
    }

    pub fn matches_game(self, game: &WorldFaultDisputeGameConfig) -> bool {
        self == game.identity()
    }
}

/// Fault-dispute-game config consumed by World proposer/challenger services.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldFaultDisputeGameConfig {
    pub aggregation_vkey: B256,
    pub range_vkey_commitment: B256,
    pub rollup_config_hash: B256,
    pub challenger_addresses: Vec<Address>,
    pub challenger_bond_wei: U256,
    pub dispute_game_finality_delay_seconds: u64,
    pub existing_anchor_state_registry: Option<Address>,
    pub existing_dispute_game_factory_proxy: Option<Address>,
    pub fallback_timeout_fp_secs: u64,
    pub game_type: u32,
    pub initial_bond_wei: U256,
    pub max_challenge_duration: u64,
    pub max_prove_duration: u64,
    pub optimism_portal2_address: Address,
    pub permissionless_mode: bool,
    pub proposer_addresses: Vec<Address>,
    pub starting_l2_block_number: u64,
    pub starting_root: B256,
    pub system_config_address: Address,
    pub use_sp1_mock_verifier: bool,
    pub verifier_address: Address,
}

impl WorldFaultDisputeGameConfig {
    pub const fn identity(&self) -> WorldProverIdentity {
        WorldProverIdentity::new(
            self.aggregation_vkey,
            self.range_vkey_commitment,
            self.rollup_config_hash,
        )
    }

    pub fn with_rollup_config<T: Serialize + ?Sized>(
        mut self,
        rollup_config: &T,
    ) -> Result<Self, RollupConfigHashError> {
        self.rollup_config_hash = hash_rollup_config(rollup_config)?;
        Ok(self)
    }

    pub fn with_world_rollup_config<T: Serialize + ?Sized>(
        mut self,
        rollup_config: &T,
        schedule: &WorldRangeHardforkConfig,
    ) -> Result<Self, RollupConfigHashError> {
        self.rollup_config_hash = hash_world_rollup_config_generic(rollup_config, schedule)?;
        Ok(self)
    }
}

/// Runtime config for the World fault-proof host service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldFaultProofServiceConfig {
    pub l1_rpc_url: String,
    pub l1_beacon_rpc_url: String,
    pub l2_rpc_url: String,
    pub safe_db_fallback: bool,
    pub range_split_count: RangeSplitCount,
    pub fault_dispute_game: WorldFaultDisputeGameConfig,
}

impl WorldFaultProofServiceConfig {
    pub fn new(
        l1_rpc_url: String,
        l1_beacon_rpc_url: String,
        l2_rpc_url: String,
        safe_db_fallback: bool,
        range_split_count: u8,
        fault_dispute_game: WorldFaultDisputeGameConfig,
    ) -> Result<Self, WorldSuccinctHostError> {
        Ok(Self {
            l1_rpc_url,
            l1_beacon_rpc_url,
            l2_rpc_url,
            safe_db_fallback,
            range_split_count: range_split_count.try_into()?,
            fault_dispute_game,
        })
    }

    pub const fn identity(&self) -> WorldProverIdentity {
        self.fault_dispute_game.identity()
    }
}

/// Half-open L2 block range `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct L2BlockRange {
    pub start: u64,
    pub end: u64,
}

impl L2BlockRange {
    pub const fn new(start: u64, end: u64) -> Result<Self, WorldSuccinctHostError> {
        if end <= start {
            return Err(WorldSuccinctHostError::InvalidRange { start, end });
        }
        Ok(Self { start, end })
    }

    pub const fn len(self) -> u64 {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        self.end <= self.start
    }
}

/// World Succinct host request builder.
#[derive(Clone, Debug)]
pub struct WorldSuccinctHost {
    pub config: WorldFaultProofServiceConfig,
}

impl WorldSuccinctHost {
    pub const fn new(config: WorldFaultProofServiceConfig) -> Self {
        Self { config }
    }

    pub fn range_request(
        &self,
        witness: &WorldRangeWitnessData,
        expected_public_values: Option<WorldRangeProofPublicValues>,
    ) -> Result<RangeProofRequest, WorldSuccinctHostError> {
        if let Some(expected) = &expected_public_values {
            let expected_hash = self.config.identity().rollup_config_hash;
            if expected.boot_info.rollup_config_hash != expected_hash {
                return Err(WorldSuccinctHostError::RollupConfigHashMismatch {
                    expected: expected_hash,
                    actual: expected.boot_info.rollup_config_hash,
                });
            }
        }

        Ok(RangeProofRequest::from_witness_data(
            witness,
            expected_public_values,
        )?)
    }

    pub fn aggregation_request(
        &self,
        boot_infos: Vec<BootInfoStruct>,
        latest_l1_checkpoint_head: B256,
        multi_block_vkey: [u32; 8],
        prover_address: Address,
        l1_headers_cbor: Vec<u8>,
        range_proofs: Vec<Vec<u8>>,
    ) -> AggregationProofRequest {
        AggregationProofRequest {
            inputs: AggregationInputs {
                boot_infos,
                latest_l1_checkpoint_head,
                multi_block_vkey,
                prover_address,
            },
            l1_headers_cbor,
            range_proofs,
        }
    }

    pub fn split_range(
        &self,
        range: L2BlockRange,
    ) -> Result<Vec<L2BlockRange>, WorldSuccinctHostError> {
        split_range(range, self.config.range_split_count.get() as u64)
    }
}

/// Splits a half-open range into nearly equal subranges.
pub fn split_range(
    range: L2BlockRange,
    split_count: u64,
) -> Result<Vec<L2BlockRange>, WorldSuccinctHostError> {
    if range.end <= range.start {
        return Err(WorldSuccinctHostError::InvalidRange {
            start: range.start,
            end: range.end,
        });
    }

    let split_count = split_count.max(1).min(range.len());
    let base_len = range.len() / split_count;
    let remainder = range.len() % split_count;

    let mut start = range.start;
    let mut ranges = Vec::with_capacity(split_count as usize);
    for index in 0..split_count {
        let extra = u64::from(index < remainder);
        let end = start + base_len + extra;
        ranges.push(L2BlockRange::new(start, end)?);
        start = end;
    }

    Ok(ranges)
}

pub const fn split_count_u64(split_count: RangeSplitCount) -> u64 {
    split_count.get() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn game_config(rollup_config_hash: B256) -> WorldFaultDisputeGameConfig {
        WorldFaultDisputeGameConfig {
            aggregation_vkey: B256::from([1; 32]),
            range_vkey_commitment: B256::from([2; 32]),
            rollup_config_hash,
            challenger_addresses: vec![Address::from([3; 20])],
            challenger_bond_wei: U256::from(1u64),
            dispute_game_finality_delay_seconds: 2,
            existing_anchor_state_registry: None,
            existing_dispute_game_factory_proxy: None,
            fallback_timeout_fp_secs: 3,
            game_type: 42,
            initial_bond_wei: U256::from(4u64),
            max_challenge_duration: 5,
            max_prove_duration: 6,
            optimism_portal2_address: Address::from([7; 20]),
            permissionless_mode: false,
            proposer_addresses: vec![Address::from([8; 20])],
            starting_l2_block_number: 9,
            starting_root: B256::from([10; 32]),
            system_config_address: Address::from([11; 20]),
            use_sp1_mock_verifier: false,
            verifier_address: Address::from([12; 20]),
        }
    }

    #[test]
    fn identity_matches_game_config() {
        let game = game_config(B256::from([9; 32]));
        let identity = WorldProverIdentity::new(
            game.aggregation_vkey,
            game.range_vkey_commitment,
            game.rollup_config_hash,
        );
        assert!(identity.matches_game(&game));
    }

    #[test]
    fn identity_hash_changes_with_rollup_config() {
        let first = json!({"jovian_time": 10, "tropo_time": 20});
        let second = json!({"jovian_time": 10, "tropo_time": 21});
        let agg = B256::from([1; 32]);
        let rvc = B256::from([2; 32]);

        let a = WorldProverIdentity::from_rollup_config(agg, rvc, &first).unwrap();
        let b = WorldProverIdentity::from_rollup_config(agg, rvc, &second).unwrap();

        assert_ne!(a.rollup_config_hash, b.rollup_config_hash);
    }

    #[test]
    fn identity_hash_with_world_schedule() {
        let rollup_config = json!({"jovian_time": 10});
        let first_schedule = WorldRangeHardforkConfig {
            tropo_time: Some(20),
            ..Default::default()
        };
        let second_schedule = WorldRangeHardforkConfig {
            tropo_time: Some(21),
            ..Default::default()
        };
        let agg = B256::from([1; 32]);
        let rvc = B256::from([2; 32]);

        let a = WorldProverIdentity::from_world_rollup_config(
            agg,
            rvc,
            &rollup_config,
            &first_schedule,
        )
        .unwrap();
        let b = WorldProverIdentity::from_world_rollup_config(
            agg,
            rvc,
            &rollup_config,
            &second_schedule,
        )
        .unwrap();

        assert_ne!(a.rollup_config_hash, b.rollup_config_hash);
    }

    #[test]
    fn validates_range_split_count() {
        assert_eq!(RangeSplitCount::try_from(16).unwrap().get(), 16);
        assert!(matches!(
            RangeSplitCount::try_from(3),
            Err(WorldSuccinctHostError::InvalidRangeSplitCount(3))
        ));
    }

    #[test]
    fn range_split_count_serializes_as_number() {
        assert_eq!(
            serde_json::to_value(RangeSplitCount::Four).unwrap(),
            json!(4)
        );
        assert_eq!(
            serde_json::from_value::<RangeSplitCount>(json!(16)).unwrap(),
            RangeSplitCount::Sixteen
        );
    }

    #[test]
    fn service_config_exposes_identity() {
        let game = game_config(B256::from([9; 32]));
        let service = WorldFaultProofServiceConfig::new(
            "http://l1".into(),
            "http://beacon".into(),
            "http://l2".into(),
            true,
            4,
            game.clone(),
        )
        .unwrap();
        assert_eq!(service.identity(), game.identity());
        assert_eq!(service.range_split_count, RangeSplitCount::Four);
    }

    #[test]
    fn splits_range_evenly_with_remainder() {
        let ranges = split_range(L2BlockRange::new(10, 20).unwrap(), 4).unwrap();
        assert_eq!(
            ranges,
            vec![
                L2BlockRange { start: 10, end: 13 },
                L2BlockRange { start: 13, end: 16 },
                L2BlockRange { start: 16, end: 18 },
                L2BlockRange { start: 18, end: 20 },
            ]
        );
    }
}
