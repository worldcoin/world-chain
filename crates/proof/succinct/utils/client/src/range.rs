//! Range-program public-value construction shared by the host and SP1 guest.

use alloy_primitives::{B256, keccak256};
use serde::{Deserialize, Serialize};

use crate::boot::{BootInfoPublicValues, BootInfoStruct};

/// World hardfork activation schedule carried by World range proof inputs.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct WorldRangeHardforkConfig {
    /// Bedrock activation block.
    #[serde(default, alias = "bedrockBlock")]
    pub bedrock_block: Option<u64>,
    /// Regolith activation timestamp.
    #[serde(default, alias = "regolithTime")]
    pub regolith_time: Option<u64>,
    /// Canyon activation timestamp.
    #[serde(default, alias = "canyonTime")]
    pub canyon_time: Option<u64>,
    /// Ecotone activation timestamp.
    #[serde(default, alias = "ecotoneTime")]
    pub ecotone_time: Option<u64>,
    /// Fjord activation timestamp.
    #[serde(default, alias = "fjordTime")]
    pub fjord_time: Option<u64>,
    /// Granite activation timestamp.
    #[serde(default, alias = "graniteTime")]
    pub granite_time: Option<u64>,
    /// Holocene activation timestamp.
    #[serde(default, alias = "holoceneTime")]
    pub holocene_time: Option<u64>,
    /// Isthmus activation timestamp.
    #[serde(default, alias = "isthmusTime")]
    pub isthmus_time: Option<u64>,
    /// Jovian activation timestamp.
    #[serde(default, alias = "jovianTime")]
    pub jovian_time: Option<u64>,
    /// Tropo activation timestamp. This is a World-only fork.
    #[serde(default, alias = "tropoTime")]
    pub tropo_time: Option<u64>,
    /// Strato activation timestamp. This is a World-only fork.
    #[serde(default, alias = "stratoTime")]
    pub strato_time: Option<u64>,
}

impl WorldRangeHardforkConfig {
    /// Returns `true` when the fork is active at the given L2 block/timestamp.
    pub fn is_active(&self, fork: WorldRangeHardfork, block_number: u64, timestamp: u64) -> bool {
        match fork {
            WorldRangeHardfork::Bedrock => block_number >= self.bedrock_block.unwrap_or(0),
            WorldRangeHardfork::Regolith => timestamp_active(self.regolith_time, timestamp),
            WorldRangeHardfork::Canyon => timestamp_active(self.canyon_time, timestamp),
            WorldRangeHardfork::Ecotone => timestamp_active(self.ecotone_time, timestamp),
            WorldRangeHardfork::Fjord => timestamp_active(self.fjord_time, timestamp),
            WorldRangeHardfork::Granite => timestamp_active(self.granite_time, timestamp),
            WorldRangeHardfork::Holocene => timestamp_active(self.holocene_time, timestamp),
            WorldRangeHardfork::Isthmus => timestamp_active(self.isthmus_time, timestamp),
            WorldRangeHardfork::Jovian => timestamp_active(self.jovian_time, timestamp),
            WorldRangeHardfork::Tropo => timestamp_active(self.tropo_time, timestamp),
            WorldRangeHardfork::Strato => timestamp_active(self.strato_time, timestamp),
        }
    }

    /// Returns the latest active World hardfork at the given L2 block/timestamp.
    pub fn active_fork_at(&self, block_number: u64, timestamp: u64) -> WorldRangeHardfork {
        [
            WorldRangeHardfork::Strato,
            WorldRangeHardfork::Tropo,
            WorldRangeHardfork::Jovian,
            WorldRangeHardfork::Isthmus,
            WorldRangeHardfork::Holocene,
            WorldRangeHardfork::Granite,
            WorldRangeHardfork::Fjord,
            WorldRangeHardfork::Ecotone,
            WorldRangeHardfork::Canyon,
            WorldRangeHardfork::Regolith,
        ]
        .into_iter()
        .find(|fork| self.is_active(*fork, block_number, timestamp))
        .unwrap_or(WorldRangeHardfork::Bedrock)
    }
}

fn timestamp_active(activation: Option<u64>, timestamp: u64) -> bool {
    activation.is_some_and(|activation| timestamp >= activation)
}

