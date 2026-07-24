//! Proof artifact types returned by World fault-proof backends.

use serde::{Deserialize, Serialize};

use crate::{boot::TransitionPublicValues, types::AggregationPublicValues};

/// Public output and proof bytes returned by a range prover.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeProofArtifact {
    /// Transition public values committed by the guest.
    pub transition_public_values: TransitionPublicValues,
    /// Serialized proof bytes (SP1 proof or attestation document, depending on backend).
    pub proof: Vec<u8>,
}

/// Public output and proof bytes returned by an aggregation prover.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregationProofArtifact {
    /// ABI-compatible public values committed by the aggregation guest.
    pub public_values: AggregationPublicValues,
    /// Serialized proof bytes (SP1 proof or attestation document, depending on backend).
    pub proof: Vec<u8>,
}
