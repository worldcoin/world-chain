use alloy_primitives::U256;
use alloy_rlp::{Decodable, Encodable, RlpDecodable, RlpEncodable};
use semaphore::packed_proof::PackedProof;
use semaphore::protocol::{verify_proof, ProofError};
use semaphore::Field;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{date_marker::DateMarker, external_nullifier::ExternalNullifier};

pub const TREE_DEPTH: usize = 30;

const LEN: usize = 256;

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

#[derive(Error, Debug, PartialEq, Eq)]
pub enum PbhValidationError {
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

/// The payload of a PBH transaction
///
/// Contains the semaphore proof and relevent metadata
/// required to to verify the pbh transaction.
#[derive(Default, Clone, Debug, RlpEncodable, RlpDecodable, PartialEq, Eq)]
pub struct PbhPayload {
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

impl PbhPayload {
    pub fn validate(
        &self,
        signal: U256,
        valid_roots: &[Field],
        pbh_nonce_limit: u8,
    ) -> Result<(), PbhValidationError> {
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
            Err(PbhValidationError::InvalidProof)
        }
    }

    pub fn validate_root(&self, valid_roots: &[Field]) -> Result<(), PbhValidationError> {
        if !valid_roots.contains(&self.root) {
            return Err(PbhValidationError::InvalidRoot);
        }

        Ok(())
    }