/// World hardfork names used inside range proof public values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorldRangeHardfork {
    /// Bedrock hardfork.
    Bedrock,
    /// Regolith hardfork.
    Regolith,
    /// Canyon hardfork.
    Canyon,
    /// Ecotone hardfork.
    Ecotone,
    /// Fjord hardfork.
    Fjord,
    /// Granite hardfork.
    Granite,
    /// Holocene hardfork.
    Holocene,
    /// Isthmus hardfork.
    Isthmus,
    /// Jovian hardfork.
    Jovian,
    /// Tropo hardfork.
    Tropo,
    /// Strato hardfork.
    Strato,
}

/// World proof spec id used by the range proof wrapper.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(non_camel_case_types)]
pub enum WorldRangeSpecId {
    /// Bedrock spec id.
    BEDROCK = 100,
    /// Regolith spec id.
    REGOLITH,
    /// Canyon spec id.
    CANYON,
    /// Ecotone spec id.
    ECOTONE,
    /// Fjord spec id.
    FJORD,
    /// Granite spec id.
    GRANITE,
    /// Holocene spec id.
    HOLOCENE,
    /// Isthmus spec id.
    ISTHMUS,
    /// Jovian spec id.
    JOVIAN,
    /// Tropo spec id.
    TROPO,
    /// Strato spec id.
    STRATO,
}

impl WorldRangeSpecId {
    /// Converts a World hardfork name to the corresponding proof spec id.
    pub const fn from_hardfork(hardfork: WorldRangeHardfork) -> Self {
        match hardfork {
            WorldRangeHardfork::Bedrock => Self::BEDROCK,
            WorldRangeHardfork::Regolith => Self::REGOLITH,
            WorldRangeHardfork::Canyon => Self::CANYON,
            WorldRangeHardfork::Ecotone => Self::ECOTONE,
            WorldRangeHardfork::Fjord => Self::FJORD,
            WorldRangeHardfork::Granite => Self::GRANITE,
            WorldRangeHardfork::Holocene => Self::HOLOCENE,
            WorldRangeHardfork::Isthmus => Self::ISTHMUS,
            WorldRangeHardfork::Jovian => Self::JOVIAN,
            WorldRangeHardfork::Tropo => Self::TROPO,
            WorldRangeHardfork::Strato => Self::STRATO,
        }
    }
}

impl From<WorldRangeSpecId> for &'static str {
    fn from(spec_id: WorldRangeSpecId) -> Self {
        match spec_id {
            WorldRangeSpecId::BEDROCK => "Bedrock",
            WorldRangeSpecId::REGOLITH => "Regolith",
            WorldRangeSpecId::CANYON => "Canyon",
            WorldRangeSpecId::ECOTONE => "Ecotone",
            WorldRangeSpecId::FJORD => "Fjord",
            WorldRangeSpecId::GRANITE => "Granite",
            WorldRangeSpecId::HOLOCENE => "Holocene",
            WorldRangeSpecId::ISTHMUS => "Isthmus",
            WorldRangeSpecId::JOVIAN => "Jovian",
            WorldRangeSpecId::TROPO => "Tropo",
            WorldRangeSpecId::STRATO => "Strato",
        }
    }
}

/// Claimed transition proven by the World range program.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldRangeProofClaim {
    /// L1 head used for the derivation pipeline.
    pub l1_head: B256,
    /// Agreed pre-state output root.
    pub agreed_l2_output_root: B256,
    /// Claimed post-state output root.
    pub claimed_l2_output_root: B256,
    /// Claimed post-state L2 block number.
    pub claimed_l2_block_number: u64,
}

impl WorldRangeProofClaim {
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
pub struct WorldRangeProofInput {
    /// World hardfork schedule extracted from the rollup config.
    pub schedule: WorldRangeHardforkConfig,
    /// Claimed transition.
    pub claim: WorldRangeProofClaim,
    /// Timestamp of the claimed post-state L2 block.
    pub claimed_l2_timestamp: u64,
    /// Hash of the full rollup config, computed with OP Succinct's hashing method.
    pub rollup_config_hash: B256,
}

impl WorldRangeProofInput {
    /// Builds the public values expected from the range proof.
    pub fn public_values(self) -> WorldRangeProofPublicValues {
        let active_fork = self.schedule.active_fork_at(
            self.claim.claimed_l2_block_number,
            self.claimed_l2_timestamp,
        );
        let world_spec_id = WorldRangeSpecId::from_hardfork(active_fork);
        WorldRangeProofPublicValues {
            boot_info: self.claim.boot_info(self.rollup_config_hash),
            active_fork,
            world_spec_id,
        }
    }
}

/// Public values emitted by the World range proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldRangeProofPublicValues {
    /// Public values that match OP Succinct's fault-proof contract inputs.
    pub boot_info: BootInfoPublicValues,
    /// Latest active World hardfork at the claimed L2 block.
    pub active_fork: WorldRangeHardfork,
    /// EVM spec id used by the proof.
    pub world_spec_id: WorldRangeSpecId,
}

