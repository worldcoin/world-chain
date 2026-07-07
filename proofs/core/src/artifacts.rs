//! Proof artifact types returned by World fault-proof backends.

use serde::{Deserialize, Serialize};

use crate::{boot::BootInfoStruct, types::AggregationOutputs};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProofArtifact {
    Range(RangeProofArtifact),
    Aggregation(AggregationProofArtifact),
}

/// Public output and proof bytes returned by a range prover.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeProofArtifact {
    /// OP Succinct-compatible boot info committed by the guest.
    pub boot_info: BootInfoStruct,
    /// Serialized proof bytes (SP1 proof or attestation document, depending on backend).
    pub proof: Vec<u8>,
}

/// Public output and proof bytes returned by an aggregation prover.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregationProofArtifact {
    /// ABI-compatible aggregation outputs committed by the guest.
    pub outputs: AggregationOutputs,
    /// Serialized proof bytes (SP1 proof or attestation document, depending on backend).
    pub proof: Vec<u8>,
}
