//! AWS Nitro TEE-attested prover for the World Chain OP Succinct Lite fault-proof stack.
//!
//! The Succinct backend produces ZK proofs of `BootInfoStruct` by re-executing the OP Stack
//! derivation pipeline inside an SP1 zkVM. This crate offers an alternative trust assumption:
//! the same derivation pipeline runs unmodified inside an AWS Nitro Enclave, and the resulting
//! `BootInfoStruct` is attested by the enclave's NSM device. Verifiers check the attestation
//! document instead of a ZK proof.
//!
//! The boundary mirrors [`world_chain_proof_succinct_proof_utils::WorldSuccinctProver`]:
//!
//! - [`NitroRangeProofRequest`] carries the full rkyv-serialized [`WorldRangeWitnessData`]
//!   that the enclave needs to drive the derivation pipeline.
//! - [`NitroRangeProofArtifact`] returns the committed [`BootInfoStruct`] plus the raw
//!   NSM attestation document (`COSE_Sign1` bytes) the host can hand to any verifier.
//! - [`WorldTeeProver`] is the analogue of `WorldSuccinctProver` for attestation-backed
//!   backends.
//!
//! Module layout:
//!
//! - [`protocol`] — wire types exchanged with the enclave (vsock framing, request enums,
//!   `user_data` derivation).
//! - [`attestation`] — verification helpers used host-side to check PCR/`user_data` fields
//!   on an attestation document.
//! - [`host`] — `NitroProver` implementation that talks to a running enclave over vsock.
//!
//! The enclave-side guest is the `world-chain-nitro-enclave` binary (`src/enclave.rs`).

use serde::{Deserialize, Serialize};
use world_chain_proof_core::{
    artifacts::AggregationProofArtifact,
    boot::BootInfoStruct,
    range::WorldRangeProofPublicValues,
    types::AggregationInputs,
    witness::WorldRangeWitnessData,
};

pub mod attestation;
#[cfg(feature = "aws_nitro")]
pub mod host;
pub mod protocol;

#[cfg(feature = "enclave")]
pub mod enclave_lib;

#[cfg(feature = "aws_nitro")]
pub use host::{NitroProver, NitroProverError};

/// Length, in bytes, of a Nitro PCR slot value (SHA-384 digest).
pub const PCR_LEN: usize = 48;

/// PCR digest committed by the Nitro enclave (PCR0/PCR1/PCR2 are SHA-384 = 48 bytes).
pub type PcrDigest = [u8; PCR_LEN];

/// Expected PCR values that pin the enclave image a verifier is willing to trust.
///
/// PCR0 covers the EIF (kernel + ramdisk + bootstrap), PCR1 covers the kernel + bootstrap on
/// their own, PCR2 covers the application code. Real values are produced at build time by
/// `nitro-cli build-enclave`; the constants below are placeholders that callers MUST replace
/// before any production use.
//
// TODO(nitro): replace with real PCR values produced by `nitro-cli build-enclave` once we
// have a reproducible EIF build for the world-chain-nitro-enclave image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpectedPcrs {
    /// EIF measurement.
    pub pcr0: PcrDigest,
    /// Kernel + bootstrap measurement.
    pub pcr1: PcrDigest,
    /// Application measurement.
    pub pcr2: PcrDigest,
}

impl ExpectedPcrs {
    /// All-zero placeholder PCRs. Useful for unit tests and as the default for the WIP
    /// integration; production callers MUST override these with real measurements.
    pub const PLACEHOLDER: Self = Self {
        pcr0: [0u8; PCR_LEN],
        pcr1: [0u8; PCR_LEN],
        pcr2: [0u8; PCR_LEN],
    };
}

impl Default for ExpectedPcrs {
    fn default() -> Self {
        Self::PLACEHOLDER
    }
}

