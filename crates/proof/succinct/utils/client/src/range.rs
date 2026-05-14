//! Range-program public-value construction shared by the host and SP1 guest.

use serde::{Deserialize, Serialize};
use world_chain_proof_client::{
    WorldProofInput, WorldProofPublicValues, WorldProofValidationError, validate_public_values,
};

use crate::boot::BootInfoStruct;

/// Witness shape read by the World range guest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldRangeWitness {
    /// World proof input used to construct the committed boot values.
    pub input: WorldProofInput,
    /// Optional host-computed public values. When present, the guest checks them before committing.
    pub expected_public_values: Option<WorldProofPublicValues>,
}

impl WorldRangeWitness {
    /// Creates a range witness without a separate expected public-value copy.
    pub const fn new(input: WorldProofInput) -> Self {
        Self {
            input,
            expected_public_values: None,
        }
    }

    /// Creates a range witness with explicit host-computed public values to validate.
    pub const fn with_expected_public_values(
        input: WorldProofInput,
        expected_public_values: WorldProofPublicValues,
    ) -> Self {
        Self {
            input,
            expected_public_values: Some(expected_public_values),
        }
    }
}

/// Error returned by the range public-value wrapper.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RangeProgramError {
    /// Host supplied expected values that do not match the witness.
    #[error(transparent)]
    InvalidPublicValues(#[from] WorldProofValidationError),
}

/// Runs the World range-program wrapper and returns OP Succinct-compatible boot values.
pub fn run_range_program(witness: WorldRangeWitness) -> Result<BootInfoStruct, RangeProgramError> {
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
    use world_chain_proof_client::{WorldProofClaim, WorldProofInput};
    use world_chain_proof_protocol::{WorldHardforkConfig, WorldSpecId};

    fn input() -> WorldProofInput {
        WorldProofInput {
            schedule: WorldHardforkConfig {
                jovian_time: Some(10),
                tropo_time: Some(20),
                strato_time: Some(30),
                ..Default::default()
            },
            claim: WorldProofClaim {
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

        assert_eq!(expected.world_spec_id, WorldSpecId::STRATO);
        assert!(
            run_range_program(WorldRangeWitness::with_expected_public_values(
                input, expected
            ))
            .is_ok()
        );
    }
}
