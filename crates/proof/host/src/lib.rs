//! Host-side configuration for World OP Succinct Lite fault proofs.
//!
//! This crate mirrors the OP Succinct/Base prover identity model: the deployed game, proposer,
//! challenger, and SP1 programs must all agree on the aggregation vkey, range vkey commitment, and
//! rollup config hash. The rollup config hash must include the World-only Tropo/Strato schedule.

use alloy_primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};
use world_chain_proof_protocol::{
    RollupConfigHashError, WorldHardforkConfig, hash_rollup_config, hash_world_rollup_config,
};

/// Error returned while constructing host-side proof config.
#[derive(Debug, thiserror::Error)]
pub enum WorldProofHostError {
    /// Rollup config serialization failed while computing the OP Succinct config hash.
    #[error(transparent)]
    RollupConfigHash(#[from] RollupConfigHashError),
    /// Invalid range split count.
    #[error("range split count must be one of 1, 2, 4, 8, or 16, got {0}")]
    InvalidRangeSplitCount(u8),
}

/// Number of independent range proofs before aggregation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
#[repr(u8)]
pub enum RangeSplitCount {
    /// One range proof.
    One = 1,
    /// Two range proofs.
    Two = 2,
    /// Four range proofs.
    Four = 4,
    /// Eight range proofs.
    Eight = 8,
    /// Sixteen range proofs.
    Sixteen = 16,
}

impl RangeSplitCount {
    /// Returns the split count as an integer.
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
    type Error = WorldProofHostError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::One),
            2 => Ok(Self::Two),
            4 => Ok(Self::Four),
            8 => Ok(Self::Eight),
            16 => Ok(Self::Sixteen),
            other => Err(WorldProofHostError::InvalidRangeSplitCount(other)),
        }
    }
}

/// Version tag for the World OP Succinct Lite prover identity.
pub const WORLD_OP_SUCCINCT_LITE_VERSION: &str = "world-op-succinct-lite-v1";

/// Prover/game identity checked before participating in a fault dispute game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldProverIdentity {
    /// SP1 aggregation verifying key.
    pub aggregation_vkey: B256,
    /// Commitment to the range verifying key.
    pub range_vkey_commitment: B256,
    /// Hash of the full rollup config.
    pub rollup_config_hash: B256,
}

impl WorldProverIdentity {
    /// Creates an identity from an already computed rollup config hash.
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

    /// Creates an identity from a full rollup config object.
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

    /// Creates an identity from the upstream rollup config plus World-only fork schedule.
    pub fn from_world_rollup_config<T: Serialize + ?Sized>(
        aggregation_vkey: B256,
        range_vkey_commitment: B256,
        rollup_config: &T,
        schedule: &WorldHardforkConfig,
    ) -> Result<Self, RollupConfigHashError> {
        Ok(Self::new(
            aggregation_vkey,
            range_vkey_commitment,
            hash_world_rollup_config(rollup_config, schedule)?,
        ))
    }

    /// Returns `true` when this local identity matches the deployed game config.
    pub fn matches_game(self, game: &WorldFaultDisputeGameConfig) -> bool {
        self == game.identity()
    }
}

/// Fault-dispute-game config consumed by World proposer/challenger services.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldFaultDisputeGameConfig {
    /// SP1 aggregation verifying key expected by the game.
    pub aggregation_vkey: B256,
    /// Commitment to the SP1 range verifying key expected by the game.
    pub range_vkey_commitment: B256,
    /// Hash of the full rollup config expected by the game.
    pub rollup_config_hash: B256,
    /// Addresses allowed to challenge when permissionless mode is disabled.
    pub challenger_addresses: Vec<Address>,
    /// Challenger bond in wei.
    pub challenger_bond_wei: U256,
    /// Finality delay for dispute games.
    pub dispute_game_finality_delay_seconds: u64,
    /// Optional pre-existing AnchorStateRegistry address.
    pub existing_anchor_state_registry: Option<Address>,
    /// Optional pre-existing DisputeGameFactory proxy address.
    pub existing_dispute_game_factory_proxy: Option<Address>,
    /// Fallback timeout used by the fault-proof service.
    pub fallback_timeout_fp_secs: u64,
    /// OP Stack dispute game type.
    pub game_type: u32,
    /// Initial game bond in wei.
    pub initial_bond_wei: U256,
    /// Maximum challenge duration in seconds.
    pub max_challenge_duration: u64,
    /// Maximum prove duration in seconds.
    pub max_prove_duration: u64,
    /// OptimismPortal2 address.
    pub optimism_portal2_address: Address,
    /// Enables permissionless proposer/challenger participation.
    pub permissionless_mode: bool,
    /// Addresses allowed to propose when permissionless mode is disabled.
    pub proposer_addresses: Vec<Address>,
    /// Starting L2 block for the game anchor state.
    pub starting_l2_block_number: u64,
    /// Starting L2 output root for the game anchor state.
    pub starting_root: B256,
    /// L1 SystemConfig address.
    pub system_config_address: Address,
    /// Whether the deployment uses the SP1 mock verifier.
    pub use_sp1_mock_verifier: bool,
    /// SP1 verifier address.
    pub verifier_address: Address,
}

impl WorldFaultDisputeGameConfig {
    /// Extracts the identity committed by this game config.
    pub const fn identity(&self) -> WorldProverIdentity {
        WorldProverIdentity::new(
            self.aggregation_vkey,
            self.range_vkey_commitment,
            self.rollup_config_hash,
        )
    }

