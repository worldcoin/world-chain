use alloy_eips::eip7702::Authorization;
use alloy_primitives::{B256, Bytes, FixedBytes, U256};
use alloy_rlp::{
    BufMut, Decodable, Encodable, Header, RlpDecodable, RlpEncodable, length_of_length,
};
use base64::{Engine, prelude::BASE64_URL_SAFE_NO_PAD};
use ed25519_dalek::{self, Verifier, VerifyingKey as Ed25519VerifyingKey};
use elliptic_curve::{FieldBytes, ProjectivePoint};
use p256::NistP256;
use sha2::{Digest, Sha256};

pub mod keychain;
pub mod pedersen;
pub mod proof;

pub use keychain::*;
pub use pedersen::*;

// ─── Signature type flag bytes (§ Transaction Type 0x1D) ────────────────────

pub const SIGNATURE_TYPE_SECP256K1: u8 = 0x00;
pub const SIGNATURE_TYPE_P256: u8 = 0x01;
pub const SIGNATURE_TYPE_WEBAUTHN: u8 = 0x02;
pub const SIGNATURE_TYPE_ED25519: u8 = 0x03;

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(thiserror::Error, Debug)]
pub enum SignatureError {
    #[error(transparent)]
    Any(#[from] eyre::Report),
    #[error("invalid ecdsa signature: {0}")]
    Ecdsa(#[from] ecdsa::Error),
    #[error("invalid ed25519 signature: {0}")]
    Ed25519(ed25519_dalek::SignatureError),
    #[error("invalid webauthn data: {0}")]
    WebAuthn(&'static str),
    #[error("unknown signature type: {0:#x}")]
    UnknownSignatureType(u8),
    #[error("invalid public key")]
    InvalidPublicKey,
    #[error("key not found in authorized set")]
    KeyNotInAuthorizedSet,
    #[error("invalid parity: {0}")]
    InvalidParity(u64),
    #[error("key hash mismatch")]
    KeyHashMismatch,
}

// ─── PrehashVerify ───────────────────────────────────────────────────────────

/// Generic prehash signature verification.
///
/// Implementors verify a cryptographic signature against a prehashed message
/// digest and yield the signer's raw public key bytes on success. The key
/// bytes can then be hashed via [`hash_key`] and checked against the Pedersen
/// membership proof.
pub trait PrehashVerify {
    /// Verify the signature and return the signer's raw public key bytes.
    ///
    /// For secp256k1 the key is *recovered* from the signature; for all other
    /// schemes the key is carried in the payload and verified against.
    fn verify_prehash(&self, message_hash: &B256) -> Result<Bytes, SignatureError>;
}

// ─── ECDSA helpers ───────────────────────────────────────────────────────────

/// Verify a P-256 ECDSA signature against raw uncompressed key bytes (64 bytes: x ‖ y)
/// and a prehashed message. Generic helper used by both P256 and WebAuthn paths.
fn verify_p256_prehash(
    key_data: &[u8],
    sig_r: &U256,
    sig_s: &U256,
    hash: &B256,
) -> Result<(), SignatureError> {
    use elliptic_curve::sec1::FromEncodedPoint;

    if key_data.len() != 64 {
        return Err(SignatureError::InvalidPublicKey);
    }

    let mut uncompressed = vec![0x04];
    uncompressed.extend_from_slice(key_data);
    let point = p256::EncodedPoint::from_bytes(&uncompressed)
        .map_err(|_| SignatureError::InvalidPublicKey)?;
    let affine = p256::AffinePoint::from_encoded_point(&point);
    if affine.is_none().into() {
        return Err(SignatureError::InvalidPublicKey);
    }
    let projective = ProjectivePoint::<NistP256>::from(affine.unwrap());

    let mut sig_bytes = [0u8; 64];
    sig_bytes[..32].copy_from_slice(&sig_r.to_be_bytes::<32>());
    sig_bytes[32..].copy_from_slice(&sig_s.to_be_bytes::<32>());
    let sig = ecdsa::Signature::<NistP256>::from_bytes((&sig_bytes).into())
        .map_err(|e| SignatureError::Ecdsa(e))?;

    let z = FieldBytes::<NistP256>::from_slice(hash.as_ref());
    ecdsa::hazmat::verify_prehashed(&projective, z, &sig)?;
    Ok(())
}

// ─── SignaturePayload ────────────────────────────────────────────────────────

/// The cryptographic signature over a World ID Account transaction.
///
/// The first byte of the RLP-encoded list is the auth type flag, which
/// determines the signature scheme and how the remaining fields are
/// interpreted. The `key` field (where present) carries the raw public
/// key bytes whose hash must match the value proven by the accompanying
/// [`MembershipProof`].
///
/// Schemes:
/// - `0x00` secp256k1 — key recovered from (v, r, s); no explicit key field.
/// - `0x01` P-256 / NIST — raw uncompressed key (64 bytes: x ‖ y) + (r, s).
/// - `0x02` WebAuthn — same as P-256 plus authenticator_data & client_data_json.
/// - `0x03` Ed25519 — raw verifying key (32 bytes) + (R, s).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SignaturePayload {
    Secp256k1 {
        y_parity: u8,
        r: U256,
        s: U256,
    },
    P256 {
        key: Bytes,
        r: U256,
        s: U256,
    },
    WebAuthn {
        key: Bytes,
        authenticator_data: Bytes,
        client_data_json: Bytes,
        r: U256,
        s: U256,
    },
    Ed25519 {
        key: Bytes,
        big_r: FixedBytes<32>,
        s: FixedBytes<32>,
    },
}

impl SignaturePayload {
    /// Returns the auth type flag byte for this signature variant.
    pub fn signature_type(&self) -> u8 {
        match self {
            Self::Secp256k1 { .. } => SIGNATURE_TYPE_SECP256K1,
            Self::P256 { .. } => SIGNATURE_TYPE_P256,
            Self::WebAuthn { .. } => SIGNATURE_TYPE_WEBAUTHN,
            Self::Ed25519 { .. } => SIGNATURE_TYPE_ED25519,
        }
    }

    /// Decode payload fields given a known signature type byte.
    pub fn decode_with_type(sig_type: u8, buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        match sig_type {
            SIGNATURE_TYPE_SECP256K1 => Ok(Self::Secp256k1 {
                y_parity: u8::decode(buf)?,
                r: U256::decode(buf)?,
                s: U256::decode(buf)?,
            }),
            SIGNATURE_TYPE_P256 => Ok(Self::P256 {
                key: Bytes::decode(buf)?,
                r: U256::decode(buf)?,
                s: U256::decode(buf)?,
            }),
            SIGNATURE_TYPE_WEBAUTHN => Ok(Self::WebAuthn {
                key: Bytes::decode(buf)?,
                authenticator_data: Bytes::decode(buf)?,
                client_data_json: Bytes::decode(buf)?,
                r: U256::decode(buf)?,
                s: U256::decode(buf)?,
            }),
            SIGNATURE_TYPE_ED25519 => Ok(Self::Ed25519 {
                key: Bytes::decode(buf)?,
                big_r: FixedBytes::<32>::decode(buf)?,
                s: FixedBytes::<32>::decode(buf)?,
            }),
            _ => Err(alloy_rlp::Error::Custom("unknown signature type")),
        }
    }
}

impl PrehashVerify for SignaturePayload {
    fn verify_prehash(&self, message_hash: &B256) -> Result<Bytes, SignatureError> {
        match self {
            Self::Secp256k1 { y_parity, r, s } => {
                let sig = alloy_primitives::Signature::new(*r, *s, *y_parity != 0);
                let recovered = sig
                    .recover_address_from_prehash(message_hash)
                    .map_err(|_| SignatureError::InvalidPublicKey)?;
                Ok(Bytes::copy_from_slice(recovered.as_ref()))
            }
            Self::P256 { key, r, s } => {
                verify_p256_prehash(key, r, s, message_hash)?;
                Ok(key.clone())
            }
            Self::WebAuthn {
                key,
                authenticator_data,
                client_data_json,
                r,
                s,
            } => {
                let webauthn_hash =
                    compute_webauthn_hash(authenticator_data, client_data_json, message_hash)?;
                verify_p256_prehash(key, r, s, &webauthn_hash)?;
                Ok(key.clone())
            }
            Self::Ed25519 { key, big_r, s } => {
                if key.len() != 32 {
                    return Err(SignatureError::InvalidPublicKey);
                }
                let vk_bytes: &[u8; 32] = key.as_ref().try_into().unwrap();
                let vk = Ed25519VerifyingKey::from_bytes(vk_bytes)
                    .map_err(|_| SignatureError::InvalidPublicKey)?;
                let mut sig_bytes = [0u8; 64];
                sig_bytes[..32].copy_from_slice(big_r.as_ref());
                sig_bytes[32..].copy_from_slice(s.as_ref());
                let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
                vk.verify(message_hash.as_ref(), &sig)
                    .map_err(SignatureError::Ed25519)?;
                Ok(key.clone())
            }
        }
    }
}

// ─── SignaturePayload RLP (list-wrapped: [type, ...fields]) ──────────────────

impl SignaturePayload {
    fn fields_payload_length(&self) -> usize {
        match self {
            Self::Secp256k1 { y_parity, r, s } => y_parity.length() + r.length() + s.length(),
            Self::P256 { key, r, s } => key.length() + r.length() + s.length(),
            Self::WebAuthn {
                key,
                authenticator_data,
                client_data_json,
                r,
                s,
            } => {
                key.length()
                    + authenticator_data.length()
                    + client_data_json.length()
                    + r.length()
                    + s.length()
            }
            Self::Ed25519 { key, big_r, s } => key.length() + big_r.length() + s.length(),
        }
    }

    fn encode_fields(&self, out: &mut dyn BufMut) {
        match self {
            Self::Secp256k1 { y_parity, r, s } => {
                y_parity.encode(out);
                r.encode(out);
                s.encode(out);
            }
            Self::P256 { key, r, s } => {
                key.encode(out);
                r.encode(out);
                s.encode(out);
            }
            Self::WebAuthn {
                key,
                authenticator_data,
                client_data_json,
                r,
                s,
            } => {
                key.encode(out);
                authenticator_data.encode(out);
                client_data_json.encode(out);
                r.encode(out);
                s.encode(out);
            }
            Self::Ed25519 { key, big_r, s } => {
                key.encode(out);
                big_r.encode(out);
                s.encode(out);
            }
        }
    }
}

impl Encodable for SignaturePayload {
    fn encode(&self, out: &mut dyn BufMut) {
        let type_len = self.signature_type().length();
        let fields_len = self.fields_payload_length();
        let payload = type_len + fields_len;
        Header {
            list: true,
            payload_length: payload,
        }
        .encode(out);
        self.signature_type().encode(out);
        self.encode_fields(out);
    }

    fn length(&self) -> usize {
        let type_len = self.signature_type().length();
        let fields_len = self.fields_payload_length();
        let payload = type_len + fields_len;
        payload + length_of_length(payload)
    }
}

impl Decodable for SignaturePayload {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let header = Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let sig_type = u8::decode(buf)?;
        Self::decode_with_type(sig_type, buf)
    }
}

// ─── SignedAuthorization ─────────────────────────────────────────────────────

/// A signed World ID Account authorization.
///
/// Bundles an EIP-7702 [`Authorization`] with the cryptographic proof that the
/// signer's key belongs to the committed key set:
///
/// 1. **`key_commitment`** — the Pedersen vector commitment `C` over the
///    account's [`Keychain`].
/// 2. **`membership_proof`** — a Sigma-protocol proof that the signer's key
///    hash is the component at index `j` of `C`.
/// 3. **`signature`** — the auth-type-tagged signature payload (first byte
///    is the scheme flag).
///
/// Verification:
/// ```text
/// 1. Decode signature to get the auth scheme + signer key bytes.
/// 2. Verify the cryptographic signature (PrehashVerify).
/// 3. Compute key_hash = hash_key(signer_key_bytes).
/// 4. Verify membership_proof against (key_commitment, key_hash, index).
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, RlpEncodable, RlpDecodable)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SignedAuthorization {
    pub inner: Authorization,
    pub key_commitment: KeyCommitment,
    pub membership_proof: pedersen::MembershipProof,
    pub signature: SignaturePayload,
}

