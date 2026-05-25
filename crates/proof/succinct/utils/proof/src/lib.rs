//! SP1-specific proof request types and the `WorldSuccinctProver` backend trait.
//!
//! Proof artifact types (`RangeProofArtifact`, `AggregationProofArtifact`) and all shared
//! primitives live in `world-chain-proof-core`.

use serde::{Deserialize, Serialize};
use world_chain_proof_core::types::AggregationInputs;
use world_chain_proof_succinct_client_utils::WorldRangeWitness;

pub use world_chain_proof_core::artifacts::{AggregationProofArtifact, RangeProofArtifact};

/// Returns the embedded World range ELF.
#[cfg(feature = "embedded-elfs")]
pub fn get_range_elf_embedded() -> &'static [u8] {
    world_chain_proof_succinct_elfs::RANGE_ELF_EMBEDDED
}

/// Returns the embedded World aggregation ELF.
#[cfg(feature = "embedded-elfs")]
pub fn get_aggregation_elf_embedded() -> &'static [u8] {
    world_chain_proof_succinct_elfs::AGGREGATION_ELF
}

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

#[cfg(all(test, feature = "embedded-elfs"))]
mod tests {
    use super::*;

    #[test]
    fn exposes_world_elfs() {
        assert!(get_range_elf_embedded().starts_with(b"\x7fELF"));
        assert!(get_aggregation_elf_embedded().starts_with(b"\x7fELF"));
    }
}
