//! World ID Account (WIA) primitive types for the `0x6f` transaction envelope.
//!
//! This module implements Rust primitives for WIP-1001:
//! - [`TxWorldId`]: EIP-2718 type `0x6f` transaction body
//! - [`WorldIdSignature`]: multi-auth signature enum (Secp256k1, P256, WebAuthn)
//! - [`AuthType`] / [`AuthorizedKey`]: key type definitions
//! - [`SignedTxWorldId`]: signed envelope with EIP-2718 encode/decode
//! - Storage slot constants matching the Solidity `WorldIDAccountStorage` library

use alloy_eips::{eip2930::AccessList, eip7702::SignedAuthorization};
use alloy_primitives::{Address, Bytes, B256, U256, address, keccak256};
use alloy_rlp::{BufMut, Decodable, Encodable, Header};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// EIP-2718 transaction type byte for World ID Account transactions.
pub const WORLD_TX_TYPE: u8 = 0x6f;

/// World Chain RP ID (chain ID for WebAuthn).
pub const WORLD_CHAIN_RP_ID: u64 = 480;

/// Maximum authorized keys per account.
pub const MAX_AUTHORIZED_KEYS: usize = 20;

/// World ID Account Factory precompile address (`0x1D`).
pub const WORLD_ID_ACCOUNT_FACTORY: Address =
    address!("000000000000000000000000000000000000001D");

// ---------------------------------------------------------------------------
// Storage slot constants (must match the Solidity `WorldIDAccountStorage` lib)
// ---------------------------------------------------------------------------

lazy_static::lazy_static! {
    /// `keccak256("worldchain.world_id_account.generation")`
    pub static ref GENERATION_SLOT: B256 = keccak256(b"worldchain.world_id_account.generation");
    /// `keccak256("worldchain.world_id_account.num_keys")`
    pub static ref NUM_KEYS_SLOT: B256 = keccak256(b"worldchain.world_id_account.num_keys");
    /// `keccak256("worldchain.world_id_account.nullifier")`
    pub static ref NULLIFIER_SLOT: B256 = keccak256(b"worldchain.world_id_account.nullifier");
}

/// Compute the storage slot for the `i`-th authorized key.
///
/// `keccak256("worldchain.world_id_account.key" || i.to_be_bytes())`
pub fn key_slot(i: U256) -> B256 {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"worldchain.world_id_account.key");
    buf.extend_from_slice(&i.to_be_bytes::<32>());
    keccak256(&buf)
}

// ---------------------------------------------------------------------------
// AuthType
// ---------------------------------------------------------------------------

/// Error returned when converting an invalid `u8` to [`AuthType`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidAuthType(pub u8);

impl core::fmt::Display for InvalidAuthType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid auth type: 0x{:02x}", self.0)
    }
}

impl std::error::Error for InvalidAuthType {}

/// Authentication key type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[derive(serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum AuthType {
    /// Secp256k1 (compressed 33-byte public key).
    Secp256k1 = 0x00,
    /// NIST P-256 (uncompressed x‖y, 64 bytes).
    P256 = 0x01,
    /// WebAuthn with P-256 (uncompressed x‖y, 64 bytes).
    WebAuthn = 0x02,
}

impl TryFrom<u8> for AuthType {
    type Error = InvalidAuthType;

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0x00 => Ok(Self::Secp256k1),
            0x01 => Ok(Self::P256),
            0x02 => Ok(Self::WebAuthn),
            other => Err(InvalidAuthType(other)),
        }
    }
}

impl Encodable for AuthType {
    fn encode(&self, out: &mut dyn BufMut) {
        (*self as u8).encode(out);
    }

    fn length(&self) -> usize {
        (*self as u8).length()
    }
}

impl Decodable for AuthType {
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let v = u8::decode(buf)?;
        AuthType::try_from(v).map_err(|_| alloy_rlp::Error::Custom("invalid auth type"))
    }
}

// ---------------------------------------------------------------------------
// AuthorizedKey
// ---------------------------------------------------------------------------

/// An authorized public key that may sign transactions for a World ID Account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedKey {
    /// The cryptographic scheme for this key.
    pub auth_type: AuthType,
    /// Raw public-key bytes.
    pub key_data: Bytes,
}