impl SignedAuthorization {
    pub fn new(
        inner: Authorization,
        key_commitment: KeyCommitment,
        membership_proof: pedersen::MembershipProof,
        signature: SignaturePayload,
    ) -> Self {
        Self {
            inner,
            key_commitment,
            membership_proof,
            signature,
        }
    }

    pub fn nonce(&self) -> u64 {
        self.inner.nonce
    }

    pub fn chain_id(&self) -> &U256 {
        &self.inner.chain_id
    }

    pub fn address(&self) -> &alloy_primitives::Address {
        &self.inner.address
    }

    /// The auth type flag byte, derived from the signature payload.
    pub fn signature_type(&self) -> u8 {
        self.signature.signature_type()
    }

    /// Verify the signature and membership proof.
    ///
    /// Returns the key hash on success: the BN254 field element whose
    /// membership in the committed key set was proven.
    pub fn verify(&self, message_hash: &B256, num_keys: usize) -> Result<KeyHash, SignatureError> {
        // 1. Verify the cryptographic signature and extract signer key bytes.
        let signer_key_bytes = self.signature.verify_prehash(message_hash)?;

        // 2. Derive the key hash from the raw public key bytes.
        let key_hash = hash_key(&signer_key_bytes);

        // 3. Verify the Pedersen membership proof.
        let commitment = PedersenCommitment::from_compressed(&self.key_commitment.0.0)
            .map_err(|e| SignatureError::Any(e.into()))?;

        if !self
            .membership_proof
            .verify(&commitment, key_hash, num_keys, &GENERATORS)
        {
            return Err(SignatureError::KeyNotInAuthorizedSet);
        }

        Ok(key_hash)
    }
}

