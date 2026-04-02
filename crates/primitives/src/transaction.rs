use alloy_consensus::{
    SignableTransaction, Transaction,
    transaction::{RlpEcdsaDecodableTx, RlpEcdsaEncodableTx},
};
use alloy_eips::{
    Encodable2718, Typed2718, eip2718::IsTyped2718, eip2930::AccessList,
    eip7702::SignedAuthorization as Eip7702SignedAuthorization,
};
use alloy_primitives::{Address, B256, Bytes, ChainId, Signature, TxKind, U256, keccak256};
use alloy_rlp::{BufMut, Decodable, Encodable, Header, length_of_length};

pub mod account;
pub mod envelope;
pub mod signature;

pub use account::*;
pub use envelope::*;

use crate::transaction::signature::{
    KeyCommitment, KeyHash, PrehashVerify, SignatureError, SignaturePayload,
    SignedAuthorization as WorldSignedAuthorization,
};

/// EIP-2718 transaction type byte for World Chain native transactions.
pub const WORLD_TX_TYPE_ID: u8 = 0x1D;

/// A World Chain native transaction (type `0x1D`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct TxWorld {
    /// EIP-155: Simple replay attack protection
    #[cfg_attr(feature = "serde", serde(with = "alloy_serde::quantity"))]
    pub chain_id: ChainId,

    /// A scalar value equal to the number of transactions sent by the sender.
    #[cfg_attr(feature = "serde", serde(with = "alloy_serde::quantity"))]
    pub nonce: u64,

    /// Max Priority fee that transaction is paying (GasTipCap).
    #[cfg_attr(feature = "serde", serde(with = "alloy_serde::quantity"))]
    pub max_priority_fee_per_gas: u128,

    /// Maximum total fee per unit of gas the sender is willing to pay (GasFeeCap).
    #[cfg_attr(feature = "serde", serde(with = "alloy_serde::quantity"))]
    pub max_fee_per_gas: u128,

    /// Maximum amount of gas that should be used in executing this transaction.
    #[cfg_attr(
        feature = "serde",
        serde(with = "alloy_serde::quantity", rename = "gas", alias = "gasLimit")
    )]
    pub gas_limit: u64,

    /// The 160-bit address of the message call's recipient.
    pub to: Address,

    /// A scalar value equal to the number of Wei to be transferred to the
    /// message call's recipient.
    pub value: U256,

    /// Input data of the message call.
    pub input: Bytes,

    /// The accessList specifies a list of addresses and storage keys;
    /// these addresses and storage keys are added into the `accessed_addresses`
    /// and `accessed_storage_keys` global sets (introduced in EIP-2929).
    #[cfg_attr(
        feature = "serde",
        serde(deserialize_with = "alloy_serde::null_as_default")
    )]
    pub access_list: AccessList,

    /// The World ID authorization: bundles the EIP-7702 delegation, Pedersen
    /// key commitment, membership proof, and cryptographic signature.
    pub authorization: WorldSignedAuthorization,
}

impl TxWorld {
    /// Returns the transaction type constant.
    pub const fn tx_type() -> u8 {
        WORLD_TX_TYPE_ID
    }

    /// Calculates a heuristic for the in-memory size of this transaction.
    #[inline]
    pub fn size(&self) -> usize {
        size_of::<Self>() + self.access_list.size() + self.input.len()
    }

    /// The Pedersen key commitment from the embedded authorization.
    pub fn key_commitment(&self) -> &KeyCommitment {
        &self.authorization.key_commitment
    }

    // ─── Signing body ────────────────────────────────────────────────────

    /// RLP payload length of the body fields included in the signing hash.
    ///
    /// Includes all EIP-1559 fields, the inner EIP-7702 authorization, and
    /// the key commitment. Excludes membership_proof and signature.
    fn signing_body_payload_length(&self) -> usize {
        self.chain_id.length()
            + self.nonce.length()
            + self.max_priority_fee_per_gas.length()
            + self.max_fee_per_gas.length()
            + self.gas_limit.length()
            + self.to.length()
            + self.value.length()
            + self.input.0.length()
            + self.access_list.length()
            + self.authorization.inner.length()
            + self.authorization.key_commitment.length()
    }