    pub fn validate_external_nullifier(
        &self,
        date: chrono::DateTime<chrono::Utc>,
        pbh_nonce_limit: u8,
    ) -> Result<(), PbhValidationError> {
        // In most cases these will be the same value, but at the month boundary
        // we'll still accept the previous month if the transaction is at most a minute late
        // or the next month if the transaction is at most a minute early
        let valid_dates = [
            DateMarker::from(date - chrono::Duration::minutes(1)),
            DateMarker::from(date),
            DateMarker::from(date + chrono::Duration::minutes(1)),
        ];
        if valid_dates
            .iter()
            .all(|d| self.external_nullifier.date_marker() != *d)
        {
            return Err(PbhValidationError::InvalidExternalNullifierPeriod);
        }

        if self.external_nullifier.nonce >= pbh_nonce_limit {
            return Err(PbhValidationError::InvalidExternalNullifierNonce);
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use alloy_primitives::Address;
    use chrono::{TimeZone, Utc};
    use ethers_core::rand::rngs::SmallRng;
    use ethers_core::rand::{Rng, SeedableRng};
    use ethers_core::types::U256;
    use semaphore::Field;

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
        let pbh_payload = PbhPayload {
            external_nullifier: ExternalNullifier::v1(1, 2024, 11),
            nullifier_hash: Field::from(10u64),
            root: Field::from(12u64),
            proof,
        };

        let mut out = vec![];
        pbh_payload.encode(&mut out);
        let decoded = PbhPayload::decode(&mut out.as_slice()).unwrap();
        assert_eq!(pbh_payload, decoded);
    }

    #[test]
    fn test_validate_root() -> eyre::Result<()> {
        // Test when the root is valid
        let mut pbh_payload = PbhPayload::default();
        pbh_payload.root = Field::from(1u64);

        let valid_roots = vec![Field::from(1u64), Field::from(2u64)];
        pbh_payload.validate_root(&valid_roots)?;

        // Test invalid root
        pbh_payload.root = Field::from(3u64);
        let res = pbh_payload.validate_root(&valid_roots);
        assert_eq!(
            res.expect_err("Expected an error"),
            PbhValidationError::InvalidRoot
        );

        Ok(())
    }

    // TODO: move validate roto test to pbhpayload tests
    // #[test]
    // fn valid_root() {
    //     let mut validator = world_chain_validator();
    //     let root = Field::from(1u64);
    //     let proof = Proof(semaphore::protocol::Proof(
    //         (U256::from(1u64), U256::from(2u64)),
    //         (
    //             [U256::from(3u64), U256::from(4u64)],
    //             [U256::from(5u64), U256::from(6u64)],
    //         ),
    //         (U256::from(7u64), U256::from(8u64)),
    //     ));
    //     let payload = PbhPayload {
    //         external_nullifier: ExternalNullifier::v1(1, 2025, 11),
    //         nullifier_hash: Field::from(10u64),
    //         root,
    //         proof,
    //     };
    //     let header = SealedHeader::new(Header::default(), Header::default().hash_slow());
    //     let body = BlockBody::<OpTransactionSigned>::default();
    //     let block = SealedBlock::new(header, body);
    //     let client = MockEthProvider::default();
    //     // Insert a world id root into the OpWorldId Account
    //     client.add_account(
    //         TEST_WORLD_ID,
    //         ExtendedAccount::new(0, alloy_primitives::U256::ZERO)
    //             .extend_storage(vec![(LATEST_ROOT_SLOT.into(), Field::from(1u64))]),
    //     );
    //     validator.root_validator.set_client(client);
    //     validator.on_new_head_block(&block);
    //     let res = validator.validate_root(&payload);

    //     let valid_roots = validator.roots();
    //     payload.validate_root(valid_roots);
    //     assert!(res.is_ok());
    // }

    // TODO: move validate roto test to pbhpayload tests
    // #[test]
    // fn invalid_root() {
    //     let mut validator = world_chain_validator();
    //     let root = Field::from(0);
    //     let proof = Proof(semaphore::protocol::Proof(
    //         (U256::from(1u64), U256::from(2u64)),
    //         (
    //             [U256::from(3u64), U256::from(4u64)],
    //             [U256::from(5u64), U256::from(6u64)],
    //         ),
    //         (U256::from(7u64), U256::from(8u64)),
    //     ));
    //     let payload = PbhPayload {
    //         external_nullifier: ExternalNullifier::v1(1, 2025, 11),
    //         nullifier_hash: Field::from(10u64),
    //         root,
    //         proof,
    //     };
    //     let header = SealedHeader::new(Header::default(), Header::default().hash_slow());
    //     let body = BlockBody::<OpTransactionSigned>::default();
    //     let block = SealedBlock::new(header, body);
    //     let client = MockEthProvider::default();
    //     // Insert a world id root into the OpWorldId Account
    //     client.add_account(
    //         TEST_WORLD_ID,
    //         ExtendedAccount::new(0, alloy_primitives::U256::ZERO)
    //             .extend_storage(vec![(LATEST_ROOT_SLOT.into(), Field::from(1u64))]),
    //     );
    //     validator.root_validator.set_client(client);
    //     validator.on_new_head_block(&block);
    //     let res = validator.validate_root(&payload);
    //     assert!(res.is_err());
    // }

    // TODO: move validate roto test to pbhpayload tests
    // #[test_case(ExternalNullifier::v1(1, 2025, 0) ; "01-2025-0")]
    // #[test_case(ExternalNullifier::v1(1, 2025, 1) ; "01-2025-1")]
    // #[test_case(ExternalNullifier::v1(1, 2025, 29) ; "01-2025-29")]
    // fn validate_external_nullifier_valid(external_nullifier: ExternalNullifier) {
    //     let validator = world_chain_validator();
    //     let date = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

    //     let payload = PbhPayload {
    //         external_nullifier,
    //         nullifier_hash: Field::ZERO,
    //         root: Field::ZERO,
    //         proof: Default::default(),
    //     };

    //     validator
    //         .validate_external_nullifier(date, &payload)
    //         .unwrap();
    // }

    // TODO: move validate roto test to pbhpayload tests
    // #[test_case(ExternalNullifier::v1(1, 2025, 0), "2024-12-31 23:59:30Z" ; "a minute early")]
    // #[test_case(ExternalNullifier::v1(1, 2025, 0), "2025-02-01 00:00:30Z" ; "a minute late")]
    // fn validate_external_nullifier_at_time(external_nullifier: ExternalNullifier, time: &str) {
    //     let validator = world_chain_validator();
    //     let date: chrono::DateTime<Utc> = time.parse().unwrap();

    //     let payload = PbhPayload {
    //         external_nullifier,
    //         nullifier_hash: Field::ZERO,
    //         root: Field::ZERO,
    //         proof: Default::default(),
    //     };

    //     validator
    //         .validate_external_nullifier(date, &payload)
    //         .unwrap();
    // }
}
