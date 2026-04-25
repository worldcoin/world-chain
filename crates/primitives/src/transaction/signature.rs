use alloy_primitives::{B256, Bytes, Signature, U256, bytes::BufMut};
use alloy_rlp::{Decodable, Encodable, Header};

/// The EIP-2718 transaction type byte for WIP-1001 transactions.
pub const WIP_1001_TX_TYPE: u8 = 0x1D;

/// Length of a SEC1-compressed secp256k1 public key.
pub const SECP256K1_PUBKEY_LEN: usize = 33;
/// Length of an uncompressed `(x, y)` P-256 public key.
pub const P256_PUBKEY_LEN: usize = 64;
/// Length of a compressed Ed25519 public key.
pub const ED25519_PUBKEY_LEN: usize = 32;

/// Signature scheme for a [`TxWip1001`](crate::transaction::TxWip1001).
///
/// The WIP-1001 envelope is polymorphic over the signing algorithm: each
/// session key may be a secp256k1, P256, WebAuthn (P256 under WebAuthn), or
/// Ed25519 key. The `signature_type` byte and `session_key` bytes live on
/// [`TxWip1001`] (covered by `signing_hash`); this enum carries only the
/// scheme-specific signature payload.
///
/// Ed25519 is intentionally not yet implemented — the wire format is reserved
/// for a follow-on PR.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "signatureType", content = "signaturePayload")]
#[non_exhaustive]
pub enum Wip1001Signature {
    /// secp256k1 ECDSA, `signature_type = 0x00`.
    ///
    /// `signature_payload = rlp([y_parity, r, s])`.
    #[serde(rename = "0x0")]
    Secp256k1(Signature),
    /// NIST P-256 ECDSA, `signature_type = 0x01`.
    ///
    /// `signature_payload = rlp([r, s])`.
    #[serde(rename = "0x1")]
    P256(P256Signature),
    /// WebAuthn (P-256 under WebAuthn), `signature_type = 0x02`.
    ///
    /// `signature_payload = rlp([authenticator_data, client_data_json, r, s])`.
    #[serde(rename = "0x2")]
    WebAuthn(WebAuthnSignature),
}

/// Raw P-256 ECDSA `(r, s)` signature pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct P256Signature {
    /// ECDSA `r` scalar.
    pub r: B256,
    /// ECDSA `s` scalar.
    pub s: B256,
}

/// WebAuthn assertion signature.
///
/// Verification computes `sha256(authenticator_data || sha256(client_data_json))`
/// and feeds the result to P-256 verification using the session pubkey from the
/// transaction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct WebAuthnSignature {
    /// Raw WebAuthn `authenticatorData` (≥ 37 bytes).
    pub authenticator_data: Bytes,
    /// Raw WebAuthn `clientDataJSON`.
    pub client_data_json: Bytes,
    /// ECDSA `r` scalar.
    pub r: B256,
    /// ECDSA `s` scalar.
    pub s: B256,
}

impl Wip1001Signature {
    /// `signature_type` byte for the secp256k1 variant.
    pub const SECP256K1_TYPE: u8 = 0x00;
    /// `signature_type` byte for the raw P-256 variant.
    pub const P256_TYPE: u8 = 0x01;
    /// `signature_type` byte for the WebAuthn variant.
    pub const WEBAUTHN_TYPE: u8 = 0x02;
    /// `signature_type` byte for the EdDSA variant (reserved).
    pub const EDDSA_TYPE: u8 = 0x03;

    /// Returns the `signature_type` tag byte for this variant.
    #[inline]
    pub const fn signature_type(&self) -> u8 {
        match self {
            Self::Secp256k1(_) => Self::SECP256K1_TYPE,
            Self::P256(_) => Self::P256_TYPE,
            Self::WebAuthn(_) => Self::WEBAUTHN_TYPE,
        }
    }

    /// Returns the inner secp256k1 [`Signature`] if this is the `Secp256k1` variant.
    pub const fn as_secp256k1(&self) -> Option<&Signature> {
        match self {
            Self::Secp256k1(sig) => Some(sig),
            _ => None,
        }
    }

    /// Returns the inner [`P256Signature`] if this is the `P256` variant.
    pub const fn as_p256(&self) -> Option<&P256Signature> {
        match self {
            Self::P256(sig) => Some(sig),
            _ => None,
        }
    }

    /// Returns the inner [`WebAuthnSignature`] if this is the `WebAuthn` variant.
    pub const fn as_webauthn(&self) -> Option<&WebAuthnSignature> {
        match self {
            Self::WebAuthn(sig) => Some(sig),
            _ => None,
        }
    }