/// Host request for a Nitro-attested range proof.
///
/// Unlike the Succinct equivalent, the Nitro prover needs the full execution witness
/// (preimages, blobs, World fork schedule) because the enclave re-runs the derivation
/// pipeline end-to-end and only returns an attested `BootInfoStruct`.
#[derive(Clone, Debug)]
pub struct NitroRangeProofRequest {
    /// rkyv-serialized [`WorldRangeWitnessData`].
    ///
    /// The host has already constructed and validated this witness; the enclave only needs
    /// the bytes since deserialization happens inside the enclave to keep the witness
    /// inside the attested execution scope.
    pub witness_rkyv: Vec<u8>,
    /// Optional host-computed public values that the enclave must match before returning a
    /// signed boot info struct. Mirrors `WorldRangeWitness::expected_public_values`.
    pub expected_public_values: Option<WorldRangeProofPublicValues>,
}

impl NitroRangeProofRequest {
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

/// Artifact returned by a Nitro range prover.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NitroRangeProofArtifact {
    /// OP Succinct-compatible boot info committed by the enclave.
    pub boot_info: BootInfoStruct,
    /// `COSE_Sign1` attestation document bytes returned by the Nitro NSM device.
    ///
    /// The document's `user_data` field commits to [`protocol::range_user_data`] of the boot
    /// info, binding the attestation to this specific transition.
    pub attestation_doc: Vec<u8>,
}

/// Host request for a Nitro-attested aggregation proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NitroAggregationProofRequest {
    /// Same aggregation inputs as the Succinct backend.
    pub inputs: AggregationInputs,
    /// CBOR-encoded L1 headers, ordered from oldest to newest.
    pub l1_headers_cbor: Vec<u8>,
}

/// Artifact returned by a Nitro aggregation prover. The shape mirrors
/// [`AggregationProofArtifact`] but the `proof` bytes hold an attestation document rather
/// than an SP1 proof.
pub type NitroAggregationProofArtifact = AggregationProofArtifact;

/// Backend trait for TEE-attested World prover implementations.
///
/// Modeled after [`world_chain_proof_succinct_proof_utils::WorldSuccinctProver`], but the
/// request and artifact types carry attestation material instead of ZK proofs.
pub trait WorldTeeProver {
    /// Backend-specific error type.
    type Error;

    /// Proves one range witness inside an attested execution environment.
    fn prove_range(
        &self,
        request: NitroRangeProofRequest,
    ) -> Result<NitroRangeProofArtifact, Self::Error>;

    /// Aggregates already-attested range proofs.
    fn prove_aggregation(
        &self,
        request: NitroAggregationProofRequest,
    ) -> Result<NitroAggregationProofArtifact, Self::Error>;
}

/// Convenience hash used to bind boot info into the attestation `user_data` field.
///
/// `SHA256(l2_pre_root || l2_post_root || l2_block_number_be || rollup_config_hash)`
#[must_use]
pub fn range_user_data(boot_info: &BootInfoStruct) -> [u8; 32] {
    protocol::range_user_data(boot_info)
}

/// Re-exports of common host-facing types so callers can do `use world_chain_proof_nitro::*`.
pub mod prelude {
    #[cfg(feature = "aws_nitro")]
    pub use crate::{NitroProver, NitroProverError};
    pub use crate::{
        ExpectedPcrs, NitroAggregationProofArtifact, NitroAggregationProofRequest,
        NitroRangeProofArtifact, NitroRangeProofRequest, WorldTeeProver, range_user_data,
    };
}

#[cfg(test)]
mod tests {
    use alloy_primitives::B256;
    use world_chain_proof_core::boot::BootInfoStruct;

    use crate::{
        ExpectedPcrs, NitroRangeProofArtifact, PCR_LEN,
        attestation::{AttestationError, verify_attestation_doc},
        protocol::range_user_data,
    };

