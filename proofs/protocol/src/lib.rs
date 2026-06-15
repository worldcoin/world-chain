//! Proof protocol types for World Chain OP Succinct Lite fault proofs.
//!
//! The prover follows the OP Stack schedule through Jovian. Tropo and Strato are
//! World-only hardforks and are carried in the proof config/hash instead of being
//! represented as OP Karst or Interop activations.

use core::str::FromStr;

use alloy_primitives::B256;
use op_revm::OpSpecId;
use reth_chainspec::ForkCondition;
use reth_optimism_forks::OpHardfork;
use revm_primitives::hardfork::{SpecId, UnknownHardfork};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
pub use world_chain_chainspec::WorldChainHardfork;
use world_chain_chainspec::WorldChainHardforks;

/// Error returned when a rollup config cannot be serialized for hashing.
#[derive(Debug, thiserror::Error)]
pub enum RollupConfigHashError {
    /// Serde failed while producing the pretty JSON representation used by OP Succinct.
    #[error("failed to serialize rollup config for hashing: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Hashes a rollup config exactly like OP Succinct Lite: pretty JSON, then SHA-256.
///
/// The resulting hash is part of the game/prover identity. World fork fields such as
/// `tropo_time`/`tropoTime` and `strato_time`/`stratoTime` must therefore be present in the
/// serialized config used by both the host and the zkVM.
pub fn hash_rollup_config<T: Serialize + ?Sized>(
    config: &T,
) -> Result<B256, RollupConfigHashError> {
    let serialized_config = serde_json::to_string_pretty(config)?;
    Ok(sha256_b256(serialized_config.as_bytes()))
}

/// Hashes a rollup config plus World-only fork schedule fields.
///
/// OP Succinct hashes Kona's rollup config as pretty JSON. World range proofs execute with the
/// upstream Kona `RollupConfig`, so Tropo/Strato must be appended separately to match the guest.
pub fn hash_world_rollup_config<T: Serialize + ?Sized>(
    rollup_config: &T,
    schedule: &WorldHardforkConfig,
) -> Result<B256, RollupConfigHashError> {
    let serialized_config = serde_json::to_string_pretty(&WorldRollupConfigHashInput {
        rollup_config,
        tropo_time: schedule.tropo_time,
        strato_time: schedule.strato_time,
    })?;
    Ok(sha256_b256(serialized_config.as_bytes()))
}

/// Hashes already serialized rollup-config bytes with SHA-256.
pub fn hash_rollup_config_bytes(serialized_config: impl AsRef<[u8]>) -> B256 {
    sha256_b256(serialized_config.as_ref())
}

fn sha256_b256(bytes: &[u8]) -> B256 {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let hash = hasher.finalize();
    B256::from_slice(hash.as_ref())
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

/// World hardfork activation schedule carried by World proof inputs.
///
/// OP rollup config JSON generally uses snake_case fields, while genesis extra fields use camel
/// case. The aliases let the prover consume either shape without losing the World-only forks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldHardforkConfig {
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

impl WorldHardforkConfig {
    /// Builds a proof schedule from a World chain spec.
    pub fn from_chain_spec<S>(chain_spec: &S) -> Self
    where
        S: WorldChainHardforks + ?Sized,
    {
        Self {
            bedrock_block: block_number(chain_spec, WorldChainHardfork::Bedrock),
            regolith_time: timestamp(chain_spec, WorldChainHardfork::Regolith),
            canyon_time: timestamp(chain_spec, WorldChainHardfork::Canyon),
            ecotone_time: timestamp(chain_spec, WorldChainHardfork::Ecotone),
            fjord_time: timestamp(chain_spec, WorldChainHardfork::Fjord),
            granite_time: timestamp(chain_spec, WorldChainHardfork::Granite),
            holocene_time: timestamp(chain_spec, WorldChainHardfork::Holocene),
            isthmus_time: timestamp(chain_spec, WorldChainHardfork::Isthmus),
            jovian_time: timestamp(chain_spec, WorldChainHardfork::Jovian),
            tropo_time: timestamp(chain_spec, WorldChainHardfork::Tropo),
            strato_time: timestamp(chain_spec, WorldChainHardfork::Strato),
        }
    }

    /// Extracts the hardfork schedule from a full rollup config JSON value.
    pub fn from_rollup_config_value(value: &serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value.clone())
    }

    /// Returns the World activation condition for a hardfork.
    pub fn activation(&self, fork: WorldChainHardfork) -> ForkCondition {
        match fork {
            WorldChainHardfork::Bedrock => ForkCondition::Block(self.bedrock_block.unwrap_or(0)),
            WorldChainHardfork::Regolith => timestamp_condition(self.regolith_time),
            WorldChainHardfork::Canyon => timestamp_condition(self.canyon_time),
            WorldChainHardfork::Ecotone => timestamp_condition(self.ecotone_time),
            WorldChainHardfork::Fjord => timestamp_condition(self.fjord_time),
            WorldChainHardfork::Granite => timestamp_condition(self.granite_time),
            WorldChainHardfork::Holocene => timestamp_condition(self.holocene_time),
            WorldChainHardfork::Isthmus => timestamp_condition(self.isthmus_time),
            WorldChainHardfork::Jovian => timestamp_condition(self.jovian_time),
            WorldChainHardfork::Tropo => timestamp_condition(self.tropo_time),
            WorldChainHardfork::Strato => timestamp_condition(self.strato_time),
            _ => ForkCondition::Never,
        }
    }

    /// Returns the OP-compatible activation condition through Jovian only.
    pub fn op_activation(&self, fork: OpHardfork) -> ForkCondition {
        match fork {
            OpHardfork::Bedrock => self.activation(WorldChainHardfork::Bedrock),
            OpHardfork::Regolith => self.activation(WorldChainHardfork::Regolith),
            OpHardfork::Canyon => self.activation(WorldChainHardfork::Canyon),
            OpHardfork::Ecotone => self.activation(WorldChainHardfork::Ecotone),
            OpHardfork::Fjord => self.activation(WorldChainHardfork::Fjord),
            OpHardfork::Granite => self.activation(WorldChainHardfork::Granite),
            OpHardfork::Holocene => self.activation(WorldChainHardfork::Holocene),
            OpHardfork::Isthmus => self.activation(WorldChainHardfork::Isthmus),
            OpHardfork::Jovian => self.activation(WorldChainHardfork::Jovian),
            OpHardfork::Karst | OpHardfork::Interop => ForkCondition::Never,
            _ => ForkCondition::Never,
        }
    }

    /// Returns `true` when the fork is active at the given L2 block/timestamp.
    pub fn is_active(&self, fork: WorldChainHardfork, block_number: u64, timestamp: u64) -> bool {
        self.activation(fork)
            .active_at_timestamp_or_number(timestamp, block_number)
    }

    /// Returns the latest active World hardfork at the given L2 block/timestamp.
    pub fn active_fork_at(&self, block_number: u64, timestamp: u64) -> WorldChainHardfork {
        [
            WorldChainHardfork::Strato,
            WorldChainHardfork::Tropo,
            WorldChainHardfork::Jovian,
            WorldChainHardfork::Isthmus,
            WorldChainHardfork::Holocene,
            WorldChainHardfork::Granite,
            WorldChainHardfork::Fjord,
            WorldChainHardfork::Ecotone,
            WorldChainHardfork::Canyon,
            WorldChainHardfork::Regolith,
        ]
        .into_iter()
        .find(|fork| self.is_active(*fork, block_number, timestamp))
        .unwrap_or(WorldChainHardfork::Bedrock)
    }

    /// Returns the proof EVM spec id active at the given L2 block/timestamp.
    pub fn proof_spec_at(&self, block_number: u64, timestamp: u64) -> WorldSpecId {
        WorldSpecId::from_hardfork(self.active_fork_at(block_number, timestamp))
    }
}

fn block_number<S>(chain_spec: &S, fork: WorldChainHardfork) -> Option<u64>
where
    S: WorldChainHardforks + ?Sized,
{
    chain_spec.world_chain_fork_activation(fork).block_number()
}

fn timestamp<S>(chain_spec: &S, fork: WorldChainHardfork) -> Option<u64>
where
    S: WorldChainHardforks + ?Sized,
{
    chain_spec.world_chain_fork_activation(fork).as_timestamp()
}

fn timestamp_condition(timestamp: Option<u64>) -> ForkCondition {
    timestamp.map_or(ForkCondition::Never, ForkCondition::Timestamp)
}

/// World-specific proof spec id.
///
/// This mirrors OP `OpSpecId` through Jovian. Tropo and Strato intentionally do not map to OP
/// Karst/Interop schedule activations, but they do use the Osaka EVM revision like the Base
/// post-Jovian proof implementation.
#[repr(u8)]
#[derive(
    Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
#[allow(non_camel_case_types)]
pub enum WorldSpecId {
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
    #[default]
    JOVIAN,
    /// Tropo spec id.
    TROPO,
    /// Strato spec id.
    STRATO,
}

impl WorldSpecId {
    /// The latest World proof spec known to this crate.
    pub const LATEST: Self = Self::STRATO;