/// Validates actual public values against expected public values.
pub fn validate_public_values(
    expected: &WorldRangeProofPublicValues,
    actual: &WorldRangeProofPublicValues,
) -> Result<(), WorldRangeProofValidationError> {
    if expected.boot_info.l1_head != actual.boot_info.l1_head {
        return Err(WorldRangeProofValidationError::L1Head {
            expected: expected.boot_info.l1_head,
            actual: actual.boot_info.l1_head,
        });
    }
    if expected.boot_info.l2_pre_root != actual.boot_info.l2_pre_root {
        return Err(WorldRangeProofValidationError::L2PreRoot {
            expected: expected.boot_info.l2_pre_root,
            actual: actual.boot_info.l2_pre_root,
        });
    }
    if expected.boot_info.l2_post_root != actual.boot_info.l2_post_root {
        return Err(WorldRangeProofValidationError::L2PostRoot {
            expected: expected.boot_info.l2_post_root,
            actual: actual.boot_info.l2_post_root,
        });
    }
    if expected.boot_info.l2_block_number != actual.boot_info.l2_block_number {
        return Err(WorldRangeProofValidationError::L2BlockNumber {
            expected: expected.boot_info.l2_block_number,
            actual: actual.boot_info.l2_block_number,
        });
    }
    if expected.boot_info.rollup_config_hash != actual.boot_info.rollup_config_hash {
        return Err(WorldRangeProofValidationError::RollupConfigHash {
            expected: expected.boot_info.rollup_config_hash,
            actual: actual.boot_info.rollup_config_hash,
        });
    }
    if expected.active_fork != actual.active_fork {
        return Err(WorldRangeProofValidationError::ActiveFork {
            expected: expected.active_fork,
            actual: actual.active_fork,
        });
    }
    if expected.world_spec_id != actual.world_spec_id {
        return Err(WorldRangeProofValidationError::WorldSpecId {
            expected: expected.world_spec_id,
            actual: actual.world_spec_id,
        });
    }
    Ok(())
}

/// Validation error for mismatched proof public values.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorldRangeProofValidationError {
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
        expected: WorldRangeHardfork,
        actual: WorldRangeHardfork,
    },
    /// Active World proof spec does not match.
    #[error("world spec id mismatch: expected {expected:?}, got {actual:?}")]
    WorldSpecId {
        expected: WorldRangeSpecId,
        actual: WorldRangeSpecId,
    },
}

/// Data needed to recompute an OP Stack output root for one L2 block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputRootWitness {
    /// L2 state root from the execution payload/header.
    pub state_root: B256,
    /// Storage root of the `L2ToL1MessagePasser` predeploy at this block.
    pub message_passer_storage_root: B256,
    /// L2 block hash.
    pub block_hash: B256,
}

impl OutputRootWitness {
    /// Computes the OP Stack output root:
    /// `keccak256(version || state_root || message_passer_storage_root || block_hash)`.
    pub fn output_root(&self) -> B256 {
        let mut preimage = [0u8; 128];
        preimage[32..64].copy_from_slice(self.state_root.as_slice());
        preimage[64..96].copy_from_slice(self.message_passer_storage_root.as_slice());
        preimage[96..128].copy_from_slice(self.block_hash.as_slice());
        keccak256(preimage)
    }
}

/// Witness shape read by the World range guest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldRangeWitness {
    /// World proof input used to construct the committed boot values.
    pub input: WorldRangeProofInput,
    /// Optional output-root witness for the agreed pre-state.
    pub pre_state: Option<OutputRootWitness>,
    /// Optional output-root witness for the claimed post-state.
    pub post_state: Option<OutputRootWitness>,
    /// Optional host-computed public values. When present, the guest checks them before committing.
    pub expected_public_values: Option<WorldRangeProofPublicValues>,
}

impl WorldRangeWitness {
    /// Creates a range witness without a separate expected public-value copy.
    pub const fn new(input: WorldRangeProofInput) -> Self {
        Self {
            input,
            pre_state: None,
            post_state: None,
            expected_public_values: None,
        }
    }