    /// RLP-encode the body fields included in the signing hash.
    fn encode_signing_body(&self, out: &mut dyn BufMut) {
        self.chain_id.encode(out);
        self.nonce.encode(out);
        self.max_priority_fee_per_gas.encode(out);
        self.max_fee_per_gas.encode(out);
        self.gas_limit.encode(out);
        self.to.encode(out);
        self.value.encode(out);
        self.input.0.encode(out);
        self.access_list.encode(out);
        self.authorization.inner.encode(out);
        self.authorization.key_commitment.encode(out);
    }

    /// Compute the signing hash.
    ///
    /// `keccak256(0x1D ‖ RLP([body_fields..., inner_auth, key_commitment]))`
    pub fn signature_hash(&self) -> B256 {
        let payload = self.signing_body_payload_length();
        let mut buf = Vec::with_capacity(1 + payload + length_of_length(payload));
        buf.put_u8(WORLD_TX_TYPE_ID);
        Header {
            list: true,
            payload_length: payload,
        }
        .encode(&mut buf);
        self.encode_signing_body(&mut buf);
        keccak256(&buf)
    }

    // ─── Verification ────────────────────────────────────────────────────

    /// End-to-end signature verification.
    ///
    /// 1. Computes the signing hash from the transaction body.
    /// 2. Verifies the cryptographic signature against the signing hash.
    /// 3. Derives `key_hash = hash_key(signer_key_bytes)`.
    /// 4. Verifies the Pedersen membership proof.
    ///
    /// Returns the verified key hash on success.
    pub fn verify_signature(&self, num_keys: usize) -> Result<KeyHash, SignatureError> {
        let sig_hash = self.signature_hash();
        self.authorization.verify(&sig_hash, num_keys)
    }
}

// ─── Full wire-format RLP (includes authorization) ───────────────────────────

impl RlpEcdsaEncodableTx for TxWorld {
    fn rlp_encoded_fields_length(&self) -> usize {
        self.chain_id.length()
            + self.nonce.length()
            + self.max_priority_fee_per_gas.length()
            + self.max_fee_per_gas.length()
            + self.gas_limit.length()
            + self.to.length()
            + self.value.length()
            + self.input.0.length()
            + self.access_list.length()
            + self.authorization.length()
    }

    fn rlp_encode_fields(&self, out: &mut dyn BufMut) {
        self.chain_id.encode(out);
        self.nonce.encode(out);
        self.max_priority_fee_per_gas.encode(out);
        self.max_fee_per_gas.encode(out);
        self.gas_limit.encode(out);
        self.to.encode(out);
        self.value.encode(out);
        self.input.0.encode(out);
        self.access_list.encode(out);
        self.authorization.encode(out);
    }
}

impl RlpEcdsaDecodableTx for TxWorld {
    const DEFAULT_TX_TYPE: u8 = WORLD_TX_TYPE_ID;

    fn rlp_decode_fields(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        Ok(Self {
            chain_id: Decodable::decode(buf)?,
            nonce: Decodable::decode(buf)?,
            max_priority_fee_per_gas: Decodable::decode(buf)?,
            max_fee_per_gas: Decodable::decode(buf)?,
            gas_limit: Decodable::decode(buf)?,
            to: Decodable::decode(buf)?,
            value: Decodable::decode(buf)?,
            input: Decodable::decode(buf)?,
            access_list: Decodable::decode(buf)?,
            authorization: Decodable::decode(buf)?,
        })
    }
}

impl Encodable2718 for TxWorld {
    fn encode_2718(&self, out: &mut dyn BufMut) {
        out.put_u8(WORLD_TX_TYPE_ID);
        let payload = self.rlp_encoded_fields_length();
        Header {
            list: true,
            payload_length: payload,
        }
        .encode(out);
        self.rlp_encode_fields(out);
    }

    fn type_flag(&self) -> Option<u8> {
        WORLD_TX_TYPE_ID.into()
    }

    fn encode_2718_len(&self) -> usize {
        1 + length_of_length(self.rlp_encoded_fields_length()) + self.rlp_encoded_fields_length()
    }
}

impl Encodable for TxWorld {
    fn encode(&self, out: &mut dyn BufMut) {
        self.rlp_encode(out);
    }