    /// Builds a minimal synthetic COSE_Sign1 attestation document suitable for unit tests.
    /// The signature bytes are a 96-byte placeholder — no cryptographic verification is
    /// performed by `parse_attestation_doc` / `verify_attestation_doc` (see the TODO in
    /// `attestation.rs`).
    fn make_attestation_doc(pcrs: &[[u8; PCR_LEN]; 3], user_data: &[u8]) -> Vec<u8> {
        let pcr_map: Vec<(ciborium::value::Value, ciborium::value::Value)> = pcrs
            .iter()
            .enumerate()
            .map(|(idx, bytes)| {
                (
                    ciborium::value::Value::Integer((idx as i128).try_into().unwrap()),
                    ciborium::value::Value::Bytes(bytes.to_vec()),
                )
            })
            .collect();

        let entries: Vec<(ciborium::value::Value, ciborium::value::Value)> = vec![
            (
                ciborium::value::Value::Text("pcrs".into()),
                ciborium::value::Value::Map(pcr_map),
            ),
            (
                ciborium::value::Value::Text("user_data".into()),
                ciborium::value::Value::Bytes(user_data.to_vec()),
            ),
            (
                ciborium::value::Value::Text("module_id".into()),
                ciborium::value::Value::Text("test-enclave".into()),
            ),
            (
                ciborium::value::Value::Text("digest".into()),
                ciborium::value::Value::Text("SHA384".into()),
            ),
        ];

        let mut payload_bytes = Vec::new();
        ciborium::into_writer(&ciborium::value::Value::Map(entries), &mut payload_bytes).unwrap();

        let cose = ciborium::value::Value::Array(vec![
            ciborium::value::Value::Bytes(vec![]),
            ciborium::value::Value::Map(vec![]),
            ciborium::value::Value::Bytes(payload_bytes),
            ciborium::value::Value::Bytes(vec![0u8; 96]),
        ]);
        let mut out = Vec::new();
        ciborium::into_writer(&cose, &mut out).unwrap();
        out
    }

    fn boot_info() -> BootInfoStruct {
        BootInfoStruct {
            l1Head: B256::from([1; 32]),
            l2PreRoot: B256::from([2; 32]),
            l2PostRoot: B256::from([3; 32]),
            l2BlockNumber: 42,
            rollupConfigHash: B256::from([4; 32]),
        }
    }

    #[test]
    fn range_artifact_user_data_binds_boot_info() {
        let boot_info = boot_info();
        let user_data = range_user_data(&boot_info);
        let attestation_doc = make_attestation_doc(&[[0u8; PCR_LEN]; 3], &user_data);

        let artifact = NitroRangeProofArtifact { boot_info, attestation_doc };

        let expected_user_data = range_user_data(&artifact.boot_info);
        verify_attestation_doc(
            &artifact.attestation_doc,
            &ExpectedPcrs::PLACEHOLDER,
            &expected_user_data,
        )
        .unwrap();
    }

    #[test]
    fn tampered_boot_info_fails_user_data_check() {
        let boot_info = boot_info();
        let user_data = range_user_data(&boot_info);
        let attestation_doc = make_attestation_doc(&[[0u8; PCR_LEN]; 3], &user_data);

        let mut tampered = boot_info;
        tampered.l2PostRoot = B256::from([9; 32]);

        let expected_user_data = range_user_data(&tampered);
        let err = verify_attestation_doc(
            &attestation_doc,
            &ExpectedPcrs::PLACEHOLDER,
            &expected_user_data,
        )
        .unwrap_err();

        assert!(matches!(err, AttestationError::UserDataMismatch { .. }));
    }

    #[test]
    fn wrong_pcrs_fail_verification() {
        let boot_info = boot_info();
        let user_data = range_user_data(&boot_info);
        // Document has all-zero PCRs but we verify against all-ones.
        let attestation_doc = make_attestation_doc(&[[0u8; PCR_LEN]; 3], &user_data);

        let wrong_pcrs = ExpectedPcrs {
            pcr0: [1u8; PCR_LEN],
            pcr1: [0u8; PCR_LEN],
            pcr2: [0u8; PCR_LEN],
        };
        let err =
            verify_attestation_doc(&attestation_doc, &wrong_pcrs, &user_data).unwrap_err();

        assert!(matches!(err, AttestationError::PcrMismatch { index: 0, .. }));
    }
}
