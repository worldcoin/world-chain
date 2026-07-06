use alloy_primitives::B256;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use world_chain_proof_core::{
    range::WorldRangeProofPublicValues, types::AggregationInputs, witness::WorldRangeWitnessData,
};

pub use world_chain_proof_core::artifacts::{AggregationProofArtifact, RangeProofArtifact};

// ---------------------------------------------------------------------------
// Proof request types and prover trait
// ---------------------------------------------------------------------------

/// Host request for a single SP1 range proof.
///
/// Carries the full rkyv-serialized [`WorldRangeWitnessData`] that the range guest reads from
/// stdin, mirroring `NitroRangeProofRequest` in `world-chain-proof-nitro`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeProofRequest {
    /// rkyv-serialized [`WorldRangeWitnessData`] consumed by the range guest.
    pub witness_rkyv: Vec<u8>,
    /// Optional host-computed public values checked against the guest commitment.
    pub expected_public_values: Option<WorldRangeProofPublicValues>,
}

impl RangeProofRequest {
    /// Builds a request by rkyv-serializing the supplied witness data.
    pub fn from_witness_data(
        witness: &WorldRangeWitnessData,
        expected_public_values: Option<WorldRangeProofPublicValues>,
    ) -> Result<Self, rkyv::rancor::Error> {
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(witness)?;
        Ok(Self {
            witness_rkyv: bytes.to_vec(),
            expected_public_values,
        })
    }
}

/// Host request for an aggregation proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregationProofRequest {
    /// Aggregation inputs consumed by the guest program.
    pub inputs: AggregationInputs,
    /// CBOR-encoded L1 headers, ordered from oldest to newest.
    pub l1_headers_cbor: Vec<u8>,
    /// Serialized compressed SP1 range proofs, ordered to match `inputs.boot_infos`.
    ///
    /// Each entry is the backend-serialized range proof returned in
    /// [`RangeProofArtifact::proof`]; the aggregation guest recursively verifies them.
    pub range_proofs: Vec<Vec<u8>>,
}

/// Interface expected from a concrete SP1 prover backend.
#[async_trait]
pub trait WorldSuccinctProver {
    /// Backend-specific error type.
    type Error;

    /// 8-word hash of the range program verifying key, as committed by the aggregation guest.
    fn multi_block_vkey(&self) -> [u32; 8];

    /// Proves one range witness.
    async fn prove_range(
        &self,
        request: RangeProofRequest,
    ) -> Result<RangeProofArtifact, Self::Error>;

    /// Aggregates already-generated range proofs.
    async fn prove_aggregation(
        &self,
        request: AggregationProofRequest,
    ) -> Result<AggregationProofArtifact, Self::Error>;

    /// Whether this prover can create durable external proof requests.
    fn supports_async_requests(&self) -> bool {
        false
    }

    /// Request a range proof from an external backend without waiting for completion.
    async fn request_range(&self, request: RangeProofRequest) -> Result<B256, Self::Error>;

    /// Poll a previously requested range proof.
    async fn poll_range(&self, id: B256) -> Result<Option<RangeProofArtifact>, Self::Error>;

    /// Request an aggregation proof from an external backend without waiting for completion.
    async fn request_aggregation(
        &self,
        request: AggregationProofRequest,
    ) -> Result<B256, Self::Error>;

    /// Poll a previously requested aggregation proof.
    async fn poll_aggregation(
        &self,
        id: B256,
    ) -> Result<Option<AggregationProofArtifact>, Self::Error>;
}