    /// Recomputes and updates the rollup config hash from the full rollup config object.
    pub fn with_rollup_config<T: Serialize + ?Sized>(
        mut self,
        rollup_config: &T,
    ) -> Result<Self, RollupConfigHashError> {
        self.rollup_config_hash = hash_rollup_config(rollup_config)?;
        Ok(self)
    }

    /// Recomputes and updates the rollup config hash from a rollup config plus World schedule.
    pub fn with_world_rollup_config<T: Serialize + ?Sized>(
        mut self,
        rollup_config: &T,
        schedule: &WorldHardforkConfig,
    ) -> Result<Self, RollupConfigHashError> {
        self.rollup_config_hash = hash_world_rollup_config(rollup_config, schedule)?;
        Ok(self)
    }
}

/// Runtime config for the World fault-proof host service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldFaultProofServiceConfig {
    /// L1 execution RPC URL.
    pub l1_rpc_url: String,
    /// L1 beacon RPC URL.
    pub l1_beacon_rpc_url: String,
    /// L2 execution RPC URL.
    pub l2_rpc_url: String,
    /// Whether the host may fall back to a safe DB when RPC data is unavailable.
    pub safe_db_fallback: bool,
    /// Number of range proofs generated before aggregation.
    pub range_split_count: RangeSplitCount,
    /// Game/deployment config.
    pub fault_dispute_game: WorldFaultDisputeGameConfig,
}

impl WorldFaultProofServiceConfig {
    /// Creates service config with a validated split count.
    pub fn new(
        l1_rpc_url: String,
        l1_beacon_rpc_url: String,
        l2_rpc_url: String,
        safe_db_fallback: bool,
        range_split_count: u8,
        fault_dispute_game: WorldFaultDisputeGameConfig,
    ) -> Result<Self, WorldProofHostError> {
        Ok(Self {
            l1_rpc_url,
            l1_beacon_rpc_url,
            l2_rpc_url,
            safe_db_fallback,
            range_split_count: range_split_count.try_into()?,
            fault_dispute_game,
        })
    }

    /// Returns the prover identity expected by this service.
    pub const fn identity(&self) -> WorldProverIdentity {
        self.fault_dispute_game.identity()
    }
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
            challenger_bond_wei: U256::from(1),
            dispute_game_finality_delay_seconds: 2,
            existing_anchor_state_registry: None,
            existing_dispute_game_factory_proxy: None,
            fallback_timeout_fp_secs: 3,
            game_type: 42,
            initial_bond_wei: U256::from(4),
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
    fn identity_hash_changes_with_world_only_forks() {
        let first = json!({
            "jovian_time": 10,
            "tropo_time": 20,
            "strato_time": 30
        });
        let second = json!({
            "jovian_time": 10,
            "tropo_time": 21,
            "strato_time": 30
        });
        let aggregation_vkey = B256::from([1; 32]);
        let range_vkey_commitment = B256::from([2; 32]);

        let first_identity = WorldProverIdentity::from_rollup_config(
            aggregation_vkey,
            range_vkey_commitment,
            &first,
        )
        .unwrap();
        let second_identity = WorldProverIdentity::from_rollup_config(
            aggregation_vkey,
            range_vkey_commitment,
            &second,
        )
        .unwrap();

        assert_ne!(
            first_identity.rollup_config_hash,
            second_identity.rollup_config_hash
        );
    }

    #[test]
    fn identity_hash_can_append_world_schedule_to_upstream_rollup_config() {
        let rollup_config = json!({
            "jovian_time": 10
        });
        let first_schedule = WorldHardforkConfig {
            jovian_time: Some(10),
            tropo_time: Some(20),
            strato_time: Some(30),
            ..Default::default()
        };
        let second_schedule = WorldHardforkConfig {
            jovian_time: Some(10),
            tropo_time: Some(21),
            strato_time: Some(30),
            ..Default::default()
        };
        let aggregation_vkey = B256::from([1; 32]);
        let range_vkey_commitment = B256::from([2; 32]);

        let first_identity = WorldProverIdentity::from_world_rollup_config(
            aggregation_vkey,
            range_vkey_commitment,
            &rollup_config,
            &first_schedule,
        )
        .unwrap();
        let second_identity = WorldProverIdentity::from_world_rollup_config(
            aggregation_vkey,
            range_vkey_commitment,
            &rollup_config,
            &second_schedule,
        )
        .unwrap();

        assert_ne!(
            first_identity.rollup_config_hash,
            second_identity.rollup_config_hash
        );
    }

    #[test]
    fn validates_range_split_count() {
        assert_eq!(RangeSplitCount::try_from(16).unwrap().get(), 16);
        assert!(matches!(
            RangeSplitCount::try_from(3),
            Err(WorldProofHostError::InvalidRangeSplitCount(3))
        ));
    }

    #[test]
    fn range_split_count_serializes_as_number() {
        assert_eq!(
            serde_json::to_value(RangeSplitCount::Four).unwrap(),
            serde_json::json!(4)
        );
        assert_eq!(
            serde_json::from_value::<RangeSplitCount>(serde_json::json!(16)).unwrap(),
            RangeSplitCount::Sixteen
        );
    }

    #[test]
    fn service_config_exposes_identity() {
        let game = game_config(B256::from([9; 32]));
        let service = WorldFaultProofServiceConfig::new(
            "http://l1".to_string(),
            "http://beacon".to_string(),
            "http://l2".to_string(),
            true,
            4,
            game.clone(),
        )
        .unwrap();

        assert_eq!(service.identity(), game.identity());
        assert_eq!(service.range_split_count, RangeSplitCount::Four);
    }
}
