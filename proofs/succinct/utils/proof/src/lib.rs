use alloy_primitives::B256;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use world_chain_proof_core::{
    artifacts::ProofArtifact, range::WorldRangeProofPublicValues, types::AggregationInputs,
    witness::WorldRangeWitnessData,
};

pub use world_chain_proof_core::artifacts::{AggregationProofArtifact, RangeProofArtifact};
use world_chain_prover_service::{ProofRequestId, SessionType};

// ---------------------------------------------------------------------------
// Proof request types and prover trait
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProofRequest {
    Range(RangeProofRequest),
    Aggregation(AggregationProofRequest),
}
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

/// Current status of a backend proving session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sp1SessionStatus {
    /// The backend session is still running.
    Running,
    /// The backend session completed successfully and the proof can be downloaded.
    Completed,
    /// The backend session failed with the given reason.
    Failed(String),
    /// The backend has no record of the session id.
    NotFound,
}

/// Interface expected from a concrete SP1 prover backend.
#[async_trait]
pub trait WorldSuccinctProver {
    // TODO: change the error type in all methods, temporarily defaulting to anyhow String

    fn supports_persistent_sessions(&self) -> bool;

    async fn submit(
        &self,
        proof_id: ProofRequestId,
        session_type: SessionType,
        request: ProofRequest,
    ) -> anyhow::Result<String>;

    async fn poll(
        &self,
        session_id: String,
        session_type: SessionType,
    ) -> anyhow::Result<Sp1SessionStatus>;

    async fn download(
        &self,
        session_id: String,
        session_type: SessionType,
    ) -> anyhow::Result<ProofArtifact>;
}
