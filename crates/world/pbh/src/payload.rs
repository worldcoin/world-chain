use crate::external_nullifier::EncodedExternalNullifier;
use crate::{date_marker::DateMarker, external_nullifier::ExternalNullifier};
use alloy_primitives::U256;
use alloy_rlp::{Decodable, Encodable, RlpDecodable, RlpEncodable};
use semaphore_rs::packed_proof::PackedProof;
use semaphore_rs::protocol::{verify_proof, ProofError};
use semaphore_rs::Field;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const TREE_DEPTH: usize = 30;
const LEN: usize = 256;

pub type ProofBytes = [u8; LEN];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof(pub semaphore_rs::protocol::Proof);

impl Default for Proof {
    fn default() -> Self {
        let proof = semaphore_rs::protocol::Proof(
            (U256::ZERO, U256::ZERO),
            ([U256::ZERO, U256::ZERO], [U256::ZERO, U256::ZERO]),
            (U256::ZERO, U256::ZERO),
        );

        Proof(proof)
    }
}

impl Decodable for Proof {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let bytes = ProofBytes::decode(buf)?;
        Ok(Proof(PackedProof(bytes).into()))
    }
}

impl Encodable for Proof {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let PackedProof(bytes) = self.0.into();
        bytes.encode(out)
    }

    fn length(&self) -> usize {
        LEN + 3
    }
}

#[derive(Error, Debug)]
pub enum PBHValidationError {
    #[error("Invalid root")]
    InvalidRoot,
    #[error("Invalid external nullifier period")]
    InvalidExternalNullifierPeriod,
    #[error("Invalid external nullifier nonce")]
    InvalidExternalNullifierNonce,
    #[error("Invalid proof")]
    InvalidProof,
    #[error(transparent)]
    ProofError(#[from] ProofError),
    #[error("Invalid calldata encoding")]
    InvalidCalldata,
    #[error("Missing PBH Payload")]
    MissingPbhPayload,
    #[error("InvalidSignatureAggregator")]
    InvalidSignatureAggregator,
    #[error("PBH call tracer error")]
    PBHCallTracerError,
    #[error("PBH gas limit exceeded")]
    PbhGasLimitExceeded,
    #[error("Duplicate nullifier hash")]
    DuplicateNullifierHash,
}

/// The payload of a PBH transaction
///
/// Contains the semaphore proof and relevant metadata
/// required to to verify the pbh transaction.
#[derive(Default, Clone, Debug, RlpEncodable, RlpDecodable, PartialEq, Eq)]
pub struct PBHPayload {
    /// A string containing a prefix, the date marker, and the pbh nonce
    pub external_nullifier: ExternalNullifier,
    /// A nullifier hash used to keep track of
    /// previously used pbh transactions
    pub nullifier_hash: Field,
    /// The root of the merkle tree for which this proof
    /// was generated
    pub root: Field,
    /// The actual semaphore proof verifying that the sender
    /// is included in the set of orb verified users
    pub proof: Proof,
}

impl PBHPayload {
    /// Validates the PBH payload by validating the merkle root, external nullifier, and semaphore proof.
    /// Returns an error if any of the validations steps fail.
    pub fn validate(
        &self,
        signal: U256,
        valid_roots: &[Field],
        pbh_nonce_limit: u16,
    ) -> Result<(), PBHValidationError> {
        self.validate_root(valid_roots)?;

        let date = chrono::Utc::now();
        self.validate_external_nullifier(date, pbh_nonce_limit)?;

        let flat = self.proof.0.flatten();
        let proof = if (flat[4] | flat[5] | flat[6] | flat[7]).is_zero() {
            // proof is compressed
            let compressed_flat = [flat[0], flat[1], flat[2], flat[3]];
            let compressed_proof =
                semaphore_rs_proof::compression::CompressedProof::from_flat(compressed_flat);
            &semaphore_rs_proof::compression::decompress_proof(compressed_proof)
                .ok_or(PBHValidationError::InvalidProof)?
        } else {
            &self.proof.0
        };

        if verify_proof(
            self.root,
            self.nullifier_hash,
            signal,
            EncodedExternalNullifier::from(self.external_nullifier).0,
            proof,
            TREE_DEPTH,
        )? {
            Ok(())
        } else {
            Err(PBHValidationError::InvalidProof)
        }
    }

    /// Checks if the Merkle root exists in the list of valid roots.
    /// Returns an error if the root is not found.
    pub fn validate_root(&self, valid_roots: &[Field]) -> Result<(), PBHValidationError> {
        if !valid_roots.contains(&self.root) {
            return Err(PBHValidationError::InvalidRoot);
        }

        Ok(())
    }