    /// Converts a World hardfork name to the corresponding proof spec id.
    pub const fn from_hardfork(hardfork: WorldChainHardfork) -> Self {
        match hardfork {
            WorldChainHardfork::Bedrock => Self::BEDROCK,
            WorldChainHardfork::Regolith => Self::REGOLITH,
            WorldChainHardfork::Canyon => Self::CANYON,
            WorldChainHardfork::Ecotone => Self::ECOTONE,
            WorldChainHardfork::Fjord => Self::FJORD,
            WorldChainHardfork::Granite => Self::GRANITE,
            WorldChainHardfork::Holocene => Self::HOLOCENE,
            WorldChainHardfork::Isthmus => Self::ISTHMUS,
            WorldChainHardfork::Jovian => Self::JOVIAN,
            WorldChainHardfork::Tropo => Self::TROPO,
            WorldChainHardfork::Strato => Self::STRATO,
            _ => Self::LATEST,
        }
    }

    /// Converts the World proof spec id into an Ethereum revm spec id.
    pub const fn into_eth_spec(self) -> SpecId {
        match self {
            Self::BEDROCK | Self::REGOLITH => SpecId::MERGE,
            Self::CANYON => SpecId::SHANGHAI,
            Self::ECOTONE | Self::FJORD | Self::GRANITE | Self::HOLOCENE => SpecId::CANCUN,
            Self::ISTHMUS | Self::JOVIAN => SpecId::PRAGUE,
            Self::TROPO | Self::STRATO => SpecId::OSAKA,
        }
    }

