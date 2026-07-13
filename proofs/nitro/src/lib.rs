#![cfg_attr(not(test), warn(unused_crate_dependencies))]
//! AWS Nitro TEE-attested prover for the World Chain OP Succinct Lite fault-proof stack.
//!
//! The Succinct backend produces ZK proofs of `BootInfoStruct` by re-executing the OP Stack
//! derivation pipeline inside an SP1 zkVM. This crate offers an alternative trust assumption:
//! the same derivation pipeline runs unmodified inside an AWS Nitro Enclave, and the resulting
//! `BootInfoStruct` is attested by the enclave's NSM device. Verifiers check the attestation
//! document instead of a ZK proof.
//!
//! - [`NitroRangeProofRequest`] carries the full rkyv-serialized [`WorldRangeWitnessData`]
//!   that the enclave needs to drive the derivation pipeline.
//! - [`NitroRangeProofArtifact`] returns the committed [`BootInfoStruct`] plus the raw
//!   NSM attestation document (`COSE_Sign1` bytes) the host can hand to any verifier.
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

// clap is used by the p384-hints binary; reference it here so the
// `unused_crate_dependencies` lint does not fire on the lib target.
use clap as _;

use serde::{Deserialize, Serialize};
use world_chain_proof_core::{
    boot::BootInfoStruct, range::WorldRangeProofPublicValues, witness::WorldRangeWitnessData,
};

// Used only by the feature-gated `enclave` module; bind with `as _`
// so the default build doesn't trip `unused_crate_dependencies`.
use anyhow as _;
// `alloy-primitives` is used by the `enclave` host module and the test modules; bind it
// here so the default non-test lib build doesn't trip `unused_crate_dependencies`.
use alloy_primitives as _;
use k256 as _;
use tracing as _;
// `tracing-subscriber` is only used by the `enclave`/`test-attestation` bins, not the
// lib; bind it here under the feature that pulls it in so the lib target doesn't trip
// `unused_crate_dependencies`.
#[cfg(all(feature = "enclave", target_os = "linux"))]
use tracing_subscriber as _;

pub mod attestation;

/// P-384 modular-inverse hint generator for on-chain hinted ECDSA384 verification.
/// See [`p384_hints::collect_hints`] for the primary entry point.
pub mod p384_hints;

#[cfg(all(feature = "enclave", target_os = "linux"))]
pub mod host;
pub mod protocol;

#[cfg(all(feature = "enclave", target_os = "linux"))]
pub mod enclave;

#[cfg(all(feature = "enclave", target_os = "linux"))]
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

    /// Returns `true` if all three PCRs are all-zero (i.e., the placeholder value).
    ///
    /// Callers can use this to skip attestation verification in dev/test environments
    /// where real PCR measurements are not available. **Never use placeholder PCRs in
    /// production.**
    #[must_use]
    pub fn is_placeholder(&self) -> bool {
        self.pcr0.iter().all(|&b| b == 0)
            && self.pcr1.iter().all(|&b| b == 0)
            && self.pcr2.iter().all(|&b| b == 0)
    }
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
    /// 65-byte recoverable secp256k1 signature over
    /// `keccak256(l2_post_root || l2_block_number_be || rollup_config_hash)`.
    ///
    /// Produced by the enclave's ephemeral signing key, which is certified by the NSM
    /// attestation document. Enables EVM-native on-chain signature recovery.
    pub signature: Vec<u8>,
}

/// Convenience hash used to bind boot info into the attestation `user_data` field.
///
/// `SHA256(l2_pre_root || l2_post_root || l2_block_number_be || rollup_config_hash)`
#[must_use]
pub fn range_user_data(boot_info: &BootInfoStruct) -> [u8; 32] {
    protocol::range_user_data(boot_info)
}

/// Convenience re-export of the enclave signing commitment.
///
/// `keccak256(l2_post_root || l2_block_number_be || rollup_config_hash)`
#[must_use]
pub fn signing_commitment(boot_info: &BootInfoStruct) -> [u8; 32] {
    protocol::signing_commitment(boot_info)
}

