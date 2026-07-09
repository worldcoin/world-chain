use alloy_primitives::{Address, B256};
use serde::{Deserialize, Serialize};
use world_chain_proof_core::{
    boot::BootInfoStruct, types::AggregationInputs, witness::WorldRangeWitnessData,
};

pub use world_chain_proof_core::artifacts::{AggregationProofArtifact, RangeProofArtifact};

// ---------------------------------------------------------------------------
// Proof request types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sp1ProofRequest {
    Range(RangeProofRequest),
    Aggregation(AggregationSessionRequest),
}

/// Host request for a single SP1 range proof.
///
/// Carries the full rkyv-serialized [`WorldRangeWitnessData`] that the range guest reads from
/// stdin, mirroring `NitroRangeProofRequest` in `world-chain-proof-nitro`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeProofRequest {
    /// rkyv-serialized [`WorldRangeWitnessData`] consumed by the range guest.
    pub witness_rkyv: Vec<u8>,
}

impl RangeProofRequest {
    /// Builds a request by rkyv-serializing the supplied witness data.
    pub fn from_witness_data(witness: &WorldRangeWitnessData) -> Result<Self, rkyv::rancor::Error> {
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(witness)?;
        Ok(Self {
            witness_rkyv: bytes.to_vec(),
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

/// Backend-agnostic request for an aggregation proof session.
///
/// Concrete provers inject their own verifying key metadata when converting this into a
/// low-level [`AggregationProofRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregationSessionRequest {
    /// Boot infos committed by the range proofs being aggregated.
    pub boot_infos: Vec<BootInfoStruct>,
    /// Latest L1 checkpoint head committed by the aggregation guest.
    pub latest_l1_checkpoint_head: B256,
    /// Prover address committed by the aggregation guest for on-chain attribution.
    pub prover_address: Address,
    /// CBOR-encoded L1 headers, ordered from oldest to newest.
    pub l1_headers_cbor: Vec<u8>,
    /// Serialized compressed SP1 range proofs, ordered to match `boot_infos`.
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
