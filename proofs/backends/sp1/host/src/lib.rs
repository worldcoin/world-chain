//! Host-side helpers for preparing World Chain OP Succinct Lite proof requests.

use std::{fmt, time::Duration};

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
#[cfg(feature = "sp1")]
pub mod vkeys;

/// Structured failures specific to all succinct provers; surfaced wrapped in
/// [`anyhow::Error`] so callers can downcast when they need to match on them.
#[derive(Debug, thiserror::Error)]
pub enum SuccinctProverError {
    /// Aggregation requires compressed range proofs for recursive verification.
    #[error("range proof was not in compressed mode")]
    NotCompressed,

    /// The network request expired before a prover accepted it.
    #[error("SP1 Network request {session_id} timed out during the auction")]
    RequestAuctionTimedOut { session_id: String },

    /// The network request exceeded its proof-generation deadline.
    #[error("SP1 Network request {session_id} timed out")]
    RequestTimedOut { session_id: String },

    /// The network determined that the request cannot be executed.
    #[error("SP1 Network request {session_id} is unexecutable")]
    RequestUnexecutable { session_id: String },

    /// The network determined that the request cannot be fulfilled.
    #[error("SP1 Network request {session_id} is unfulfillable")]
    RequestUnfulfillable { session_id: String },
}

impl SuccinctProverError {
    /// Whether a fresh SP1 Network request may recover from this terminal session failure.
    pub const fn should_resubmit(&self) -> bool {
        matches!(
            self,
            Self::RequestAuctionTimedOut { .. } | Self::RequestTimedOut { .. }
        )
    }

    /// Whether the external proving session has reached a terminal state.
    pub const fn is_terminal_session(&self) -> bool {
        matches!(
            self,
            Self::RequestAuctionTimedOut { .. }
                | Self::RequestTimedOut { .. }
                | Self::RequestUnexecutable { .. }
                | Self::RequestUnfulfillable { .. }
        )
    }
}

/// Interface expected from a concrete SP1 prover backend.
#[cfg(feature = "sp1")]
#[async_trait]
pub trait WorldSuccinctProver {
    fn supports_persistent_sessions(&self) -> bool;

    async fn submit(
        &self,
        request: world_chain_proof_sp1_types::Sp1ProofRequest,
    ) -> anyhow::Result<String>;

    async fn poll(
        &self,
        session_id: &str,
    ) -> anyhow::Result<world_chain_proof_sp1_types::Sp1SessionStatus>;

    async fn download(&self, session_id: &str) -> anyhow::Result<SP1ProofWithPublicValues>;

    /// Waits for a submitted session and returns its proof.
    ///
    /// Network provers override this to use the SDK's auction cancellation and request-deadline
    /// handling. Local provers use the generic poll-and-download loop.
    async fn wait(&self, session_id: &str) -> anyhow::Result<SP1ProofWithPublicValues> {
        loop {
            match self.poll(session_id).await? {
                world_chain_proof_sp1_types::Sp1SessionStatus::Running => {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
                world_chain_proof_sp1_types::Sp1SessionStatus::Completed => {
                    return self.download(session_id).await;
                }
                world_chain_proof_sp1_types::Sp1SessionStatus::Failed(reason) => {
                    anyhow::bail!("proof session {session_id} failed: {reason}");
                }
                world_chain_proof_sp1_types::Sp1SessionStatus::NotFound => {
                    anyhow::bail!("proof session {session_id} was not found");
                }
            }
        }
    }
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

#[cfg(all(test, feature = "sp1"))]
mod tests {
    use super::SuccinctProverError;

    #[test]
    fn only_network_timeouts_are_resubmitted() {
        for error in [
            SuccinctProverError::RequestAuctionTimedOut {
                session_id: "auction".to_string(),
            },
            SuccinctProverError::RequestTimedOut {
                session_id: "proof".to_string(),
            },
        ] {
            assert!(error.is_terminal_session());
            assert!(error.should_resubmit());
        }

        for error in [
            SuccinctProverError::RequestUnexecutable {
                session_id: "unexecutable".to_string(),
            },
            SuccinctProverError::RequestUnfulfillable {
                session_id: "unfulfillable".to_string(),
            },
        ] {
            assert!(error.is_terminal_session());
            assert!(!error.should_resubmit());
        }

        assert!(!SuccinctProverError::NotCompressed.is_terminal_session());
        assert!(!SuccinctProverError::NotCompressed.should_resubmit());
    }
}