    /// Ensures the external nullifier is valid by checking the month, year and nonce limit.
    /// Returns an error if the date is incorrect or if the nonce exceeds the allowed limit.
    pub fn validate_external_nullifier(
        &self,
        date: chrono::DateTime<chrono::Utc>,
        pbh_nonce_limit: u16,
    ) -> Result<(), PBHValidationError> {
        if self.external_nullifier.date_marker() != DateMarker::from(date) {
            return Err(PBHValidationError::InvalidExternalNullifierPeriod);
        }

        if self.external_nullifier.nonce >= pbh_nonce_limit {
            return Err(PBHValidationError::InvalidExternalNullifierNonce);
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use alloy_primitives::U256;
    use chrono::{Datelike, TimeZone, Utc};
    use semaphore_rs::Field;
    use test_case::test_case;

    use super::*;

    #[test]
    // TODO: fuzz inputs
    fn encode_decode() {
        let proof = Proof(semaphore_rs::protocol::Proof(
            (U256::from(1u64), U256::from(2u64)),
            (
                [U256::from(3u64), U256::from(4u64)],
                [U256::from(5u64), U256::from(6u64)],
            ),
            (U256::from(7u64), U256::from(8u64)),
        ));
        let pbh_payload = PBHPayload {
            external_nullifier: ExternalNullifier::v1(1, 2024, 11),
            nullifier_hash: Field::from(10u64),
            root: Field::from(12u64),
            proof,
        };

        let mut out = vec![];
        pbh_payload.encode(&mut out);
        let decoded = PBHPayload::decode(&mut out.as_slice()).unwrap();
        assert_eq!(pbh_payload, decoded);
    }

    #[test]
    fn serialize_compressed_proof() {
        let identity = semaphore_rs::identity::Identity::from_secret(&mut [1, 2, 3], None);
        let mut tree = semaphore_rs::poseidon_tree::LazyPoseidonTree::new_with_dense_prefix(
            30,
            0,
            &U256::ZERO,
        );
        tree = tree.update_with_mutation(0, &identity.commitment());

        let merkle_proof = tree.proof(0);
        let now = Utc::now();
        let date_marker = DateMarker::new(now.year(), now.month());

        let external_nullifier = ExternalNullifier::with_date_marker(date_marker, 0);
        let external_nullifier_hash: EncodedExternalNullifier = external_nullifier.into();
        let external_nullifier_hash = external_nullifier_hash.0;
        let signal = U256::ZERO;

        // Generate a normal proof
        let proof = semaphore_rs::protocol::generate_proof(
            &identity,
            &merkle_proof,
            external_nullifier_hash,
            signal,
        )
        .unwrap();
        let nullifier_hash =
            semaphore_rs::protocol::generate_nullifier_hash(&identity, external_nullifier_hash);

        // Compress the proof
        let compressed_proof = semaphore_rs_proof::compression::compress_proof(proof).unwrap();

        // Reserialize to backwards compat format
        let flat = compressed_proof.flatten();
        let proof = [
            flat[0],
            flat[1],
            flat[2],
            flat[3],
            U256::ZERO,
            U256::ZERO,
            U256::ZERO,
            U256::ZERO,
        ];
        let proof = semaphore_rs::protocol::Proof::from_flat(proof);
        let proof = Proof(proof);

        let pbh_payload = PBHPayload {
            root: tree.root(),
            external_nullifier,
            nullifier_hash,
            proof,
        };

        pbh_payload.validate(signal, &[tree.root()], 10).unwrap();
    }

    #[test]
    fn valid_root() -> eyre::Result<()> {
        let pbh_payload = PBHPayload {
            root: Field::from(1u64),
            ..Default::default()
        };

        let valid_roots = vec![Field::from(1u64), Field::from(2u64)];
        pbh_payload.validate_root(&valid_roots)?;

        Ok(())
    }

    #[test]
    fn invalid_root() -> eyre::Result<()> {
        let pbh_payload = PBHPayload {
            root: Field::from(3u64),
            ..Default::default()
        };

        let valid_roots = vec![Field::from(1u64), Field::from(2u64)];
        let res = pbh_payload.validate_root(&valid_roots);
        assert!(matches!(res, Err(PBHValidationError::InvalidRoot)));

        Ok(())
    }

    #[test_case(ExternalNullifier::v1(1, 2025, 0) ; "01-2025-0")]
    #[test_case(ExternalNullifier::v1(1, 2025, 1) ; "01-2025-1")]
    #[test_case(ExternalNullifier::v1(1, 2025, 29) ; "01-2025-29")]
    fn valid_external_nullifier(external_nullifier: ExternalNullifier) -> eyre::Result<()> {
        let pbh_nonce_limit = 30;
        let date = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

        let pbh_payload = PBHPayload {
            external_nullifier,
            ..Default::default()
        };

        pbh_payload.validate_external_nullifier(date, pbh_nonce_limit)?;
        Ok(())
    }

    #[test_case(ExternalNullifier::v1(1, 2024, 0) ; "01-2024-0")]
    #[test_case(ExternalNullifier::v1(2, 2025, 0) ; "02-2025-0")]
    fn invalid_external_nullifier_invalid_period(
        external_nullifier: ExternalNullifier,
    ) -> eyre::Result<()> {
        let pbh_nonce_limit = 30;
        let date = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

        let pbh_payload = PBHPayload {
            external_nullifier,
            ..Default::default()
        };

        let res = pbh_payload.validate_external_nullifier(date, pbh_nonce_limit);
        assert!(matches!(
            res,
            Err(PBHValidationError::InvalidExternalNullifierPeriod)
        ));

        Ok(())
    }

    #[test]
    fn invalid_external_nullifier_invalid_nonce() -> eyre::Result<()> {
        let pbh_nonce_limit = 30;
        let date = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

        let external_nullifier = ExternalNullifier::v1(1, 2025, 30);
        let pbh_payload = PBHPayload {
            external_nullifier,
            ..Default::default()
        };

        let res = pbh_payload.validate_external_nullifier(date, pbh_nonce_limit);
        assert!(matches!(
            res,
            Err(PBHValidationError::InvalidExternalNullifierNonce)
        ));

        Ok(())
    }
}
