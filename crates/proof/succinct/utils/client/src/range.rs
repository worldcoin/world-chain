//! SP1-specific range program types — output root witnesses, the full witness struct, and the
//! range program entry point. Shared public-value types live in `world-chain-proof-core`.

use alloy_primitives::{B256, keccak256};
use serde::{Deserialize, Serialize};

use world_chain_proof_core::{
    boot::BootInfoStruct,
    range::{
        WorldRangeProofInput, WorldRangeProofPublicValues, WorldRangeProofValidationError,
        validate_public_values,
    },
};

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
    use world_chain_proof_core::range::{
        WorldRangeHardforkConfig, WorldRangeProofClaim, WorldRangeSpecId,
    };

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
            run_range_program(WorldRangeWitness::with_expected_public_values(input, expected))
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