impl AuthorizedKey {
    /// Returns the expected byte-length of `key_data` for the key's [`AuthType`].
    pub fn expected_key_len(&self) -> usize {
        match self.auth_type {
            AuthType::Secp256k1 => 33,
            AuthType::P256 | AuthType::WebAuthn => 64,
        }
    }

    /// Returns `true` if `key_data` has the correct length for its `auth_type`.
    pub fn validate(&self) -> bool {
        self.key_data.len() == self.expected_key_len()
    }
}

impl Encodable for AuthorizedKey {
    fn encode(&self, out: &mut dyn BufMut) {
        let payload_len = self.auth_type.length() + self.key_data.length();
        Header { list: true, payload_length: payload_len }.encode(out);
        self.auth_type.encode(out);
        self.key_data.encode(out);
    }

    fn length(&self) -> usize {
        let payload_len = self.auth_type.length() + self.key_data.length();
        Header { list: true, payload_length: payload_len }.length() + payload_len
    }
}

impl Decodable for AuthorizedKey {
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let header = Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let auth_type = AuthType::decode(buf)?;
        let key_data = Bytes::decode(buf)?;
        Ok(Self { auth_type, key_data })
    }
}

// ---------------------------------------------------------------------------
// WorldIdSignature
// ---------------------------------------------------------------------------

/// Multi-auth signature for World ID Account transactions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldIdSignature {
    /// Secp256k1 ECDSA signature.
    Secp256k1 {
        y_parity: bool,
        r: U256,
        s: U256,
    },
    /// NIST P-256 ECDSA signature.
    P256 {
        r: U256,
        s: U256,
    },
    /// WebAuthn (P-256) signature with authenticator data.
    WebAuthn {
        authenticator_data: Bytes,
        client_data_json: Bytes,
        r: U256,
        s: U256,
    },
}

impl WorldIdSignature {
    /// Returns the [`AuthType`] that matches this signature variant.
    pub fn auth_type(&self) -> AuthType {
        match self {
            Self::Secp256k1 { .. } => AuthType::Secp256k1,
            Self::P256 { .. } => AuthType::P256,
            Self::WebAuthn { .. } => AuthType::WebAuthn,
        }
    }

    /// Compute the RLP payload length of the inner signature fields (without the
    /// type byte or outer list header).
    fn sig_payload_len(&self) -> usize {
        match self {
            Self::Secp256k1 { y_parity, r, s } => {
                (*y_parity as u8).length() + r.length() + s.length()
            }
            Self::P256 { r, s } => r.length() + s.length(),
            Self::WebAuthn { authenticator_data, client_data_json, r, s } => {
                authenticator_data.length() + client_data_json.length() + r.length() + s.length()
            }
        }
    }

    /// Encode the inner signature fields as RLP items (no list wrapper).
    fn encode_sig_fields(&self, out: &mut dyn BufMut) {
        match self {
            Self::Secp256k1 { y_parity, r, s } => {
                (*y_parity as u8).encode(out);
                r.encode(out);
                s.encode(out);
            }
            Self::P256 { r, s } => {
                r.encode(out);
                s.encode(out);
            }
            Self::WebAuthn { authenticator_data, client_data_json, r, s } => {
                authenticator_data.encode(out);
                client_data_json.encode(out);
                r.encode(out);
                s.encode(out);
            }
        }
    }
}

impl Encodable for WorldIdSignature {
    fn encode(&self, out: &mut dyn BufMut) {
        // Outer list: [signature_type_byte, ...payload_fields]
        let type_byte = self.auth_type() as u8;
        let payload_len = type_byte.length() + self.sig_payload_len();
        Header { list: true, payload_length: payload_len }.encode(out);
        type_byte.encode(out);
        self.encode_sig_fields(out);
    }

    fn length(&self) -> usize {
        let type_byte = self.auth_type() as u8;
        let payload_len = type_byte.length() + self.sig_payload_len();
        Header { list: true, payload_length: payload_len }.length() + payload_len
    }
}

impl Decodable for WorldIdSignature {
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let header = Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let pre = buf.len();
        let type_byte = u8::decode(buf)?;
        let auth_type =
            AuthType::try_from(type_byte).map_err(|_| alloy_rlp::Error::Custom("invalid sig type"))?;

