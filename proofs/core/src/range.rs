//! Shared range-proof public-value types used by all World fault-proof backends.

use alloy_primitives::B256;
use serde::{Deserialize, Serialize};

use crate::boot::BootInfoPublicValues;

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
            // Bedrock is a genesis fork keyed on block number; every other OP Stack fork is
            // keyed on block timestamp. We treat `bedrock_block = None` as "always active"
            // (block 0) so chains that never explicitly enable Bedrock still report it as
            // active, matching the asymmetric semantics in `op-node`.
            WorldRangeHardfork::Bedrock => block_active(self.bedrock_block, block_number),
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

/// Returns whether a block-number-keyed fork is active at `block`.
///
/// Mirrors [`timestamp_active`] but applies the convention that an unset activation
/// (`None`) is treated as `0`, i.e. always active. Bedrock is the only OP Stack fork
/// that activates by block number, so it is the sole user of this helper.
fn block_active(activation: Option<u64>, block: u64) -> bool {
    block >= activation.unwrap_or(0)
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