    fn length(&self) -> usize {
        self.rlp_encoded_length()
    }
}

impl Decodable for TxWorld {
    fn decode(data: &mut &[u8]) -> alloy_rlp::Result<Self> {
        Self::rlp_decode(data)
    }
}

impl Typed2718 for TxWorld {
    fn ty(&self) -> u8 {
        WORLD_TX_TYPE_ID
    }
}

impl IsTyped2718 for TxWorld {
    fn is_type(type_id: u8) -> bool {
        type_id == WORLD_TX_TYPE_ID
    }
}

impl Transaction for TxWorld {
    #[inline]
    fn chain_id(&self) -> Option<ChainId> {
        Some(self.chain_id)
    }

    #[inline]
    fn nonce(&self) -> u64 {
        self.nonce
    }

    #[inline]
    fn gas_limit(&self) -> u64 {
        self.gas_limit
    }

    #[inline]
    fn gas_price(&self) -> Option<u128> {
        None
    }

    #[inline]
    fn max_fee_per_gas(&self) -> u128 {
        self.max_fee_per_gas
    }

    #[inline]
    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        Some(self.max_priority_fee_per_gas)
    }

    #[inline]
    fn max_fee_per_blob_gas(&self) -> Option<u128> {
        None
    }

    #[inline]
    fn priority_fee_or_price(&self) -> u128 {
        self.max_priority_fee_per_gas
    }

    fn effective_gas_price(&self, base_fee: Option<u64>) -> u128 {
        alloy_eips::eip1559::calc_effective_gas_price(
            self.max_fee_per_gas,
            self.max_priority_fee_per_gas,
            base_fee,
        )
    }

    #[inline]
    fn is_dynamic_fee(&self) -> bool {
        true
    }

    #[inline]
    fn kind(&self) -> TxKind {
        self.to.into()
    }

    #[inline]
    fn is_create(&self) -> bool {
        false
    }

    #[inline]
    fn value(&self) -> U256 {
        self.value
    }

    #[inline]
    fn input(&self) -> &Bytes {
        &self.input
    }

    #[inline]
    fn access_list(&self) -> Option<&AccessList> {
        Some(&self.access_list)
    }

    #[inline]
    fn blob_versioned_hashes(&self) -> Option<&[B256]> {
        None
    }

    #[inline]
    fn authorization_list(&self) -> Option<&[Eip7702SignedAuthorization]> {
        None
    }
}

// ───── Signable Transaction ─────────────────────────────────────────────────

impl SignableTransaction<Signature> for TxWorld {
    fn set_chain_id(&mut self, chain_id: ChainId) {
        self.chain_id = chain_id;
    }

    fn encode_for_signing(&self, out: &mut dyn BufMut) {
        out.put_u8(WORLD_TX_TYPE_ID);
        let payload = self.signing_body_payload_length();
        Header {
            list: true,
            payload_length: payload,
        }
        .encode(out);
        self.encode_signing_body(out);
    }

    fn payload_len_for_signature(&self) -> usize {
        let payload = self.signing_body_payload_length();
        1 + payload + length_of_length(payload)
    }
}

