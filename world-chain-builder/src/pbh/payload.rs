use alloy_rlp::{Decodable, Encodable, RlpDecodable, RlpEncodable};
use semaphore::packed_proof::PackedProof;
use semaphore::Field;
use serde::{Deserialize, Serialize};

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

/// The payload of a PBH transaction
///
/// Contains the semaphore proof and relevent metadata
/// required to to verify the pbh transaction.
#[derive(Clone, Debug, RlpEncodable, RlpDecodable, PartialEq, Eq, Default)]
pub struct PbhPayload {
    /// A string containing a prefix, the date marker, and the pbh nonce
    pub external_nullifier: String,
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

#[cfg(test)]
mod test {
    use ethers_core::types::U256;

    use super::*;

    #[test]
    fn encode_decode() {
        let proof = Proof(semaphore::protocol::Proof(
            (U256::from(1u64), U256::from(2u64)),
            (
                [U256::from(3u64), U256::from(4u64)],
                [U256::from(5u64), U256::from(6u64)],
            ),
            (U256::from(7u64), U256::from(8u64)),
        ));
        let semaphore_proof = PbhPayload {
            external_nullifier: "0-012025-11".to_string(),
            nullifier_hash: Field::from(10u64),
            root: Field::from(12u64),
            proof,
        };
        let encoded = alloy_rlp::encode(&semaphore_proof);
        let mut buf = encoded.as_slice();
        let decoded = PbhPayload::decode(&mut buf).unwrap();
        assert_eq!(semaphore_proof, decoded);
    }
}
