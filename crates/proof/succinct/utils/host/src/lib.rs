//! Host-side helpers for preparing World Chain OP Succinct Lite proof requests.

pub mod witness_generation;

use alloy_primitives::{Address, B256};
use serde::{Deserialize, Serialize};
use world_chain_proof_client::{WorldProofInput, WorldProofPublicValues};
use world_chain_proof_host::{RangeSplitCount, WorldFaultProofServiceConfig};
use world_chain_proof_protocol::{WorldChainHardfork, WorldSpecId};
use world_chain_proof_succinct_client_utils::{
    WorldRangeHardfork, WorldRangeHardforkConfig, WorldRangeProofClaim, WorldRangeProofInput,
    WorldRangeProofPublicValues, WorldRangeSpecId, WorldRangeWitness,
    boot::{BootInfoPublicValues, BootInfoStruct},
    types::AggregationInputs,
};
use world_chain_proof_succinct_proof_utils::{AggregationProofRequest, RangeProofRequest};

/// Host request construction error.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorldSuccinctHostError {
    /// The range bounds are invalid.
    #[error("range end block {end} must be greater than start block {start}")]
    InvalidRange { start: u64, end: u64 },
    /// The witness rollup config hash does not match the configured dispute game.
    #[error("rollup config hash mismatch: expected {expected:?}, got {actual:?}")]
    RollupConfigHashMismatch { expected: B256, actual: B256 },
}

/// Half-open L2 block range `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct L2BlockRange {
    /// First L2 block in the range.
    pub start: u64,
    /// One past the last L2 block in the range.
    pub end: u64,
}

impl L2BlockRange {
    /// Creates a validated half-open L2 block range.
    pub const fn new(start: u64, end: u64) -> Result<Self, WorldSuccinctHostError> {
        if end <= start {
            return Err(WorldSuccinctHostError::InvalidRange { start, end });
        }
        Ok(Self { start, end })
    }

    /// Number of blocks in the range.
    pub const fn len(self) -> u64 {
        self.end - self.start
    }
}

/// World Succinct host request builder.
#[derive(Clone, Debug)]
pub struct WorldSuccinctHost {
    /// Fault-proof service config.
    pub config: WorldFaultProofServiceConfig,
}

impl WorldSuccinctHost {
    /// Creates a request builder.
    pub const fn new(config: WorldFaultProofServiceConfig) -> Self {
        Self { config }
    }

    /// Builds a range proof request and checks the witness binds to this host identity.
    pub fn range_request(
        &self,
        input: WorldProofInput,
        expected_public_values: Option<WorldProofPublicValues>,
    ) -> Result<RangeProofRequest, WorldSuccinctHostError> {
        let expected_hash = self.config.identity().rollup_config_hash;
        if input.rollup_config_hash != expected_hash {
            return Err(WorldSuccinctHostError::RollupConfigHashMismatch {
                expected: expected_hash,
                actual: input.rollup_config_hash,
            });
        }

        Ok(RangeProofRequest {
            witness: WorldRangeWitness {
                input: range_proof_input(input),
                pre_state: None,
                post_state: None,
                expected_public_values: expected_public_values.map(range_public_values),
            },
        })
    }

    /// Builds an aggregation proof request from range boot infos and CBOR-encoded L1 headers.
    pub fn aggregation_request(
        &self,
        boot_infos: Vec<BootInfoStruct>,
        latest_l1_checkpoint_head: B256,
        multi_block_vkey: [u32; 8],
        prover_address: Address,
        l1_headers_cbor: Vec<u8>,
    ) -> AggregationProofRequest {
        AggregationProofRequest {
            inputs: AggregationInputs {
                boot_infos,
                latest_l1_checkpoint_head,
                multi_block_vkey,
                prover_address,
            },
            l1_headers_cbor,
        }
    }

    /// Splits a half-open range according to the configured range split count.
    pub fn split_range(
        &self,
        range: L2BlockRange,
    ) -> Result<Vec<L2BlockRange>, WorldSuccinctHostError> {
        let split_count = self.config.range_split_count.get() as u64;
        split_range(range, split_count)
    }
}