impl std::ops::Deref for SignedAuthorization {
    type Target = Authorization;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ─── Ed25519Signature ────────────────────────────────────────────────────────

/// A standalone Ed25519 signature with its verifying key.
#[derive(Clone, Debug, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct Ed25519Signature {
    pub signer: Bytes,
    pub big_r: FixedBytes<32>,
    pub s: FixedBytes<32>,
}

impl Ed25519Signature {
    pub fn from_components(signer: Bytes, r: &[u8], s: &[u8]) -> Self {
        Self {
            signer,
            big_r: FixedBytes::from_slice(r),
            s: FixedBytes::from_slice(s),
        }
    }

    pub fn to_dalek_signature(&self) -> ed25519_dalek::Signature {
        let mut bytes = [0u8; 64];
        bytes[..32].copy_from_slice(self.big_r.as_ref());
        bytes[32..].copy_from_slice(self.s.as_ref());
        ed25519_dalek::Signature::from_bytes(&bytes)
    }
}

impl PrehashVerify for Ed25519Signature {
    fn verify_prehash(&self, message_hash: &B256) -> Result<Bytes, SignatureError> {
        if self.signer.len() != 32 {
            return Err(SignatureError::InvalidPublicKey);
        }
        let vk_bytes: &[u8; 32] = self.signer.as_ref().try_into().unwrap();
        let vk = Ed25519VerifyingKey::from_bytes(vk_bytes)
            .map_err(|_| SignatureError::InvalidPublicKey)?;
        vk.verify(message_hash.as_ref(), &self.to_dalek_signature())
            .map_err(SignatureError::Ed25519)?;
        Ok(self.signer.clone())
    }
}

