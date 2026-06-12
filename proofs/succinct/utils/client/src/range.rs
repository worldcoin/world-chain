use world_chain_proof_core::{
    boot::BootInfoStruct,
    range::{WorldRangeProofValidationError, validate_public_values},
};
use world_chain_proof_kona_utils::WorldRangeWitness;

/// Error returned by the range public-value wrapper.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RangeProgramError {
    /// Host supplied expected values that do not match the witness.
    #[error(transparent)]
    InvalidPublicValues(#[from] WorldRangeProofValidationError),
    /// The pre-state output root does not match the supplied output-root witness.
    #[error("pre-state output root mismatch: expected {expected:?}, got {actual:?}")]
    PreStateOutputRoot {
        expected: alloy_primitives::B256,
        actual: alloy_primitives::B256,
    },
    /// The post-state output root does not match the supplied output-root witness.
    #[error("post-state output root mismatch: expected {expected:?}, got {actual:?}")]
    PostStateOutputRoot {
        expected: alloy_primitives::B256,
        actual: alloy_primitives::B256,
    },
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