fn range_proof_input(input: WorldProofInput) -> WorldRangeProofInput {
    WorldRangeProofInput {
        schedule: WorldRangeHardforkConfig {
            bedrock_block: input.schedule.bedrock_block,
            regolith_time: input.schedule.regolith_time,
            canyon_time: input.schedule.canyon_time,
            ecotone_time: input.schedule.ecotone_time,
            fjord_time: input.schedule.fjord_time,
            granite_time: input.schedule.granite_time,
            holocene_time: input.schedule.holocene_time,
            isthmus_time: input.schedule.isthmus_time,
            jovian_time: input.schedule.jovian_time,
            tropo_time: input.schedule.tropo_time,
            strato_time: input.schedule.strato_time,
        },
        claim: WorldRangeProofClaim {
            l1_head: input.claim.l1_head,
            agreed_l2_output_root: input.claim.agreed_l2_output_root,
            claimed_l2_output_root: input.claim.claimed_l2_output_root,
            claimed_l2_block_number: input.claim.claimed_l2_block_number,
        },
        claimed_l2_timestamp: input.claimed_l2_timestamp,
        rollup_config_hash: input.rollup_config_hash,
    }
}

fn range_public_values(values: WorldProofPublicValues) -> WorldRangeProofPublicValues {
    WorldRangeProofPublicValues {
        boot_info: BootInfoPublicValues {
            l1_head: values.boot_info.l1_head,
            l2_pre_root: values.boot_info.l2_pre_root,
            l2_post_root: values.boot_info.l2_post_root,
            l2_block_number: values.boot_info.l2_block_number,
            rollup_config_hash: values.boot_info.rollup_config_hash,
        },
        active_fork: range_hardfork(values.active_fork),
        world_spec_id: range_spec_id(values.world_spec_id),
    }
}

fn range_hardfork(hardfork: WorldChainHardfork) -> WorldRangeHardfork {
    match hardfork {
        WorldChainHardfork::Bedrock => WorldRangeHardfork::Bedrock,
        WorldChainHardfork::Regolith => WorldRangeHardfork::Regolith,
        WorldChainHardfork::Canyon => WorldRangeHardfork::Canyon,
        WorldChainHardfork::Ecotone => WorldRangeHardfork::Ecotone,
        WorldChainHardfork::Fjord => WorldRangeHardfork::Fjord,
        WorldChainHardfork::Granite => WorldRangeHardfork::Granite,
        WorldChainHardfork::Holocene => WorldRangeHardfork::Holocene,
        WorldChainHardfork::Isthmus => WorldRangeHardfork::Isthmus,
        WorldChainHardfork::Jovian => WorldRangeHardfork::Jovian,
        WorldChainHardfork::Tropo => WorldRangeHardfork::Tropo,
        WorldChainHardfork::Strato => WorldRangeHardfork::Strato,
        _ => WorldRangeHardfork::Strato,
    }
}

fn range_spec_id(spec_id: WorldSpecId) -> WorldRangeSpecId {
    match spec_id {
        WorldSpecId::BEDROCK => WorldRangeSpecId::BEDROCK,
        WorldSpecId::REGOLITH => WorldRangeSpecId::REGOLITH,
        WorldSpecId::CANYON => WorldRangeSpecId::CANYON,
        WorldSpecId::ECOTONE => WorldRangeSpecId::ECOTONE,
        WorldSpecId::FJORD => WorldRangeSpecId::FJORD,
        WorldSpecId::GRANITE => WorldRangeSpecId::GRANITE,
        WorldSpecId::HOLOCENE => WorldRangeSpecId::HOLOCENE,
        WorldSpecId::ISTHMUS => WorldRangeSpecId::ISTHMUS,
        WorldSpecId::JOVIAN => WorldRangeSpecId::JOVIAN,
        WorldSpecId::TROPO => WorldRangeSpecId::TROPO,
        WorldSpecId::STRATO => WorldRangeSpecId::STRATO,
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

/// Returns the split count as a nonzero u64.
pub const fn split_count_u64(split_count: RangeSplitCount) -> u64 {
    split_count.get() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

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