// ─── WebAuthn helpers ────────────────────────────────────────────────────────

const MIN_AUTH_DATA_LEN: usize = 37;
const UP: u8 = 0x01;
const UV: u8 = 0x04;
const AT: u8 = 0x40;
const ED_FLAG: u8 = 0x80;

#[derive(serde::Deserialize)]
struct ClientDataJson<'a> {
    #[serde(rename = "type")]
    type_field: &'a str,
    challenge: &'a str,
}

/// Validate WebAuthn assertion and compute the signing hash.
pub fn compute_webauthn_hash(
    authenticator_data: &[u8],
    client_data_json: &[u8],
    tx_hash: &B256,
) -> Result<B256, SignatureError> {
    if authenticator_data.len() < MIN_AUTH_DATA_LEN {
        return Err(SignatureError::WebAuthn("authenticator_data too short"));
    }
    let flags = authenticator_data[32];
    if flags & UP == 0 && flags & UV == 0 {
        return Err(SignatureError::WebAuthn("neither UP nor UV flag set"));
    }
    if flags & AT != 0 {
        return Err(SignatureError::WebAuthn("AT flag must not be set"));
    }
    if flags & ED_FLAG != 0 {
        return Err(SignatureError::WebAuthn("ED flag must not be set"));
    }
    let client_data: ClientDataJson<'_> = serde_json::from_slice(client_data_json)
        .map_err(|_| SignatureError::WebAuthn("clientDataJSON is not valid JSON"))?;
    if client_data.type_field != "webauthn.get" {
        return Err(SignatureError::WebAuthn("type must be webauthn.get"));
    }
    if client_data.challenge != BASE64_URL_SAFE_NO_PAD.encode(tx_hash.as_slice()) {
        return Err(SignatureError::WebAuthn("challenge mismatch"));
    }
    let client_data_hash = Sha256::digest(client_data_json);
    let mut final_hasher = Sha256::new();
    final_hasher.update(authenticator_data);
    final_hasher.update(client_data_hash);
    Ok(B256::from_slice(&final_hasher.finalize()))
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, address};

    fn test_key_commitment() -> KeyCommitment {
        KeyCommitment(B256::from([0xAB; 32]))
    }

    fn test_membership_proof() -> pedersen::MembershipProof {
        pedersen::MembershipProof {
            key_index: 0,
            r_point: [0u8; 32],
            responses: vec![],
            blinding_response: U256::ZERO,
        }
    }

    #[test]
    fn test_authorization_rlp_roundtrip() {
        let auth = Authorization {
            chain_id: U256::from(480),
            address: address!("0x000000000000000000000000000000000000001D"),
            nonce: 42,
        };
        let mut buf = Vec::new();
        auth.encode(&mut buf);
        let decoded = Authorization::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(auth, decoded);
    }

    #[test]
    fn test_authorization_signature_hash_deterministic() {
        let auth = Authorization {
            chain_id: U256::from(480),
            address: address!("0x000000000000000000000000000000000000001D"),
            nonce: 0,
        };
        assert_eq!(auth.signature_hash(), auth.signature_hash());
        assert_ne!(auth.signature_hash(), B256::ZERO);
    }

    #[test]
    fn test_signed_authorization_secp256k1_rlp_roundtrip() {
        let signed = SignedAuthorization::new(
            Authorization {
                chain_id: U256::from(1),
                address: Address::ZERO,
                nonce: 0,
            },
            test_key_commitment(),
            test_membership_proof(),
            SignaturePayload::Secp256k1 {
                y_parity: 1,
                r: U256::from(123),
                s: U256::from(456),
            },
        );
        let mut buf = Vec::new();
        signed.encode(&mut buf);
        let decoded = SignedAuthorization::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(signed, decoded);
    }

    #[test]
    fn test_signed_authorization_p256_rlp_roundtrip() {
        let signed = SignedAuthorization::new(
            Authorization {
                chain_id: U256::from(480),
                address: address!("0x000000000000000000000000000000000000001D"),
                nonce: 7,
            },
            test_key_commitment(),
            test_membership_proof(),
            SignaturePayload::P256 {
                key: Bytes::from(vec![0xAB; 64]),
                r: U256::from(111),
                s: U256::from(222),
            },
        );
        let mut buf = Vec::new();
        signed.encode(&mut buf);
        let decoded = SignedAuthorization::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(signed, decoded);
    }

    #[test]
    fn test_signed_authorization_webauthn_rlp_roundtrip() {
        let signed = SignedAuthorization::new(
            Authorization {
                chain_id: U256::from(480),
                address: address!("0x000000000000000000000000000000000000001D"),
                nonce: 1,
            },
            test_key_commitment(),
            test_membership_proof(),
            SignaturePayload::WebAuthn {
                key: Bytes::from(vec![0xAB; 64]),
                authenticator_data: Bytes::from(vec![0xab; 37]),
                client_data_json: Bytes::from(b"{\"test\": true}".to_vec()),
                r: U256::from(333),
                s: U256::from(444),
            },
        );
        let mut buf = Vec::new();
        signed.encode(&mut buf);
        let decoded = SignedAuthorization::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(signed, decoded);
    }

    #[test]
    fn test_signed_authorization_ed25519_rlp_roundtrip() {
        let signed = SignedAuthorization::new(
            Authorization {
                chain_id: U256::from(480),
                address: address!("0x000000000000000000000000000000000000001D"),
                nonce: 2,
            },
            test_key_commitment(),
            test_membership_proof(),
            SignaturePayload::Ed25519 {
                key: Bytes::from(vec![0xCD; 32]),
                big_r: FixedBytes::from([1u8; 32]),
                s: FixedBytes::from([2u8; 32]),
            },
        );
        let mut buf = Vec::new();
        signed.encode(&mut buf);
        let decoded = SignedAuthorization::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(signed, decoded);
    }

    #[test]
    fn test_signed_authorization_accessors() {
        let signed = SignedAuthorization::new(
            Authorization {
                chain_id: U256::from(480),
                address: address!("0x000000000000000000000000000000000000001D"),
                nonce: 5,
            },
            test_key_commitment(),
            test_membership_proof(),
            SignaturePayload::Secp256k1 {
                y_parity: 0,
                r: U256::ZERO,
                s: U256::ZERO,
            },
        );
        assert_eq!(signed.nonce(), 5);
        assert_eq!(*signed.chain_id(), U256::from(480));
        assert_eq!(signed.signature_type(), SIGNATURE_TYPE_SECP256K1);
    }

    #[test]
    fn test_unknown_signature_type_decode_fails() {
        // Encode a valid payload, then try to decode with wrong type byte.
        let payload = SignaturePayload::Secp256k1 {
            y_parity: 0,
            r: U256::from(1),
            s: U256::from(1),
        };
        let mut buf = Vec::new();
        payload.encode_fields(&mut buf);
        assert!(SignaturePayload::decode_with_type(0xFF, &mut buf.as_slice()).is_err());
    }

    #[test]
    fn test_signature_type_consistency() {
        let payloads = [
            SignaturePayload::Secp256k1 {
                y_parity: 0,
                r: U256::ZERO,
                s: U256::ZERO,
            },
            SignaturePayload::P256 {
                key: Bytes::from(vec![0xAB; 64]),
                r: U256::ZERO,
                s: U256::ZERO,
            },
            SignaturePayload::WebAuthn {
                key: Bytes::from(vec![0xAB; 64]),
                authenticator_data: Bytes::new(),
                client_data_json: Bytes::new(),
                r: U256::ZERO,
                s: U256::ZERO,
            },
            SignaturePayload::Ed25519 {
                key: Bytes::from(vec![0xCD; 32]),
                big_r: FixedBytes::ZERO,
                s: FixedBytes::ZERO,
            },
        ];
        for (payload, expected) in payloads.iter().zip([0x00, 0x01, 0x02, 0x03]) {
            assert_eq!(payload.signature_type(), expected);
        }
    }

    #[test]
    fn test_ed25519_signature_rlp_roundtrip() {
        let sig =
            Ed25519Signature::from_components(Bytes::from(vec![0xCD; 32]), &[1u8; 32], &[2u8; 32]);
        let mut buf = Vec::new();
        sig.encode(&mut buf);
        let decoded = Ed25519Signature::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(sig, decoded);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use alloy_primitives::Address;
    use proptest::prelude::*;

    fn arb_u256() -> impl Strategy<Value = U256> {
        any::<[u8; 32]>().prop_map(|b| U256::from_be_bytes(b))
    }

    fn arb_address() -> impl Strategy<Value = Address> {
        any::<[u8; 20]>().prop_map(|b| Address::new(b))
    }

    fn arb_authorization() -> impl Strategy<Value = Authorization> {
        (arb_u256(), arb_address(), any::<u64>()).prop_map(|(chain_id, address, nonce)| {
            Authorization {
                chain_id,
                address,
                nonce,
            }
        })
    }

    fn arb_key_commitment() -> impl Strategy<Value = KeyCommitment> {
        any::<[u8; 32]>().prop_map(|b| KeyCommitment(B256::new(b)))
    }

    fn arb_bytes(len: usize) -> impl Strategy<Value = Bytes> {
        proptest::collection::vec(any::<u8>(), len..=len).prop_map(Bytes::from)
    }

    fn arb_signature_payload() -> impl Strategy<Value = SignaturePayload> {
        prop_oneof![
            (0..=1u8, arb_u256(), arb_u256())
                .prop_map(|(y, r, s)| { SignaturePayload::Secp256k1 { y_parity: y, r, s } }),
            (arb_bytes(64), arb_u256(), arb_u256())
                .prop_map(|(key, r, s)| SignaturePayload::P256 { key, r, s }),
            (
                arb_bytes(64),
                arb_bytes(37),
                arb_bytes(64),
                arb_u256(),
                arb_u256()
            )
                .prop_map(|(key, ad, cd, r, s)| SignaturePayload::WebAuthn {
                    key,
                    authenticator_data: ad,
                    client_data_json: cd,
                    r,
                    s,
                }),
            (arb_bytes(32), any::<[u8; 32]>(), any::<[u8; 32]>()).prop_map(|(key, r, s)| {
                SignaturePayload::Ed25519 {
                    key,
                    big_r: FixedBytes::new(r),
                    s: FixedBytes::new(s),
                }
            }),
        ]
    }

    fn arb_membership_proof() -> impl Strategy<Value = pedersen::MembershipProof> {
        (
            0..=19u8,
            any::<[u8; 32]>(),
            proptest::collection::vec(arb_u256(), 0..=5),
            arb_u256(),
        )
            .prop_map(|(idx, rp, resp, br)| pedersen::MembershipProof {
                key_index: idx,
                r_point: rp,
                responses: resp,
                blinding_response: br,
            })
    }

    fn arb_signed_authorization() -> impl Strategy<Value = SignedAuthorization> {
        (
            arb_authorization(),
            arb_key_commitment(),
            arb_membership_proof(),
            arb_signature_payload(),
        )
            .prop_map(|(auth, kc, mp, sig)| SignedAuthorization::new(auth, kc, mp, sig))
    }

    proptest! {
        #[test]
        fn prop_authorization_rlp_roundtrip(auth in arb_authorization()) {
            let mut buf = Vec::new();
            auth.encode(&mut buf);
            let decoded = Authorization::decode(&mut buf.as_slice()).unwrap();
            prop_assert_eq!(auth, decoded);
        }

        #[test]
        fn prop_signed_authorization_rlp_roundtrip(signed in arb_signed_authorization()) {
            let mut buf = Vec::new();
            signed.encode(&mut buf);
            let decoded = SignedAuthorization::decode(&mut buf.as_slice()).unwrap();
            prop_assert_eq!(signed, decoded);
        }

        #[test]
        fn prop_signature_type_matches_payload(signed in arb_signed_authorization()) {
            prop_assert_eq!(signed.signature_type(), signed.signature.signature_type());
        }

        #[test]
        fn prop_signed_authorization_deref(signed in arb_signed_authorization()) {
            prop_assert_eq!(*signed.chain_id(), signed.inner.chain_id);
            prop_assert_eq!(signed.nonce(), signed.inner.nonce);
            prop_assert_eq!(*signed.address(), signed.inner.address);
        }

        #[test]
        fn prop_signed_authorization_encode_length(signed in arb_signed_authorization()) {
            let mut buf = Vec::new();
            signed.encode(&mut buf);
            prop_assert_eq!(buf.len(), signed.length());
        }

        #[test]
        fn prop_signature_payload_encode_length(payload in arb_signature_payload()) {
            let mut buf = Vec::new();
            payload.encode(&mut buf);
            prop_assert_eq!(buf.len(), payload.length());
        }
    }
}