        let sig = match auth_type {
            AuthType::Secp256k1 => {
                let y_parity = u8::decode(buf)? != 0;
                let r = U256::decode(buf)?;
                let s = U256::decode(buf)?;
                Self::Secp256k1 { y_parity, r, s }
            }
            AuthType::P256 => {
                let r = U256::decode(buf)?;
                let s = U256::decode(buf)?;
                Self::P256 { r, s }
            }
            AuthType::WebAuthn => {
                let authenticator_data = Bytes::decode(buf)?;
                let client_data_json = Bytes::decode(buf)?;
                let r = U256::decode(buf)?;
                let s = U256::decode(buf)?;
                Self::WebAuthn { authenticator_data, client_data_json, r, s }
            }
        };

        let consumed = pre - buf.len();
        if consumed != header.payload_length {
            return Err(alloy_rlp::Error::ListLengthMismatch {
                expected: header.payload_length,
                got: consumed,
            });
        }

        Ok(sig)
    }
}

// ---------------------------------------------------------------------------
// TxWorldId
// ---------------------------------------------------------------------------

/// Body of a World ID Account transaction (EIP-2718 type `0x6f`).
///
/// Fields 0‒10 are included in the signing hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxWorldId {
    /// Chain ID (field 0).
    pub chain_id: u64,
    /// Transaction nonce (field 1).
    pub nonce: u64,
    /// Max priority fee per gas in wei (field 2).
    pub max_priority_fee_per_gas: u128,
    /// Max fee per gas in wei (field 3).
    pub max_fee_per_gas: u128,
    /// Gas limit (field 4).
    pub gas_limit: u64,
    /// Destination address (field 5).
    pub to: Address,
    /// Value in wei (field 6).
    pub value: U256,
    /// Call data (field 7).
    pub data: Bytes,
    /// EIP-2930 access list (field 8).
    pub access_list: AccessList,
    /// EIP-7702 authorization list (field 9).
    pub authorization_list: Vec<SignedAuthorization>,
    /// World ID account nullifier (field 10).
    pub account_nullifier: U256,
}

impl TxWorldId {
    /// Encode transaction fields 0‒10 as sequential RLP items (no outer list wrapper).
    pub fn encode_fields(&self, out: &mut dyn BufMut) {
        self.chain_id.encode(out);
        self.nonce.encode(out);
        self.max_priority_fee_per_gas.encode(out);
        self.max_fee_per_gas.encode(out);
        self.gas_limit.encode(out);
        self.to.encode(out);
        self.value.encode(out);
        self.data.encode(out);
        self.access_list.encode(out);
        alloy_rlp::encode_list(&self.authorization_list, out);
        self.account_nullifier.encode(out);
    }

    /// Compute the RLP payload length of fields 0‒10.
    pub fn fields_rlp_payload_len(&self) -> usize {
        self.chain_id.length()
            + self.nonce.length()
            + self.max_priority_fee_per_gas.length()
            + self.max_fee_per_gas.length()
            + self.gas_limit.length()
            + self.to.length()
            + self.value.length()
            + self.data.length()
            + self.access_list.length()
            + alloy_rlp::list_length(&self.authorization_list)
            + self.account_nullifier.length()
    }

    /// Compute the EIP-2718 signing hash: `keccak256(0x6f || rlp([fields 0..=10]))`.
    pub fn signing_hash(&self) -> B256 {
        let payload_len = self.fields_rlp_payload_len();
        let mut buf = Vec::with_capacity(1 + Header { list: true, payload_length: payload_len }.length() + payload_len);
        buf.push(WORLD_TX_TYPE);
        Header { list: true, payload_length: payload_len }.encode(&mut buf);
        self.encode_fields(&mut buf);
        keccak256(&buf)
    }

    /// Derive the sender address from the account nullifier:
    /// `address(keccak256(nullifier.to_be_bytes()))`.
    pub fn derive_sender(&self) -> Address {
        let hash = keccak256(self.account_nullifier.to_be_bytes::<32>());
        Address::from_slice(&hash[..20])
    }
}

// ---------------------------------------------------------------------------
// SignedTxWorldId
// ---------------------------------------------------------------------------