    /// Length of the RLP-encoded `signature_payload` bytes (no outer string header).
    pub(crate) fn payload_encoded_len(&self) -> usize {
        match self {
            Self::Secp256k1(sig) => {
                let y_parity = sig.v() as u8;
                let list_payload_len = y_parity.length() + sig.r().length() + sig.s().length();
                Header {
                    list: true,
                    payload_length: list_payload_len,
                }
                .length_with_payload()
            }
            Self::P256(sig) => {
                let r = U256::from_be_bytes(sig.r.0);
                let s = U256::from_be_bytes(sig.s.0);
                let list_payload_len = r.length() + s.length();
                Header {
                    list: true,
                    payload_length: list_payload_len,
                }
                .length_with_payload()
            }
            Self::WebAuthn(sig) => {
                let r = U256::from_be_bytes(sig.r.0);
                let s = U256::from_be_bytes(sig.s.0);
                let list_payload_len = sig.authenticator_data.0.length()
                    + sig.client_data_json.0.length()
                    + r.length()
                    + s.length();
                Header {
                    list: true,
                    payload_length: list_payload_len,
                }
                .length_with_payload()
            }
        }
    }

    /// Encodes the `signature_payload` into `out` without an outer RLP string header.
    ///
    /// Per WIP-1001 §Envelope, this writes the per-scheme RLP list directly:
    /// - secp256k1: `rlp([y_parity, r, s])`
    /// - P256: `rlp([r, s])`
    /// - WebAuthn: `rlp([authenticator_data, client_data_json, r, s])`
    pub(crate) fn encode_payload_raw(&self, out: &mut dyn BufMut) {
        match self {
            Self::Secp256k1(sig) => {
                let y_parity = sig.v() as u8;
                let list_payload_len = y_parity.length() + sig.r().length() + sig.s().length();
                Header {
                    list: true,
                    payload_length: list_payload_len,
                }
                .encode(out);
                y_parity.encode(out);
                sig.r().encode(out);
                sig.s().encode(out);
            }
            Self::P256(sig) => {
                let r = U256::from_be_bytes(sig.r.0);
                let s = U256::from_be_bytes(sig.s.0);
                let list_payload_len = r.length() + s.length();
                Header {
                    list: true,
                    payload_length: list_payload_len,
                }
                .encode(out);
                r.encode(out);
                s.encode(out);
            }
            Self::WebAuthn(sig) => {
                let r = U256::from_be_bytes(sig.r.0);
                let s = U256::from_be_bytes(sig.s.0);
                let list_payload_len = sig.authenticator_data.0.length()
                    + sig.client_data_json.0.length()
                    + r.length()
                    + s.length();
                Header {
                    list: true,
                    payload_length: list_payload_len,
                }
                .encode(out);
                sig.authenticator_data.0.encode(out);
                sig.client_data_json.0.encode(out);
                r.encode(out);
                s.encode(out);
            }
        }
    }

    /// Decodes a `signature_payload` byte string, given its `signature_type`.
    ///
    /// The caller has already consumed the outer RLP string header around
    /// `signature_payload`; this reads the raw payload bytes.
    pub(crate) fn decode_payload_raw(ty: u8, buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        match ty {
            Self::SECP256K1_TYPE => {
                let header = Header::decode(buf)?;
                if !header.list {
                    return Err(alloy_rlp::Error::UnexpectedString);
                }
                let start = buf.len();
                let y_parity: u8 = Decodable::decode(buf)?;
                let r: U256 = Decodable::decode(buf)?;
                let s: U256 = Decodable::decode(buf)?;
                let consumed = start - buf.len();
                if consumed != header.payload_length {
                    return Err(alloy_rlp::Error::ListLengthMismatch {
                        expected: header.payload_length,
                        got: consumed,
                    });
                }
                if y_parity > 1 {
                    return Err(alloy_rlp::Error::Custom("invalid y_parity"));
                }
                Ok(Self::Secp256k1(Signature::new(r, s, y_parity != 0)))
            }
            Self::P256_TYPE => {
                let header = Header::decode(buf)?;
                if !header.list {
                    return Err(alloy_rlp::Error::UnexpectedString);
                }
                let start = buf.len();
                let r: U256 = Decodable::decode(buf)?;
                let s: U256 = Decodable::decode(buf)?;
                let consumed = start - buf.len();
                if consumed != header.payload_length {
                    return Err(alloy_rlp::Error::ListLengthMismatch {
                        expected: header.payload_length,
                        got: consumed,
                    });
                }
                Ok(Self::P256(P256Signature {
                    r: B256::from(r.to_be_bytes::<32>()),
                    s: B256::from(s.to_be_bytes::<32>()),
                }))
            }
            Self::WEBAUTHN_TYPE => {
                let header = Header::decode(buf)?;
                if !header.list {
                    return Err(alloy_rlp::Error::UnexpectedString);
                }
                let start = buf.len();
                let authenticator_data: alloy_primitives::bytes::Bytes = Decodable::decode(buf)?;
                let client_data_json: alloy_primitives::bytes::Bytes = Decodable::decode(buf)?;
                let r: U256 = Decodable::decode(buf)?;
                let s: U256 = Decodable::decode(buf)?;
                let consumed = start - buf.len();
                if consumed != header.payload_length {
                    return Err(alloy_rlp::Error::ListLengthMismatch {
                        expected: header.payload_length,
                        got: consumed,
                    });
                }
                Ok(Self::WebAuthn(WebAuthnSignature {
                    authenticator_data: Bytes(authenticator_data),
                    client_data_json: Bytes(client_data_json),
                    r: B256::from(r.to_be_bytes::<32>()),
                    s: B256::from(s.to_be_bytes::<32>()),
                }))
            }
            _ => Err(alloy_rlp::Error::Custom(
                "unsupported wip-1001 signature type",
            )),
        }
    }
}