    /// Compatibility bridge for EVM factories that still accept `OpSpecId`.
    ///
    /// Returning `KARST` for Tropo/Strato selects the Osaka EVM revision in `op-revm`; it must not
    /// be used as evidence that OP Karst is active in the World hardfork schedule.
    pub const fn into_op_revm_spec(self) -> OpSpecId {
        match self {
            Self::BEDROCK => OpSpecId::BEDROCK,
            Self::REGOLITH => OpSpecId::REGOLITH,
            Self::CANYON => OpSpecId::CANYON,
            Self::ECOTONE => OpSpecId::ECOTONE,
            Self::FJORD => OpSpecId::FJORD,
            Self::GRANITE => OpSpecId::GRANITE,
            Self::HOLOCENE => OpSpecId::HOLOCENE,
            Self::ISTHMUS => OpSpecId::ISTHMUS,
            Self::JOVIAN => OpSpecId::JOVIAN,
            Self::TROPO | Self::STRATO => OpSpecId::KARST,
        }
    }

    /// Checks if `other` is enabled in `self`.
    pub const fn is_enabled_in(self, other: Self) -> bool {
        self as u8 >= other as u8
    }
}

impl From<WorldSpecId> for SpecId {
    fn from(spec: WorldSpecId) -> Self {
        spec.into_eth_spec()
    }
}

impl From<WorldSpecId> for OpSpecId {
    fn from(spec: WorldSpecId) -> Self {
        spec.into_op_revm_spec()
    }
}

impl FromStr for WorldSpecId {
    type Err = UnknownHardfork;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            name::BEDROCK => Ok(Self::BEDROCK),
            name::REGOLITH => Ok(Self::REGOLITH),
            name::CANYON => Ok(Self::CANYON),
            name::ECOTONE => Ok(Self::ECOTONE),
            name::FJORD => Ok(Self::FJORD),
            name::GRANITE => Ok(Self::GRANITE),
            name::HOLOCENE => Ok(Self::HOLOCENE),
            name::ISTHMUS => Ok(Self::ISTHMUS),
            name::JOVIAN => Ok(Self::JOVIAN),
            name::TROPO => Ok(Self::TROPO),
            name::STRATO => Ok(Self::STRATO),
            _ => Err(UnknownHardfork),
        }
    }
}

impl From<WorldSpecId> for &'static str {
    fn from(spec_id: WorldSpecId) -> Self {
        match spec_id {
            WorldSpecId::BEDROCK => name::BEDROCK,
            WorldSpecId::REGOLITH => name::REGOLITH,
            WorldSpecId::CANYON => name::CANYON,
            WorldSpecId::ECOTONE => name::ECOTONE,
            WorldSpecId::FJORD => name::FJORD,
            WorldSpecId::GRANITE => name::GRANITE,
            WorldSpecId::HOLOCENE => name::HOLOCENE,
            WorldSpecId::ISTHMUS => name::ISTHMUS,
            WorldSpecId::JOVIAN => name::JOVIAN,
            WorldSpecId::TROPO => name::TROPO,
            WorldSpecId::STRATO => name::STRATO,
        }
    }
}