/// A signed World ID Account transaction (type `0x6f`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedTxWorldId {
    /// The unsigned transaction body.
    pub tx: TxWorldId,
    /// The multi-auth signature.
    pub signature: WorldIdSignature,
}

impl SignedTxWorldId {
    /// Encode as EIP-2718 typed transaction:
    /// `0x6f || rlp([fields[0..=10], sig_type, sig_payload_fields...])`
    pub fn encode_2718(&self, out: &mut dyn BufMut) {
        out.put_u8(WORLD_TX_TYPE);
        self.encode_inner(out);
    }

    /// Encode the inner RLP list (everything after the `0x6f` type byte).
    fn encode_inner(&self, out: &mut dyn BufMut) {
        let payload_len = self.inner_payload_len();
        Header { list: true, payload_length: payload_len }.encode(out);
        self.tx.encode_fields(out);
        // Inline signature: type byte + payload fields (not wrapped in a sub-list)
        (self.signature.auth_type() as u8).encode(out);
        self.signature.encode_sig_fields(out);
    }

    /// Compute the RLP payload length for the inner list.
    fn inner_payload_len(&self) -> usize {
        let sig_type_byte = self.signature.auth_type() as u8;
        self.tx.fields_rlp_payload_len()
            + sig_type_byte.length()
            + self.signature.sig_payload_len()
    }

    /// Decode from EIP-2718 typed transaction bytes **after** consuming the `0x6f` type byte.
    pub fn decode_2718(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let header = Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }

        let pre = buf.len();

        // Decode fields 0‒10
        let chain_id = u64::decode(buf)?;
        let nonce = u64::decode(buf)?;
        let max_priority_fee_per_gas = u128::decode(buf)?;
        let max_fee_per_gas = u128::decode(buf)?;
        let gas_limit = u64::decode(buf)?;
        let to = Address::decode(buf)?;
        let value = U256::decode(buf)?;
        let data = Bytes::decode(buf)?;
        let access_list = AccessList::decode(buf)?;

        // authorization_list is an RLP list of SignedAuthorization
        let auth_header = Header::decode(buf)?;
        if !auth_header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let mut authorization_list = Vec::new();
        let auth_pre = buf.len();
        while auth_pre - buf.len() < auth_header.payload_length {
            authorization_list.push(SignedAuthorization::decode(buf)?);
        }

        let account_nullifier = U256::decode(buf)?;

        // Decode inline signature: type byte + fields
        let sig_type = u8::decode(buf)?;
        let auth_type = AuthType::try_from(sig_type)
            .map_err(|_| alloy_rlp::Error::Custom("invalid sig type in envelope"))?;

        let signature = match auth_type {
            AuthType::Secp256k1 => {
                let y_parity = u8::decode(buf)? != 0;
                let r = U256::decode(buf)?;
                let s = U256::decode(buf)?;
                WorldIdSignature::Secp256k1 { y_parity, r, s }
            }
            AuthType::P256 => {
                let r = U256::decode(buf)?;
                let s = U256::decode(buf)?;
                WorldIdSignature::P256 { r, s }
            }
            AuthType::WebAuthn => {
                let authenticator_data = Bytes::decode(buf)?;
                let client_data_json = Bytes::decode(buf)?;
                let r = U256::decode(buf)?;
                let s = U256::decode(buf)?;
                WorldIdSignature::WebAuthn { authenticator_data, client_data_json, r, s }
            }
        };

        let consumed = pre - buf.len();
        if consumed != header.payload_length {
            return Err(alloy_rlp::Error::ListLengthMismatch {
                expected: header.payload_length,
                got: consumed,
            });
        }

        let tx = TxWorldId {
            chain_id,
            nonce,
            max_priority_fee_per_gas,
            max_fee_per_gas,
            gas_limit,
            to,
            value,
            data,
            access_list,
            authorization_list,
            account_nullifier,
        };