/// Re-exports of common host-facing types so callers can do `use world_chain_proof_nitro::*`.
pub mod prelude {
    pub use crate::{
        ExpectedPcrs, NitroRangeProofArtifact, NitroRangeProofRequest, range_user_data,
        signing_commitment,
    };
    #[cfg(all(feature = "enclave", target_os = "linux"))]
    pub use crate::{NitroProver, NitroProverError};
}

#[cfg(test)]
mod tests {
    use alloy_primitives::B256;
    use world_chain_proof_core::boot::BootInfoStruct;

    use crate::{
        ExpectedPcrs, NitroRangeProofArtifact, PCR_LEN,
        attestation::{AttestationError, parse_and_check_pcrs},
        protocol::range_user_data,
    };

    /// Builds a minimal synthetic COSE_Sign1 attestation document suitable for unit tests.
    /// The signature bytes are a 96-byte placeholder — no cryptographic verification is
    /// performed by `parse_attestation_doc` / `parse_and_check_pcrs` (see the TODO in
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

    /// PCR values used by every test — a non-placeholder, fully deterministic measurement
    /// so `parse_and_check_pcrs` does not bail out on the all-zero guard.
    fn test_pcrs() -> [[u8; PCR_LEN]; 3] {
        [[3u8; PCR_LEN]; 3]
    }

    fn test_expected_pcrs() -> ExpectedPcrs {
        ExpectedPcrs {
            pcr0: [3u8; PCR_LEN],
            pcr1: [3u8; PCR_LEN],
            pcr2: [3u8; PCR_LEN],
        }
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
        let attestation_doc = make_attestation_doc(&test_pcrs(), &user_data);

        let artifact = NitroRangeProofArtifact {
            boot_info,
            attestation_doc,
            signature: vec![],
        };

        let expected_user_data = range_user_data(&artifact.boot_info);
        parse_and_check_pcrs(
            &artifact.attestation_doc,
            &test_expected_pcrs(),
            &expected_user_data,
        )
        .unwrap();
    }

    #[test]
    fn tampered_boot_info_fails_user_data_check() {
        let boot_info = boot_info();
        let user_data = range_user_data(&boot_info);
        let attestation_doc = make_attestation_doc(&test_pcrs(), &user_data);

        let mut tampered = boot_info;
        tampered.l2PostRoot = B256::from([9; 32]);

        let expected_user_data = range_user_data(&tampered);
        let err =
            parse_and_check_pcrs(&attestation_doc, &test_expected_pcrs(), &expected_user_data)
                .unwrap_err();

        assert!(matches!(err, AttestationError::UserDataMismatch { .. }));
    }

    #[test]
    fn wrong_pcrs_fail_verification() {
        let boot_info = boot_info();
        let user_data = range_user_data(&boot_info);
        // Document carries PCR0 = 0x03; we verify against a different non-zero value.
        let attestation_doc = make_attestation_doc(&test_pcrs(), &user_data);

        let wrong_pcrs = ExpectedPcrs {
            pcr0: [1u8; PCR_LEN],
            pcr1: [3u8; PCR_LEN],
            pcr2: [3u8; PCR_LEN],
        };
        let err = parse_and_check_pcrs(&attestation_doc, &wrong_pcrs, &user_data).unwrap_err();

        assert!(matches!(
            err,
            AttestationError::PcrMismatch { index: 0, .. }
        ));
    }

    #[test]
    fn placeholder_pcrs_are_rejected() {
        let boot_info = boot_info();
        let user_data = range_user_data(&boot_info);
        let attestation_doc = make_attestation_doc(&[[0u8; PCR_LEN]; 3], &user_data);

        let err = parse_and_check_pcrs(&attestation_doc, &ExpectedPcrs::PLACEHOLDER, &user_data)
            .unwrap_err();
        assert!(matches!(
            err,
            AttestationError::EmptyExpectedPcr { index: 0 }
        ));
    }
}