/// String identifiers for World proof specs.
pub mod name {
    /// Bedrock spec name.
    pub const BEDROCK: &str = "Bedrock";
    /// Regolith spec name.
    pub const REGOLITH: &str = "Regolith";
    /// Canyon spec name.
    pub const CANYON: &str = "Canyon";
    /// Ecotone spec name.
    pub const ECOTONE: &str = "Ecotone";
    /// Fjord spec name.
    pub const FJORD: &str = "Fjord";
    /// Granite spec name.
    pub const GRANITE: &str = "Granite";
    /// Holocene spec name.
    pub const HOLOCENE: &str = "Holocene";
    /// Isthmus spec name.
    pub const ISTHMUS: &str = "Isthmus";
    /// Jovian spec name.
    pub const JOVIAN: &str = "Jovian";
    /// Tropo spec name.
    pub const TROPO: &str = "Tropo";
    /// Strato spec name.
    pub const STRATO: &str = "Strato";
}

#[cfg(test)]
mod tests {
    use super::*;
    use reth_optimism_forks::OpHardfork;
    use revm_primitives::hardfork::SpecId;
    use serde_json::json;

    #[test]
    fn parses_snake_and_camel_world_fork_times() {
        let snake: WorldHardforkConfig = serde_json::from_value(json!({
            "jovian_time": 10,
            "tropo_time": 20,
            "strato_time": 30
        }))
        .unwrap();
        let camel: WorldHardforkConfig = serde_json::from_value(json!({
            "jovianTime": 10,
            "tropoTime": 20,
            "stratoTime": 30
        }))
        .unwrap();

        assert_eq!(snake, camel);
        assert_eq!(snake.tropo_time, Some(20));
        assert_eq!(snake.strato_time, Some(30));
    }

    #[test]
    fn world_schedule_activates_tropo_and_strato() {
        let config = WorldHardforkConfig {
            jovian_time: Some(10),
            tropo_time: Some(20),
            strato_time: Some(30),
            ..Default::default()
        };

        assert_eq!(config.active_fork_at(1, 19), WorldChainHardfork::Jovian);
        assert_eq!(config.active_fork_at(1, 20), WorldChainHardfork::Tropo);
        assert_eq!(config.active_fork_at(1, 30), WorldChainHardfork::Strato);
        assert_eq!(config.proof_spec_at(1, 30), WorldSpecId::STRATO);
    }

    #[test]
    fn op_schedule_stops_at_jovian_for_world() {
        let config = WorldHardforkConfig {
            jovian_time: Some(10),
            tropo_time: Some(20),
            strato_time: Some(30),
            ..Default::default()
        };

        assert_eq!(
            config.op_activation(OpHardfork::Jovian),
            ForkCondition::Timestamp(10)
        );
        assert_eq!(
            config.op_activation(OpHardfork::Karst),
            ForkCondition::Never
        );
        assert_eq!(
            config.op_activation(OpHardfork::Interop),
            ForkCondition::Never
        );
    }

    #[test]
    fn world_specs_map_post_jovian_to_osaka_without_activating_op_karst() {
        assert_eq!(WorldSpecId::TROPO.into_eth_spec(), SpecId::OSAKA);
        assert_eq!(WorldSpecId::STRATO.into_eth_spec(), SpecId::OSAKA);
        assert_eq!(WorldSpecId::TROPO.into_op_revm_spec(), OpSpecId::KARST);
        assert!(WorldSpecId::STRATO.is_enabled_in(WorldSpecId::TROPO));
    }

    #[test]
    fn rollup_config_hash_binds_world_only_fork_times() {
        let before = json!({
            "jovian_time": 10,
            "tropo_time": 20,
            "strato_time": 30
        });
        let after = json!({
            "jovian_time": 10,
            "tropo_time": 21,
            "strato_time": 30
        });

        assert_ne!(
            hash_rollup_config(&before).unwrap(),
            hash_rollup_config(&after).unwrap()
        );
    }

    #[test]
    fn world_rollup_config_hash_appends_world_only_fork_times() {
        let rollup_config = json!({
            "jovian_time": 10
        });
        let no_world_forks = WorldHardforkConfig {
            jovian_time: Some(10),
            ..Default::default()
        };
        let with_world_forks = WorldHardforkConfig {
            jovian_time: Some(10),
            tropo_time: Some(20),
            strato_time: Some(30),
            ..Default::default()
        };

        assert_eq!(
            hash_world_rollup_config(&rollup_config, &no_world_forks).unwrap(),
            hash_rollup_config(&rollup_config).unwrap()
        );
        assert_ne!(
            hash_world_rollup_config(&rollup_config, &no_world_forks).unwrap(),
            hash_world_rollup_config(&rollup_config, &with_world_forks).unwrap()
        );
    }

    #[test]
    fn bytes_hash_matches_pretty_json_hash() {
        let config = json!({"jovian_time": 10, "tropo_time": 20});
        let pretty = serde_json::to_string_pretty(&config).unwrap();

        assert_eq!(
            hash_rollup_config(&config).unwrap(),
            hash_rollup_config_bytes(pretty.as_bytes())
        );
    }
}