impl alloy_consensus::InMemorySize for TxWorld {
    fn size(&self) -> usize {
        TxWorld::size(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::signature::{self, GENERATORS, hash_key, pedersen};
    use alloy_eips::eip7702::Authorization;
    use alloy_primitives::address;
    use ark_std::rand::SeedableRng;

    fn dummy_authorization() -> WorldSignedAuthorization {
        WorldSignedAuthorization::new(
            Authorization {
                chain_id: U256::from(480),
                address: address!("0x000000000000000000000000000000000000001D"),
                nonce: 0,
            },
            KeyCommitment::default(),
            pedersen::MembershipProof {
                key_index: 0,
                r_point: [0u8; 32],
                responses: vec![],
                blinding_response: U256::ZERO,
            },
            SignaturePayload::Secp256k1 {
                y_parity: 0,
                r: U256::ZERO,
                s: U256::ZERO,
            },
        )
    }

    fn test_tx() -> TxWorld {
        TxWorld {
            chain_id: 480,
            nonce: 1,
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 50_000_000_000,
            gas_limit: 21_000,
            to: address!("0x000000000000000000000000000000000000001D"),
            value: U256::from(1_000_000_000_000_000_000u128),
            input: Bytes::new(),
            access_list: AccessList::default(),
            authorization: dummy_authorization(),
        }
    }

    #[test]
    fn test_rlp_roundtrip() {
        let tx = test_tx();
        let mut buf = Vec::new();
        tx.encode(&mut buf);

        let decoded = TxWorld::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(tx, decoded);
    }

    #[test]
    fn test_typed_2718() {
        let tx = test_tx();
        assert_eq!(tx.ty(), WORLD_TX_TYPE_ID);
        assert!(<TxWorld as IsTyped2718>::is_type(WORLD_TX_TYPE_ID));
        assert!(!<TxWorld as IsTyped2718>::is_type(0x02));
    }

    #[test]
    fn test_transaction_trait() {
        let tx = test_tx();
        assert_eq!(Transaction::chain_id(&tx), Some(480));
        assert_eq!(Transaction::nonce(&tx), 1);
        assert_eq!(Transaction::gas_limit(&tx), 21_000);
        assert_eq!(Transaction::gas_price(&tx), None);
        assert_eq!(Transaction::max_fee_per_gas(&tx), 50_000_000_000);
        assert_eq!(
            Transaction::max_priority_fee_per_gas(&tx),
            Some(1_000_000_000)
        );
        assert!(Transaction::is_dynamic_fee(&tx));
        assert!(!Transaction::is_create(&tx));
    }

    #[test]
    fn test_signing_hash_excludes_signature() {
        let tx = test_tx();
        let hash1 = tx.signature_hash();

        // Change the signature payload — signing hash must NOT change.
        let mut tx2 = tx.clone();
        tx2.authorization.signature = SignaturePayload::Secp256k1 {
            y_parity: 1,
            r: U256::from(999),
            s: U256::from(888),
        };
        assert_eq!(hash1, tx2.signature_hash());

        // Change the key_commitment — signing hash MUST change.
        let mut tx3 = tx.clone();
        tx3.authorization.key_commitment = KeyCommitment(B256::from([0xFF; 32]));
        assert_ne!(hash1, tx3.signature_hash());
    }

    #[test]
    fn test_signing_hash_includes_type_prefix() {
        let tx = test_tx();
        let mut buf = Vec::new();
        SignableTransaction::<Signature>::encode_for_signing(&tx, &mut buf);
        assert_eq!(buf[0], WORLD_TX_TYPE_ID);
    }

    #[test]
    fn test_signing_hash_deterministic() {
        let tx = test_tx();
        assert_eq!(tx.signature_hash(), tx.signature_hash());
        assert_ne!(tx.signature_hash(), B256::ZERO);
    }

    // ─── End-to-end: build keychain → commit → prove → sign → verify ─────

    #[test]
    fn test_e2e_ed25519_sign_and_verify() {
        use ed25519_dalek::{Signer, SigningKey};

        // 1. Generate an Ed25519 keypair.
        let mut rng = ark_std::rand::rngs::StdRng::seed_from_u64(0xCAFE);
        let signing_key = SigningKey::generate(&mut rng);
        let verifying_key = signing_key.verifying_key();
        let vk_bytes = verifying_key.to_bytes();

        // 2. Build a keychain with 3 keys; our Ed25519 key is at index 1.
        let key_hashes = vec![
            hash_key(&[0x02; 33]), // some secp256k1 key
            hash_key(&vk_bytes),   // our Ed25519 key
            hash_key(&[0xAB; 64]), // some P256 key
        ];
        let blinding = U256::from(42u64);
        let gens = &*GENERATORS;

        // 3. Commit.
        let commitment =
            pedersen::PedersenCommitment::commit(&key_hashes, blinding, gens).expect("commit");
        let key_commitment = commitment.to_key_commitment();

        // 4. Create membership proof for index 1.
        let proof = pedersen::MembershipProof::prove(&key_hashes, blinding, 1, gens, &mut rng)
            .expect("prove");

        // 5. Build the unsigned transaction body (with placeholder sig).
        let mut tx = TxWorld {
            chain_id: 480,
            nonce: 0,
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 50_000_000_000,
            gas_limit: 21_000,
            to: address!("0x000000000000000000000000000000000000001D"),
            value: U256::ZERO,
            input: Bytes::new(),
            access_list: AccessList::default(),
            authorization: WorldSignedAuthorization::new(
                Authorization {
                    chain_id: U256::from(480),
                    address: address!("0x000000000000000000000000000000000000001D"),
                    nonce: 0,
                },
                key_commitment,
                proof.clone(),
                // Placeholder signature — will be replaced after signing.
                SignaturePayload::Ed25519 {
                    key: Bytes::from(vk_bytes.to_vec()),
                    big_r: alloy_primitives::FixedBytes::ZERO,
                    s: alloy_primitives::FixedBytes::ZERO,
                },
            ),
        };

        // 6. Compute signing hash and sign.
        let sig_hash = tx.signature_hash();
        let ed_sig = signing_key.sign(sig_hash.as_ref());
        let sig_bytes = ed_sig.to_bytes();

        // 7. Replace placeholder with real signature.
        tx.authorization.signature = SignaturePayload::Ed25519 {
            key: Bytes::from(vk_bytes.to_vec()),
            big_r: alloy_primitives::FixedBytes::from_slice(&sig_bytes[..32]),
            s: alloy_primitives::FixedBytes::from_slice(&sig_bytes[32..]),
        };

        // 8. Verify end-to-end.
        let verified_key_hash = tx
            .verify_signature(key_hashes.len())
            .expect("verification should succeed");
        assert_eq!(verified_key_hash, hash_key(&vk_bytes));

        // 9. Tampering: changing the tx body should cause verification failure.
        let mut tampered = tx.clone();
        tampered.value = U256::from(999);
        assert!(tampered.verify_signature(key_hashes.len()).is_err());
    }

    #[test]
    fn test_e2e_wrong_key_fails_membership() {
        use ed25519_dalek::{Signer, SigningKey};

        let mut rng = ark_std::rand::rngs::StdRng::seed_from_u64(0xBEEF);
        let signing_key = SigningKey::generate(&mut rng);
        let vk_bytes = signing_key.verifying_key().to_bytes();

        // Keychain does NOT contain our key.
        let key_hashes = vec![hash_key(&[0x02; 33]), hash_key(&[0x03; 33])];
        let blinding = U256::from(7u64);
        let gens = &*GENERATORS;

        let commitment =
            pedersen::PedersenCommitment::commit(&key_hashes, blinding, gens).expect("commit");
        let key_commitment = commitment.to_key_commitment();

        // Proof is for index 0 (a different key).
        let proof = pedersen::MembershipProof::prove(&key_hashes, blinding, 0, gens, &mut rng)
            .expect("prove");

        let mut tx = TxWorld {
            chain_id: 480,
            nonce: 0,
            max_priority_fee_per_gas: 0,
            max_fee_per_gas: 0,
            gas_limit: 21_000,
            to: Address::ZERO,
            value: U256::ZERO,
            input: Bytes::new(),
            access_list: AccessList::default(),
            authorization: WorldSignedAuthorization::new(
                Authorization {
                    chain_id: U256::from(480),
                    address: Address::ZERO,
                    nonce: 0,
                },
                key_commitment,
                proof,
                SignaturePayload::Ed25519 {
                    key: Bytes::from(vk_bytes.to_vec()),
                    big_r: alloy_primitives::FixedBytes::ZERO,
                    s: alloy_primitives::FixedBytes::ZERO,
                },
            ),
        };

        let sig_hash = tx.signature_hash();
        let ed_sig = signing_key.sign(sig_hash.as_ref());
        let sig_bytes = ed_sig.to_bytes();

        tx.authorization.signature = SignaturePayload::Ed25519 {
            key: Bytes::from(vk_bytes.to_vec()),
            big_r: alloy_primitives::FixedBytes::from_slice(&sig_bytes[..32]),
            s: alloy_primitives::FixedBytes::from_slice(&sig_bytes[32..]),
        };

        // Verification should fail: key hash doesn't match what the proof proves.
        assert!(tx.verify_signature(key_hashes.len()).is_err());
    }
}
