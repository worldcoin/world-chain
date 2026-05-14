//! Proof request/artifact types shared by host services and prover adapters.

use serde::{Deserialize, Serialize};
use world_chain_proof_succinct_client_utils::{
    WorldRangeWitness,
    boot::BootInfoStruct,
    types::{AggregationInputs, AggregationOutputs},
};

/// Host request for a single SP1 range proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeProofRequest {
    /// Witness consumed by the range guest.
    pub witness: WorldRangeWitness,
}

/// Host request for an aggregation proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregationProofRequest {
    /// Aggregation inputs consumed by the guest program.
    pub inputs: AggregationInputs,
    /// CBOR-encoded L1 headers, ordered from oldest to newest.
    pub l1_headers_cbor: Vec<u8>,
}

/// Public output and proof bytes returned by a range prover.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeProofArtifact {
    /// OP Succinct-compatible boot info committed by the guest.
    pub boot_info: BootInfoStruct,
    /// Serialized SP1 proof bytes.
    pub proof: Vec<u8>,
}

/// Public output and proof bytes returned by an aggregation prover.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregationProofArtifact {
    /// ABI-compatible aggregation outputs committed by the guest.
    pub outputs: AggregationOutputs,
    /// Serialized SP1 proof bytes.
    pub proof: Vec<u8>,
}

/// Interface expected from a concrete SP1 prover backend.
pub trait WorldSuccinctProver {
    /// Backend-specific error type.
    type Error;

    /// Proves one range witness.
    fn prove_range(&self, request: RangeProofRequest) -> Result<RangeProofArtifact, Self::Error>;

    /// Aggregates already-generated range proofs.
    fn prove_aggregation(
        &self,
        request: AggregationProofRequest,
    ) -> Result<AggregationProofArtifact, Self::Error>;
}
