//! Aggregation program input and output types.

use alloy_primitives::B256;
use alloy_sol_types::sol;
use serde::{Deserialize, Serialize};

use crate::boot::TransitionPublicValues;

/// Inputs consumed by the aggregation guest program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregationInputs {
    /// Public values from each range proof being aggregated.
    pub transition_public_values: Vec<TransitionPublicValues>,
    /// L1 checkpoint head that the supplied header chain must terminate at.
    pub latest_l1_checkpoint_head: B256,
    /// SP1 verifying key for the range proof program.
    pub multi_block_vkey: [u32; 8],
}

sol! {
    /// ABI-encoded public values committed by the aggregation proof.
    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct AggregationPublicValues {
        TransitionPublicValues transitionPublicValues;
        bytes32 multiBlockVKey;
    }
}

pub use kona_sp1_client_utils::types::u32_to_u8;