/// Typed view of a session key, parsed from a `(signature_type, key_data)` pair
/// stored on a [`TxWip1001`](crate::transaction::TxWip1001).
///
/// Lengths are validated per WIP-1001 §Session Keys:
/// - `Secp256k1`: 33 bytes (SEC1-compressed pubkey)
/// - `P256` / `WebAuthn`: 64 bytes (uncompressed `x || y`)
/// - `EdDSA`: 32 bytes (compressed Ed25519 pubkey)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SessionKey {
    /// SEC1-compressed secp256k1 public key (33 bytes).
    Secp256k1([u8; SECP256K1_PUBKEY_LEN]),
    /// Uncompressed P-256 public key, `x || y` (64 bytes).
    P256 {
        /// Affine x coordinate.
        x: B256,
        /// Affine y coordinate.
        y: B256,
    },
    /// Uncompressed P-256 public key for WebAuthn assertions (64 bytes).
    WebAuthn {
        /// Affine x coordinate.
        x: B256,
        /// Affine y coordinate.
        y: B256,
    },
}

/// Parse error for [`SessionKey::from_wire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionKeyError {
    /// `signature_type` is not one of the supported variants.
    #[error("unsupported session key type: 0x{0:02x}")]
    UnsupportedType(u8),
    /// `key_data` length does not match the spec for the declared `signature_type`.
    #[error("invalid session key length: expected {expected}, got {got}")]
    InvalidLength {
        /// Expected length in bytes.
        expected: usize,
        /// Actual length in bytes.
        got: usize,
    },
}

impl SessionKey {
    /// Parse a `(signature_type, key_data)` wire pair into a typed session key.
    pub fn from_wire(signature_type: u8, key_data: &[u8]) -> Result<Self, SessionKeyError> {
        match signature_type {
            Wip1001Signature::SECP256K1_TYPE => {
                if key_data.len() != SECP256K1_PUBKEY_LEN {
                    return Err(SessionKeyError::InvalidLength {
                        expected: SECP256K1_PUBKEY_LEN,
                        got: key_data.len(),
                    });
                }
                let mut buf = [0u8; SECP256K1_PUBKEY_LEN];
                buf.copy_from_slice(key_data);
                Ok(Self::Secp256k1(buf))
            }
            Wip1001Signature::P256_TYPE => {
                let (x, y) = parse_p256_key_data(key_data)?;
                Ok(Self::P256 { x, y })
            }
            Wip1001Signature::WEBAUTHN_TYPE => {
                let (x, y) = parse_p256_key_data(key_data)?;
                Ok(Self::WebAuthn { x, y })
            }
            other => Err(SessionKeyError::UnsupportedType(other)),
        }
    }

    /// Returns the `signature_type` tag byte associated with this session key.
    #[inline]
    pub const fn signature_type(&self) -> u8 {
        match self {
            Self::Secp256k1(_) => Wip1001Signature::SECP256K1_TYPE,
            Self::P256 { .. } => Wip1001Signature::P256_TYPE,
            Self::WebAuthn { .. } => Wip1001Signature::WEBAUTHN_TYPE,
        }
    }
}

fn parse_p256_key_data(key_data: &[u8]) -> Result<(B256, B256), SessionKeyError> {
    if key_data.len() != P256_PUBKEY_LEN {
        return Err(SessionKeyError::InvalidLength {
            expected: P256_PUBKEY_LEN,
            got: key_data.len(),
        });
    }
    Ok((
        B256::from_slice(&key_data[..32]),
        B256::from_slice(&key_data[32..]),
    ))
}