    /// Creates a range witness with explicit host-computed public values to validate.
    pub const fn with_expected_public_values(
        input: WorldRangeProofInput,
        expected_public_values: WorldRangeProofPublicValues,
    ) -> Self {
        Self {
            input,
            pre_state: None,
            post_state: None,
            expected_public_values: Some(expected_public_values),
        }
    }

    /// Attaches output-root witnesses for the pre/post L2 states.
    pub fn with_output_root_witnesses(
        mut self,
        pre_state: OutputRootWitness,
        post_state: OutputRootWitness,
    ) -> Self {
        self.pre_state = Some(pre_state);
        self.post_state = Some(post_state);
        self
    }
}

/// Error returned by the range public-value wrapper.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RangeProgramError {
    /// Host supplied expected values that do not match the witness.
    #[error(transparent)]
    InvalidPublicValues(#[from] WorldRangeProofValidationError),
    /// The pre-state output root does not match the supplied output-root witness.
    #[error("pre-state output root mismatch: expected {expected:?}, got {actual:?}")]
    PreStateOutputRoot { expected: B256, actual: B256 },
    /// The post-state output root does not match the supplied output-root witness.
    #[error("post-state output root mismatch: expected {expected:?}, got {actual:?}")]
    PostStateOutputRoot { expected: B256, actual: B256 },
}

/// Runs the World range-program wrapper and returns OP Succinct-compatible boot values.
pub fn run_range_program(witness: WorldRangeWitness) -> Result<BootInfoStruct, RangeProgramError> {
    if let Some(pre_state) = &witness.pre_state {
        let actual = pre_state.output_root();
        let expected = witness.input.claim.agreed_l2_output_root;
        if actual != expected {
            return Err(RangeProgramError::PreStateOutputRoot { expected, actual });
        }
    }

    if let Some(post_state) = &witness.post_state {
        let actual = post_state.output_root();
        let expected = witness.input.claim.claimed_l2_output_root;
        if actual != expected {
            return Err(RangeProgramError::PostStateOutputRoot { expected, actual });
        }
    }

    let public_values = witness.input.public_values();
    if let Some(expected) = witness.expected_public_values {
        validate_public_values(&expected, &public_values)?;
    }
    Ok(BootInfoStruct::from(public_values.boot_info))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

    fn input() -> WorldRangeProofInput {
        WorldRangeProofInput {
            schedule: WorldRangeHardforkConfig {
                jovian_time: Some(10),
                tropo_time: Some(20),
                strato_time: Some(30),
                ..Default::default()
            },
            claim: WorldRangeProofClaim {
                l1_head: B256::from([1; 32]),
                agreed_l2_output_root: B256::from([2; 32]),
                claimed_l2_output_root: B256::from([3; 32]),
                claimed_l2_block_number: 42,
            },
            claimed_l2_timestamp: 30,
            rollup_config_hash: B256::from([4; 32]),
        }
    }

    #[test]
    fn emits_boot_values_for_world_schedule() {
        let boot_info = run_range_program(WorldRangeWitness::new(input())).unwrap();

        assert_eq!(boot_info.l1Head, B256::from([1; 32]));
        assert_eq!(boot_info.l2PreRoot, B256::from([2; 32]));
        assert_eq!(boot_info.l2PostRoot, B256::from([3; 32]));
        assert_eq!(boot_info.l2BlockNumber, 42);
        assert_eq!(boot_info.rollupConfigHash, B256::from([4; 32]));
    }

    #[test]
    fn validates_expected_public_values() {
        let input = input();
        let expected = input.clone().public_values();

        assert_eq!(expected.world_spec_id, WorldRangeSpecId::STRATO);
        assert!(
            run_range_program(WorldRangeWitness::with_expected_public_values(
                input, expected
            ))
            .is_ok()
        );
    }

    #[test]
    fn validates_output_root_witnesses() {
        let pre_state = OutputRootWitness {
            state_root: B256::from([10; 32]),
            message_passer_storage_root: B256::from([11; 32]),
            block_hash: B256::from([12; 32]),
        };
        let post_state = OutputRootWitness {
            state_root: B256::from([20; 32]),
            message_passer_storage_root: B256::from([21; 32]),
            block_hash: B256::from([22; 32]),
        };
        let mut input = input();
        input.claim.agreed_l2_output_root = pre_state.output_root();
        input.claim.claimed_l2_output_root = post_state.output_root();

        assert!(
            run_range_program(
                WorldRangeWitness::new(input).with_output_root_witnesses(pre_state, post_state)
            )
            .is_ok()
        );
    }
}