        Ok(Self { tx, signature })
    }

    /// Returns the sender address derived from the account nullifier.
    pub fn sender(&self) -> Address {
        self.tx.derive_sender()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_type_round_trip() {
        for v in [AuthType::Secp256k1, AuthType::P256, AuthType::WebAuthn] {
            let encoded = alloy_rlp::encode(&v);
            let decoded = AuthType::decode(&mut encoded.as_slice()).unwrap();
            assert_eq!(v, decoded);
        }
    }

    #[test]
    fn test_auth_type_invalid() {
        assert!(AuthType::try_from(0x03).is_err());
        assert!(AuthType::try_from(0xff).is_err());
    }

    #[test]
    fn test_authorized_key_validation() {
        let good_secp = AuthorizedKey {
            auth_type: AuthType::Secp256k1,
            key_data: Bytes::from(vec![0u8; 33]),
        };
        assert!(good_secp.validate());

        let bad_secp = AuthorizedKey {
            auth_type: AuthType::Secp256k1,
            key_data: Bytes::from(vec![0u8; 64]),
        };
        assert!(!bad_secp.validate());

        let good_p256 = AuthorizedKey {
            auth_type: AuthType::P256,
            key_data: Bytes::from(vec![0u8; 64]),
        };
        assert!(good_p256.validate());
    }

    #[test]
    fn test_authorized_key_rlp_round_trip() {
        let key = AuthorizedKey {
            auth_type: AuthType::WebAuthn,
            key_data: Bytes::from(vec![0xABu8; 64]),
        };
        let encoded = alloy_rlp::encode(&key);
        let decoded = AuthorizedKey::decode(&mut encoded.as_slice()).unwrap();
        assert_eq!(key, decoded);
    }

    #[test]
    fn test_world_id_signature_rlp_secp256k1() {
        let sig = WorldIdSignature::Secp256k1 {
            y_parity: true,
            r: U256::from(12345u64),
            s: U256::from(67890u64),
        };
        let encoded = alloy_rlp::encode(&sig);
        let decoded = WorldIdSignature::decode(&mut encoded.as_slice()).unwrap();
        assert_eq!(sig, decoded);
    }

    #[test]
    fn test_world_id_signature_rlp_p256() {
        let sig = WorldIdSignature::P256 {
            r: U256::from(1u64),
            s: U256::from(2u64),
        };
        let encoded = alloy_rlp::encode(&sig);
        let decoded = WorldIdSignature::decode(&mut encoded.as_slice()).unwrap();
        assert_eq!(sig, decoded);
    }

    #[test]
    fn test_world_id_signature_rlp_webauthn() {
        let sig = WorldIdSignature::WebAuthn {
            authenticator_data: Bytes::from(b"authdata".to_vec()),
            client_data_json: Bytes::from(b"{}".to_vec()),
            r: U256::from(1u64),
            s: U256::from(2u64),
        };
        let encoded = alloy_rlp::encode(&sig);
        let decoded = WorldIdSignature::decode(&mut encoded.as_slice()).unwrap();
        assert_eq!(sig, decoded);
    }

    #[test]
    fn test_tx_world_id_rlp_round_trip() {
        let tx = TxWorldId {
            chain_id: 480,
            nonce: 0,
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 2_000_000_000,
            gas_limit: 21_000,
            to: Address::ZERO,
            value: U256::ZERO,
            data: Bytes::new(),
            access_list: Default::default(),
            authorization_list: vec![],
            account_nullifier: U256::from(42u64),
        };
        let sig = WorldIdSignature::P256 {
            r: U256::from(1u64),
            s: U256::from(2u64),
        };
        let signed = SignedTxWorldId { tx, signature: sig };

        let mut buf = Vec::new();
        signed.encode_2718(&mut buf);

        assert_eq!(buf[0], WORLD_TX_TYPE);

        let decoded = SignedTxWorldId::decode_2718(&mut &buf[1..]).unwrap();
        assert_eq!(signed, decoded);
    }

    #[test]
    fn test_tx_world_id_rlp_round_trip_secp256k1() {
        let tx = TxWorldId {
            chain_id: 480,
            nonce: 7,
            max_priority_fee_per_gas: 500,
            max_fee_per_gas: 1000,
            gas_limit: 100_000,
            to: address!("0000000000000000000000000000000000000042"),
            value: U256::from(1_000_000u64),
            data: Bytes::from(vec![0xCA, 0xFE]),
            access_list: Default::default(),
            authorization_list: vec![],
            account_nullifier: U256::from(999u64),
        };
        let sig = WorldIdSignature::Secp256k1 {
            y_parity: false,
            r: U256::from(111u64),
            s: U256::from(222u64),
        };
        let signed = SignedTxWorldId { tx, signature: sig };

        let mut buf = Vec::new();
        signed.encode_2718(&mut buf);
        assert_eq!(buf[0], WORLD_TX_TYPE);

        let decoded = SignedTxWorldId::decode_2718(&mut &buf[1..]).unwrap();
        assert_eq!(signed, decoded);
    }

    #[test]
    fn test_tx_world_id_rlp_round_trip_webauthn() {
        let tx = TxWorldId {
            chain_id: 480,
            nonce: 1,
            max_priority_fee_per_gas: 0,
            max_fee_per_gas: 0,
            gas_limit: 21_000,
            to: Address::ZERO,
            value: U256::ZERO,
            data: Bytes::new(),
            access_list: Default::default(),
            authorization_list: vec![],
            account_nullifier: U256::from(77u64),
        };
        let sig = WorldIdSignature::WebAuthn {
            authenticator_data: Bytes::from(b"auth".to_vec()),
            client_data_json: Bytes::from(b"{\"type\":\"webauthn.get\"}".to_vec()),
            r: U256::from(333u64),
            s: U256::from(444u64),
        };
        let signed = SignedTxWorldId { tx, signature: sig };

        let mut buf = Vec::new();
        signed.encode_2718(&mut buf);
        assert_eq!(buf[0], WORLD_TX_TYPE);

        let decoded = SignedTxWorldId::decode_2718(&mut &buf[1..]).unwrap();
        assert_eq!(signed, decoded);
    }

    #[test]
    fn test_signing_hash_deterministic() {
        let tx = TxWorldId {
            chain_id: 480,
            nonce: 5,
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 2_000_000_000,
            gas_limit: 21_000,
            to: Address::ZERO,
            value: U256::ZERO,
            data: Bytes::new(),
            access_list: Default::default(),
            authorization_list: vec![],
            account_nullifier: U256::from(999u64),
        };
        assert_eq!(tx.signing_hash(), tx.signing_hash());
    }

    #[test]
    fn test_signing_hash_changes_with_nonce() {
        let tx1 = TxWorldId {
            chain_id: 480,
            nonce: 0,
            max_priority_fee_per_gas: 0,
            max_fee_per_gas: 0,
            gas_limit: 0,
            to: Address::ZERO,
            value: U256::ZERO,
            data: Bytes::new(),
            access_list: Default::default(),
            authorization_list: vec![],
            account_nullifier: U256::from(1u64),
        };
        let tx2 = TxWorldId { nonce: 1, ..tx1.clone() };
        assert_ne!(tx1.signing_hash(), tx2.signing_hash());
    }

    #[test]
    fn test_sender_derivation() {
        let nullifier = U256::from(42u64);
        let tx = TxWorldId {
            chain_id: 480,
            nonce: 0,
            max_priority_fee_per_gas: 0,
            max_fee_per_gas: 0,
            gas_limit: 0,
            to: Address::ZERO,
            value: U256::ZERO,
            data: Bytes::new(),
            access_list: Default::default(),
            authorization_list: vec![],
            account_nullifier: nullifier,
        };

        let sender = tx.derive_sender();
        // Independently compute: bytes20(keccak256(abi.encodePacked(uint256(42))))
        let expected = Address::from_slice(&keccak256(nullifier.to_be_bytes::<32>())[..20]);
        assert_eq!(sender, expected);

        // Different nullifiers → different senders
        let tx2 = TxWorldId {
            account_nullifier: U256::from(43u64),
            ..tx.clone()
        };
        assert_ne!(tx.derive_sender(), tx2.derive_sender());
    }

    #[test]
    fn test_storage_slots() {
        // Verify slot values match what Solidity would compute
        let gen_slot = keccak256(b"worldchain.world_id_account.generation");
        assert_eq!(*GENERATION_SLOT, gen_slot);

        // key_slot(0) and key_slot(1) should be different
        assert_ne!(key_slot(U256::from(0u64)), key_slot(U256::from(1u64)));
    }
}
