#![cfg_attr(not(test), warn(unused_crate_dependencies))]

pub mod date_marker;
pub mod external_nullifier;

use crate::{date_marker::DateMarker, external_nullifier::ExternalNullifier};
use alloy_primitives::U256;
use alloy_rlp::{Decodable, Encodable, RlpDecodable, RlpEncodable};
use semaphore::packed_proof::PackedProof;
use semaphore::protocol::{verify_proof, ProofError};
use semaphore::Field;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const TREE_DEPTH: usize = 30;
const LEN: usize = 256;

// TODO: docs
#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum PBHSidecar {
    PBHPayload(PBHPayload),
    PBHBundle(Vec<PBHPayload>),
}

impl PBHSidecar {
    pub fn external_nullifiers(&self) -> Vec<&ExternalNullifier> {
        match self {
            PBHSidecar::PBHPayload(payload) => vec![&payload.external_nullifier],
            PBHSidecar::PBHBundle(payloads) => {
                payloads.iter().map(|p| &p.external_nullifier).collect()
            }
        }
    }
}

/// The payload of a PBH transaction
///
/// Contains the semaphore proof and relevent metadata
/// required to to verify the pbh transaction.
#[derive(
    Default, Clone, Debug, Deserialize, Serialize, RlpEncodable, RlpDecodable, PartialEq, Eq,
)]
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
    pub fn validate(
        &self,
        signal: U256,
        valid_roots: &[Field],
        pbh_nonce_limit: u8,
    ) -> Result<(), PBHValidationError> {
        self.validate_root(valid_roots)?;

        let date = chrono::Utc::now();
        self.validate_external_nullifier(date, pbh_nonce_limit)?;

        if verify_proof(
            self.root,
            self.nullifier_hash,
            signal,
            self.external_nullifier.to_word(),
            &self.proof.0,
            TREE_DEPTH,
        )? {
            Ok(())
        } else {
            Err(PBHValidationError::InvalidProof)
        }
    }

    pub fn validate_root(&self, valid_roots: &[Field]) -> Result<(), PBHValidationError> {
        if !valid_roots.contains(&self.root) {
            return Err(PBHValidationError::InvalidRoot);
        }

        Ok(())
    }

    pub fn validate_external_nullifier(
        &self,
        date: chrono::DateTime<chrono::Utc>,
        pbh_nonce_limit: u8,
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

pub type ProofBytes = [u8; LEN];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof(pub semaphore::protocol::Proof);

impl Default for Proof {
    fn default() -> Self {
        let proof = semaphore::protocol::Proof(
            (0u64.into(), 0u64.into()),
            ([0u64.into(), 0u64.into()], [0u64.into(), 0u64.into()]),
            (0u64.into(), 0u64.into()),
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
}

#[cfg(test)]
mod test {
    use chrono::TimeZone;
    use ethers_core::types::U256;
    use semaphore::Field;
    use test_case::test_case;

    use super::*;

    #[test]
    // TODO: fuzz inputs
    fn test_encode_decode() {
        let proof = Proof(semaphore::protocol::Proof(
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
    fn test_valid_root() -> eyre::Result<()> {
        let pbh_payload = PBHPayload {
            root: Field::from(1u64),
            ..Default::default()
        };

        let valid_roots = vec![Field::from(1u64), Field::from(2u64)];
        pbh_payload.validate_root(&valid_roots)?;

        Ok(())
    }

    #[test]
    fn test_invalid_root() -> eyre::Result<()> {
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
    fn test_valid_external_nullifier(external_nullifier: ExternalNullifier) -> eyre::Result<()> {
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
    fn test_invalid_external_nullifier_invalid_period(
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
    fn test_invalid_external_nullifier_invalid_nonce() -> eyre::Result<()> {
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
