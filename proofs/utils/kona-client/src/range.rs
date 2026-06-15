use alloy_primitives::{B256, keccak256};
use serde::{Deserialize, Serialize};

use world_chain_proof_core::range::{WorldRangeProofInput, WorldRangeProofPublicValues};

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
