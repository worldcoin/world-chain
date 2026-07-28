//! Host-side helpers for preparing World Chain OP Succinct Lite proof requests.

use std::fmt;

#[cfg(feature = "sp1")]
use anyhow::Context;
#[cfg(feature = "sp1")]
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
#[cfg(feature = "sp1")]
use sp1_sdk::{SP1Proof, SP1ProofWithPublicValues};
use strum::EnumString;
#[cfg(feature = "sp1")]
use world_chain_proof_core::{
    artifacts::{AggregationProofArtifact, RangeProofArtifact},
    boot::TransitionPublicValues,
    types::AggregationPublicValues,
};

#[cfg(feature = "sp1")]
pub mod cpu_prover;
#[cfg(feature = "sp1")]
pub mod mock_prover;
#[cfg(feature = "sp1")]
pub mod network_prover;
#[cfg(feature = "sp1")]
pub mod validity;

/// Structured failures specific to all succinct provers; surfaced wrapped in
/// [`anyhow::Error`] so callers can downcast when they need to match on them.
#[derive(Debug, thiserror::Error)]
pub enum SuccinctProverError {
    /// Aggregation requires compressed range proofs for recursive verification.
    #[error("range proof was not in compressed mode")]
    NotCompressed,
}

/// Interface expected from a concrete SP1 prover backend.
#[cfg(feature = "sp1")]
#[async_trait]
pub trait WorldSuccinctProver {
    fn supports_persistent_sessions(&self) -> bool;

    async fn submit(
        &self,
        request: world_chain_proof_succinct_utils::Sp1ProofRequest,
    ) -> anyhow::Result<String>;

    async fn poll(
        &self,
        session_id: &str,
    ) -> anyhow::Result<world_chain_proof_succinct_utils::Sp1SessionStatus>;

    async fn download(&self, session_id: &str) -> anyhow::Result<SP1ProofWithPublicValues>;
}

/// Converts a raw compressed SP1 range proof into the artifact consumed by aggregation.
#[cfg(feature = "sp1")]
pub fn range_artifact_from_sp1_proof(
    proof: &SP1ProofWithPublicValues,
) -> anyhow::Result<RangeProofArtifact> {
    let transition_public_values: TransitionPublicValues =
        bincode::deserialize(proof.public_values.as_slice())
            .context("range proof public values deserialization failed")?;

    let SP1Proof::Compressed(_) = &proof.proof else {
        return Err(SuccinctProverError::NotCompressed.into());
    };

    let proof_bytes = bincode::serialize(proof).context("range proof serialization failed")?;

    Ok(RangeProofArtifact {
        transition_public_values,
        proof: proof_bytes,
    })
}

/// Converts a raw SP1 aggregation proof into the artifact submitted on-chain.
#[cfg(feature = "sp1")]
pub fn aggregation_artifact_from_sp1_proof(
    proof: &SP1ProofWithPublicValues,
) -> anyhow::Result<AggregationProofArtifact> {
    let public_values = <AggregationPublicValues as alloy_sol_types::SolValue>::abi_decode(
        proof.public_values.as_slice(),
    )
    .context("aggregation public values abi decoding failed")?;

    // Groth16/Plonk proofs serialize to their on-chain calldata representation; other
    // modes (mock runs, compressed) keep the full sdk proof for offline use.
    let proof_bytes = match &proof.proof {
        SP1Proof::Groth16(_) | SP1Proof::Plonk(_) => proof.bytes(),
        _ => bincode::serialize(proof).context("aggregation proof serialization failed")?,
    };

    Ok(AggregationProofArtifact {
        public_values,
        proof: proof_bytes,
    })
}

/// SP1 proving backend selected by binaries and dev tooling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, EnumString)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case", ascii_case_insensitive)]
pub enum Sp1ProverKind {
    /// Local CPU prover.
    Cpu,
    /// Local mock prover.
    Mock,
    /// Succinct proving network.
    Network,
}

impl Sp1ProverKind {
    /// Stable CLI/env representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Mock => "mock",
            Self::Network => "network",
        }
    }
}

impl fmt::Display for Sp1ProverKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
