//! WIP-1001 typed transaction envelope (`0x1D`).
//!
//! See `wips/wip-1001.md` for the full specification. This module implements the
//! transaction body ([`TxWip1001`]), the multi-scheme signature enum
//! ([`Wip1001Signature`]) — scoped to ECDSA over secp256k1 for now, but extensible
//! to the additional variants defined in the spec (P256, WebAuthn, EdDSA) — and
//! the RLP / EIP-2718 codecs required to wire `Signed<TxWip1001, Wip1001Signature>`
//! into [`WorldChainTxEnvelope`](crate::transaction::WorldChainTxEnvelope).
use alloy_consensus::{SignableTransaction, Signed, Transaction, transaction::TxHashable};
use alloy_eips::{
    Decodable2718, Encodable2718, Typed2718,
    eip2718::{Eip2718Error, Eip2718Result, IsTyped2718},
    eip2930::AccessList,
    eip7702::SignedAuthorization,
};
use alloy_primitives::{
    Address, B256, Bytes, ChainId, Signature, TxHash, TxKind, U256, bytes::BufMut, keccak256,
};
use alloy_rlp::{Decodable, Encodable, Header};

use crate::transaction::{WIP_1001_TX_TYPE, Wip1001Signature};

/// A WIP-1001 typed transaction (`0x1D`).
///
/// The transaction is signed by a *session verifier* in the *Key Ring* of a
/// *World Chain Account* — the [`world_chain_account`](Self::world_chain_account) field
/// carries that account's 20-byte address and is the protocol-level sender.
/// Protocol validation authorizes the recovered public key against the
/// predeploy-managed Key Ring of `world_chain_account`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxWip1001 {
    /// EIP-155 chain id.
    #[serde(with = "alloy_serde::quantity")]
    pub chain_id: ChainId,
    /// Session-key nonce at the World ID Account.
    /// (Per-Key-Ring counter incremented on each successful 0x1D execution.)
    #[serde(with = "alloy_serde::quantity")]
    pub nonce: u64,
    /// EIP-1559 priority fee (tip cap).
    #[serde(with = "alloy_serde::quantity")]
    pub max_priority_fee_per_gas: u128,
    /// EIP-1559 fee cap.
    #[serde(with = "alloy_serde::quantity")]
    pub max_fee_per_gas: u128,
    /// Gas limit.
    #[serde(with = "alloy_serde::quantity", rename = "gas", alias = "gasLimit")]
    pub gas_limit: u64,
    /// The World Chain Account.
    pub world_chain_account: Address,
    /// The session verifier.
    pub session_verifier: Address,
    /// Target of the message call, or `Create` for contract creation.
    #[serde(default)]
    pub to: TxKind,
    /// Value transferred with the call.
    pub value: U256,
    /// Calldata / init code.
    pub input: Bytes,
    /// EIP-2930 access list.
    #[serde(default)]
    pub access_list: AccessList,
}

impl TxWip1001 {
    /// Length of the RLP-encoded unsigned fields (positions 0..=10, i.e. the
    /// 11 envelope fields excluding `signature`), without a list header.
    #[inline]
    pub fn rlp_encoded_fields_length(&self) -> usize {
        self.chain_id.length()
            + self.nonce.length()
            + self.max_priority_fee_per_gas.length()
            + self.max_fee_per_gas.length()
            + self.gas_limit.length()
            + self.world_chain_account.length()
            + self.session_verifier.length()
            + self.to.length()
            + self.value.length()
            + self.input.0.length()
            + self.access_list.length()
    }

    /// Encodes the unsigned fields (positions 0..=10) into `out`, without a list header.
    pub fn rlp_encode_fields(&self, out: &mut dyn BufMut) {
        self.chain_id.encode(out);
        self.nonce.encode(out);
        self.max_priority_fee_per_gas.encode(out);
        self.max_fee_per_gas.encode(out);
        self.gas_limit.encode(out);
        self.world_chain_account.encode(out);
        self.session_verifier.encode(out);
        self.to.encode(out);
        self.value.encode(out);
        self.input.0.encode(out);
        self.access_list.encode(out);
    }

    /// Decodes the unsigned fields (positions 0..=10) from RLP bytes, without
    /// a list header.
    pub fn rlp_decode_fields(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        Ok(Self {
            chain_id: Decodable::decode(buf)?,
            nonce: Decodable::decode(buf)?,
            max_priority_fee_per_gas: Decodable::decode(buf)?,
            max_fee_per_gas: Decodable::decode(buf)?,
            gas_limit: Decodable::decode(buf)?,
            world_chain_account: Decodable::decode(buf)?,
            session_verifier: Decodable::decode(buf)?,
            to: Decodable::decode(buf)?,
            value: Decodable::decode(buf)?,
            input: Decodable::decode(buf)?,
            access_list: Decodable::decode(buf)?,
        })
    }

    /// RLP list header for the *unsigned* transaction.
    fn rlp_header(&self) -> Header {
        Header {
            list: true,
            payload_length: self.rlp_encoded_fields_length(),
        }
    }

    /// RLP-encoded length of the *unsigned* transaction (list header + fields).
    fn rlp_encoded_length(&self) -> usize {
        self.rlp_header().length_with_payload()
    }

    /// RLP-encodes the *unsigned* transaction (list of the 11 fields at
    /// positions 0..=10, i.e. all envelope fields except `signature`).
    fn rlp_encode(&self, out: &mut dyn BufMut) {
        self.rlp_header().encode(out);
        self.rlp_encode_fields(out);
    }

    /// Length of the *signed* transaction's RLP payload, i.e. the contents of
    /// the outer list: the 11 unsigned fields plus the opaque `signature`
    /// byte-string at position 12.
    fn signed_payload_length(&self, sig: &Wip1001Signature) -> usize {
        self.rlp_encoded_fields_length() + sig.payload_encoded_len()
    }

    /// RLP list header for the *signed* transaction.
    fn rlp_header_signed(&self, sig: &Wip1001Signature) -> Header {
        Header {
            list: true,
            payload_length: self.signed_payload_length(sig),
        }
    }

    /// RLP-encoded length of the *signed* transaction (list header + fields + signature).
    pub fn rlp_encoded_length_with_signature(&self, sig: &Wip1001Signature) -> usize {
        self.rlp_header_signed(sig).length_with_payload()
    }

    /// RLP-encodes the *signed* transaction per WIP-1001:
    /// `rlp([chainId, nonce, ..., accessList, signature])`. The `signature`
    /// is emitted as a plain RLP byte-string at position 12 with no extra
    /// wrapping.
    pub fn rlp_encode_signed(&self, sig: &Wip1001Signature, out: &mut dyn BufMut) {
        self.rlp_header_signed(sig).encode(out);
        self.rlp_encode_fields(out);
        sig.encode_payload_raw(out);
    }

    /// Decodes the *signed* transaction including the signature.
    pub fn rlp_decode_with_signature(
        buf: &mut &[u8],
    ) -> alloy_rlp::Result<(Self, Wip1001Signature)> {
        let header = Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let remaining = buf.len();
        let tx = Self::rlp_decode_fields(buf)?;
        let sig = Wip1001Signature::decode_payload_raw(buf)?;

        if buf.len() + header.payload_length != remaining {
            return Err(alloy_rlp::Error::ListLengthMismatch {
                expected: header.payload_length,
                got: remaining - buf.len(),
            });
        }
        Ok((tx, sig))
    }

    /// Decodes the signed transaction into a [`Signed<TxWip1001, Wip1001Signature>`].
    pub fn rlp_decode_signed(buf: &mut &[u8]) -> alloy_rlp::Result<Signed<Self, Wip1001Signature>> {
        let (tx, sig) = Self::rlp_decode_with_signature(buf)?;
        let hash = tx.tx_hash(&sig);
        Ok(Signed::new_unchecked(tx, sig, hash))
    }

    /// Length of the EIP-2718 encoding (`type_byte || rlp_encode_signed`).
    pub fn eip2718_encoded_length(&self, sig: &Wip1001Signature) -> usize {
        1 + self.rlp_encoded_length_with_signature(sig)
    }

    /// EIP-2718 encodes the transaction with `type_byte = 0x1D`.
    pub fn eip2718_encode(&self, sig: &Wip1001Signature, out: &mut dyn BufMut) {
        out.put_u8(WIP_1001_TX_TYPE);
        self.rlp_encode_signed(sig, out);
    }

    /// EIP-2718 decodes a signed transaction, asserting the leading type byte
    /// equals `0x1D`.
    pub fn eip2718_decode(buf: &mut &[u8]) -> Eip2718Result<Signed<Self, Wip1001Signature>> {
        if buf.is_empty() {
            return Err(alloy_rlp::Error::InputTooShort.into());
        }
        let ty = buf[0];
        if ty != WIP_1001_TX_TYPE {
            return Err(Eip2718Error::UnexpectedType(ty));
        }
        *buf = &buf[1..];
        // OPT: compute the hash from the original buffer to avoid re-serializing.
        let original = *buf;
        let (tx, sig) = Self::rlp_decode_with_signature(buf)?;
        let consumed = original.len() - buf.len();
        let mut hash_buf = Vec::with_capacity(1 + consumed);
        hash_buf.push(WIP_1001_TX_TYPE);
        hash_buf.extend_from_slice(&original[..consumed]);
        let hash = keccak256(&hash_buf);
        Ok(Signed::new_unchecked(tx, sig, hash))
    }

    /// EIP-2718 decodes with an expected `type_byte`.
    pub fn eip2718_decode_with_type(
        buf: &mut &[u8],
        ty: u8,
    ) -> Eip2718Result<Signed<Self, Wip1001Signature>> {
        if ty != WIP_1001_TX_TYPE {
            return Err(Eip2718Error::UnexpectedType(ty));
        }
        let mut full = Vec::with_capacity(1 + buf.len());
        full.push(ty);
        full.extend_from_slice(buf);
        let mut slice = full.as_slice();
        let res = Self::eip2718_decode(&mut slice)?;
        // Advance the caller's buffer by the number of bytes consumed.
        let consumed = full.len() - slice.len() - 1; // subtract leading type byte
        *buf = &buf[consumed..];
        Ok(res)
    }

    /// Computes the transaction hash: `keccak256(0x1D || rlp_encode_signed)`.
    pub fn tx_hash(&self, sig: &Wip1001Signature) -> TxHash {
        let mut buf = Vec::with_capacity(self.eip2718_encoded_length(sig));
        self.eip2718_encode(sig, &mut buf);
        keccak256(&buf)
    }

    /// Computes the signing hash per WIP-1001:
    /// `keccak256(0x1D || rlp([chainId, nonce, maxPriorityFeePerGas,
    /// maxFeePerGas, gasLimit, account, sessionVerifier, to, value, data,
    /// accessList]))`.
    pub fn signing_hash(&self) -> B256 {
        let mut buf = Vec::with_capacity(1 + self.rlp_encoded_length());
        buf.put_u8(WIP_1001_TX_TYPE);
        self.rlp_encode(&mut buf);
        keccak256(&buf)
    }
}

impl Typed2718 for TxWip1001 {
    fn ty(&self) -> u8 {
        WIP_1001_TX_TYPE
    }
}

impl IsTyped2718 for TxWip1001 {
    fn is_type(type_id: u8) -> bool {
        type_id == WIP_1001_TX_TYPE
    }
}

impl Transaction for TxWip1001 {
    #[inline]
    fn chain_id(&self) -> Option<u64> {
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
        self.to
    }

    #[inline]
    fn is_create(&self) -> bool {
        self.to.is_create()
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
    fn authorization_list(&self) -> Option<&[SignedAuthorization]> {
        None
    }
}

impl SignableTransaction<Wip1001Signature> for TxWip1001 {
    fn set_chain_id(&mut self, chain_id: ChainId) {
        self.chain_id = chain_id;
    }

    fn encode_for_signing(&self, out: &mut dyn BufMut) {
        out.put_u8(WIP_1001_TX_TYPE);
        self.rlp_encode(out);
    }

    fn payload_len_for_signature(&self) -> usize {
        1 + self.rlp_encoded_length()
    }

    fn into_signed(self, signature: Wip1001Signature) -> Signed<Self, Wip1001Signature> {
        let hash = self.tx_hash(&signature);
        Signed::new_unchecked(self, signature, hash)
    }
}

/// Bridge impl so that `WorldChainTypedTransaction: SignableTransaction<Signature>`
impl SignableTransaction<Signature> for TxWip1001 {
    fn set_chain_id(&mut self, chain_id: ChainId) {
        self.chain_id = chain_id;
    }

    fn encode_for_signing(&self, out: &mut dyn BufMut) {
        <Self as SignableTransaction<Wip1001Signature>>::encode_for_signing(self, out);
    }

    fn payload_len_for_signature(&self) -> usize {
        <Self as SignableTransaction<Wip1001Signature>>::payload_len_for_signature(self)
    }

    fn into_signed(self, signature: Signature) -> Signed<Self, Signature> {
        let wip_sig: Wip1001Signature = signature.into();
        let hash = self.tx_hash(&wip_sig);
        Signed::new_unchecked(self, signature, hash)
    }
}

impl TxHashable<Wip1001Signature> for TxWip1001 {
    fn tx_hash_with_type(&self, signature: &Wip1001Signature, _ty: u8) -> TxHash {
        self.tx_hash(signature)
    }
}

impl Encodable for TxWip1001 {
    fn encode(&self, out: &mut dyn BufMut) {
        self.rlp_encode(out);
    }

    fn length(&self) -> usize {
        self.rlp_encoded_length()
    }
}

impl Decodable for TxWip1001 {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let header = Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let remaining = buf.len();
        let this = Self::rlp_decode_fields(buf)?;
        if buf.len() + header.payload_length != remaining {
            return Err(alloy_rlp::Error::UnexpectedLength);
        }
        Ok(this)
    }
}

/// Newtype wrapper around [`Signed<TxWip1001, Wip1001Signature>`].
///
/// `Signed` lives in `alloy-consensus` and so does not satisfy Rust's orphan
/// rule for `impl Encodable2718`/`impl Decodable2718` when specialized over a
/// non-default `Sig` type. [`SignedWip1001`] is a transparent, local wrapper
/// carrying only the type-system discriminator needed for those impls; it
/// [`Deref`](core::ops::Deref)s back to the inner [`Signed`] and converts
/// losslessly in both directions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct SignedWip1001 {
    inner: Signed<TxWip1001, Wip1001Signature>,
}

impl SignedWip1001 {
    /// Wraps the given [`Signed`].
    pub const fn new(inner: Signed<TxWip1001, Wip1001Signature>) -> Self {
        Self { inner }
    }

    /// Signs a [`TxWip1001`] with the given [`Wip1001Signature`] and wraps the result.
    pub fn new_signed(tx: TxWip1001, signature: Wip1001Signature) -> Self {
        Self::new(tx.into_signed(signature))
    }

    /// Returns the inner [`Signed`].
    pub const fn inner(&self) -> &Signed<TxWip1001, Wip1001Signature> {
        &self.inner
    }

    /// Unwraps to the inner [`Signed`].
    pub fn into_inner(self) -> Signed<TxWip1001, Wip1001Signature> {
        self.inner
    }

    /// Reference to the transaction body.
    pub const fn tx(&self) -> &TxWip1001 {
        self.inner.tx()
    }

    /// Mutable reference to the transaction body.
    ///
    /// # Warning
    ///
    /// Modifying the transaction structurally invalidates the signature and
    /// cached hash.
    #[doc(hidden)]
    pub const fn tx_mut(&mut self) -> &mut TxWip1001 {
        self.inner.tx_mut()
    }

    /// Reference to the signature.
    pub const fn signature(&self) -> &Wip1001Signature {
        self.inner.signature()
    }

    /// The cached transaction hash.
    pub fn hash(&self) -> &B256 {
        self.inner.hash()
    }

    /// EIP-2718 decode a signed WIP-1001 transaction.
    pub fn eip2718_decode(buf: &mut &[u8]) -> Eip2718Result<Self> {
        TxWip1001::eip2718_decode(buf).map(Self::new)
    }
}

impl core::ops::Deref for SignedWip1001 {
    type Target = Signed<TxWip1001, Wip1001Signature>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl From<Signed<TxWip1001, Wip1001Signature>> for SignedWip1001 {
    fn from(inner: Signed<TxWip1001, Wip1001Signature>) -> Self {
        Self::new(inner)
    }
}

impl From<SignedWip1001> for Signed<TxWip1001, Wip1001Signature> {
    fn from(value: SignedWip1001) -> Self {
        value.inner
    }
}

impl Typed2718 for SignedWip1001 {
    fn ty(&self) -> u8 {
        WIP_1001_TX_TYPE
    }
}

impl IsTyped2718 for SignedWip1001 {
    fn is_type(type_id: u8) -> bool {
        type_id == WIP_1001_TX_TYPE
    }
}

impl Transaction for SignedWip1001 {
    #[inline]
    fn chain_id(&self) -> Option<u64> {
        self.tx().chain_id()
    }
    #[inline]
    fn nonce(&self) -> u64 {
        self.tx().nonce()
    }
    #[inline]
    fn gas_limit(&self) -> u64 {
        self.tx().gas_limit()
    }
    #[inline]
    fn gas_price(&self) -> Option<u128> {
        self.tx().gas_price()
    }
    #[inline]
    fn max_fee_per_gas(&self) -> u128 {
        self.tx().max_fee_per_gas()
    }
    #[inline]
    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        self.tx().max_priority_fee_per_gas()
    }
    #[inline]
    fn max_fee_per_blob_gas(&self) -> Option<u128> {
        self.tx().max_fee_per_blob_gas()
    }
    #[inline]
    fn priority_fee_or_price(&self) -> u128 {
        self.tx().priority_fee_or_price()
    }
    fn effective_gas_price(&self, base_fee: Option<u64>) -> u128 {
        self.tx().effective_gas_price(base_fee)
    }
    #[inline]
    fn is_dynamic_fee(&self) -> bool {
        self.tx().is_dynamic_fee()
    }
    #[inline]
    fn kind(&self) -> TxKind {
        self.tx().kind()
    }
    #[inline]
    fn is_create(&self) -> bool {
        self.tx().is_create()
    }
    #[inline]
    fn value(&self) -> U256 {
        self.tx().value()
    }
    #[inline]
    fn input(&self) -> &Bytes {
        self.tx().input()
    }
    #[inline]
    fn access_list(&self) -> Option<&AccessList> {
        self.tx().access_list()
    }
    #[inline]
    fn blob_versioned_hashes(&self) -> Option<&[B256]> {
        self.tx().blob_versioned_hashes()
    }
    #[inline]
    fn authorization_list(&self) -> Option<&[SignedAuthorization]> {
        self.tx().authorization_list()
    }
}

impl Encodable2718 for SignedWip1001 {
    fn encode_2718_len(&self) -> usize {
        self.tx().eip2718_encoded_length(self.signature())
    }

    fn encode_2718(&self, out: &mut dyn BufMut) {
        self.tx().eip2718_encode(self.signature(), out);
    }

    fn trie_hash(&self) -> B256 {
        *self.hash()
    }
}

impl Decodable2718 for SignedWip1001 {
    fn typed_decode(ty: u8, buf: &mut &[u8]) -> Eip2718Result<Self> {
        if ty != WIP_1001_TX_TYPE {
            return Err(Eip2718Error::UnexpectedType(ty));
        }
        TxWip1001::rlp_decode_signed(buf)
            .map(Self::new)
            .map_err(Into::into)
    }

    fn fallback_decode(_buf: &mut &[u8]) -> Eip2718Result<Self> {
        // WIP-1001 transactions are always typed; there is no legacy fallback.
        Err(Eip2718Error::UnexpectedType(0))
    }
}

impl Encodable for SignedWip1001 {
    fn encode(&self, out: &mut dyn BufMut) {
        <Self as Encodable2718>::network_encode(self, out)
    }

    fn length(&self) -> usize {
        <Self as Encodable2718>::network_len(self)
    }
}

impl Decodable for SignedWip1001 {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        Ok(<Self as Decodable2718>::network_decode(buf)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, hex};

    fn sample_tx() -> TxWip1001 {
        TxWip1001 {
            chain_id: 480,
            nonce: 0x42,
            max_priority_fee_per_gas: 0x3b9aca00,
            max_fee_per_gas: 0x4a817c800,
            gas_limit: 44386,
            world_chain_account: address!("000000000000000000000000000000000000001d"),
            session_verifier: address!("00000000000000000000000000000000000000aa"),
            to: address!("6069a6c32cf691f5982febae4faf8a6f3ab2f0f6").into(),
            value: U256::from(1u64),
            input: hex!("a22cb465").into(),
            access_list: AccessList::default(),
        }
    }

    fn sample_sig() -> Wip1001Signature {
        // The WIP-1001 envelope does not interpret signature bytes; verification
        // is delegated to the on-chain session verifier contract via EIP-1271
        // (see `wips/wip-1001.md`). Use an arbitrary opaque payload here.
        Wip1001Signature {
            signature: hex!(
                "840cfc572845f5786e702984c2a582528cad4b49b2a10b9db1be7fca9005856525e7109ceb98168d95b09b18bbf6b685130e0562f233877d492b94eee0c5b6d1"
            )
            .into(),
        }
    }

    #[test]
    fn wip1001_signature_payload_round_trip() {
        let sig = sample_sig();
        let mut buf = Vec::new();
        sig.encode_payload_raw(&mut buf);
        assert_eq!(buf.len(), sig.payload_encoded_len());

        let mut slice = buf.as_slice();
        let decoded = Wip1001Signature::decode_payload_raw(&mut slice).expect("decode payload");
        assert!(slice.is_empty());
        assert_eq!(decoded, sig);
    }

    #[test]
    fn wip1001_signed_rlp_round_trip() {
        let tx = sample_tx();
        let sig = sample_sig();

        let mut buf = Vec::new();
        tx.rlp_encode_signed(&sig, &mut buf);
        assert_eq!(buf.len(), tx.rlp_encoded_length_with_signature(&sig));

        let mut slice = buf.as_slice();
        let (decoded_tx, decoded_sig) =
            TxWip1001::rlp_decode_with_signature(&mut slice).expect("decode");
        assert!(slice.is_empty());
        assert_eq!(decoded_tx, tx);
        assert_eq!(decoded_sig, sig);
    }

    #[test]
    fn wip1001_eip2718_round_trip() {
        let tx = sample_tx();
        let sig = sample_sig();

        let mut buf = Vec::new();
        tx.eip2718_encode(&sig, &mut buf);
        assert_eq!(buf[0], WIP_1001_TX_TYPE);
        assert_eq!(buf.len(), tx.eip2718_encoded_length(&sig));

        let mut slice = buf.as_slice();
        let signed = TxWip1001::eip2718_decode(&mut slice).expect("decode 2718");
        assert!(slice.is_empty());
        assert_eq!(signed.tx(), &tx);
        assert_eq!(signed.signature(), &sig);
        assert_eq!(*signed.hash(), tx.tx_hash(&sig));
    }

    #[test]
    fn wip1001_signed_encode_2718_matches_tx_helper() {
        let tx = sample_tx();
        let sig = sample_sig();
        let signed = SignedWip1001::new_signed(tx.clone(), sig.clone());

        let mut via_signed = Vec::new();
        signed.encode_2718(&mut via_signed);

        let mut via_tx = Vec::new();
        tx.eip2718_encode(&sig, &mut via_tx);

        assert_eq!(via_signed, via_tx);
        assert_eq!(signed.encode_2718_len(), via_tx.len());
    }

    #[test]
    fn wip1001_typed_decode_rejects_wrong_type() {
        let tx = sample_tx();
        let sig = sample_sig();
        let mut buf = Vec::new();
        tx.rlp_encode_signed(&sig, &mut buf);

        let mut slice = buf.as_slice();
        let err = <SignedWip1001 as Decodable2718>::typed_decode(0x02, &mut slice).unwrap_err();
        assert!(matches!(err, Eip2718Error::UnexpectedType(0x02)));
    }

    #[test]
    fn wip1001_signed_newtype_round_trip() {
        let tx = sample_tx();
        let sig = sample_sig();
        let signed = SignedWip1001::new_signed(tx.clone(), sig.clone());

        // EIP-2718 round-trip through the newtype.
        let mut buf = Vec::new();
        signed.encode_2718(&mut buf);
        let decoded = SignedWip1001::eip2718_decode(&mut buf.as_slice()).expect("decode");
        assert_eq!(decoded.tx(), &tx);
        assert_eq!(decoded.signature(), &sig);
        assert_eq!(*decoded.hash(), *signed.hash());

        // `Signed` <-> `SignedWip1001` conversions are lossless.
        let inner: Signed<TxWip1001, Wip1001Signature> = signed.clone().into_inner();
        assert_eq!(inner.tx(), &tx);
        let back = SignedWip1001::from(inner);
        assert_eq!(*back.hash(), *signed.hash());
    }

    #[test]
    fn wip1001_signing_hash_excludes_signature() {
        let tx = sample_tx();
        let sig1 = sample_sig();
        let sig2 = Wip1001Signature {
            signature: hex!("aabbccddeeff").into(),
        };
        assert_eq!(tx.signing_hash(), tx.signing_hash());
        let h1 = tx.tx_hash(&sig1);
        let h2 = tx.tx_hash(&sig2);
        assert_ne!(h1, h2, "tx hash depends on signature");
        // Signing hash excludes signature fields.
        let signing = tx.signing_hash();
        assert_ne!(signing, h1);
        assert_ne!(signing, h2);
    }

    #[test]
    fn wip1001_tx_type_byte() {
        let tx = sample_tx();
        assert_eq!(<TxWip1001 as Typed2718>::ty(&tx), 0x1D);
        assert!(<TxWip1001 as IsTyped2718>::is_type(0x1D));
        assert!(!<TxWip1001 as IsTyped2718>::is_type(0x02));
    }

    #[test]
    fn wip1001_into_signed_sets_hash() {
        let tx = sample_tx();
        let sig = sample_sig();
        let signed: Signed<TxWip1001, Wip1001Signature> = tx.clone().into_signed(sig.clone());
        assert_eq!(signed.tx(), &tx);
        assert_eq!(signed.signature(), &sig);
        assert_eq!(*signed.hash(), tx.tx_hash(&sig));
    }

    /// Pins the WIP-1001 wire format: the 12th list item is a plain RLP
    /// byte-string carrying the opaque signature bytes — no inner list
    /// wrapper, no outer byte-string wrapping.
    #[test]
    fn wip1001_signed_wire_format_signature_is_plain_byte_string() {
        let tx = sample_tx();
        let sig = Wip1001Signature {
            signature: hex!("deadbeef").into(),
        };

        let mut buf = Vec::new();
        tx.rlp_encode_signed(&sig, &mut buf);

        let mut slice = buf.as_slice();
        let outer = Header::decode(&mut slice).expect("outer list header");
        assert!(outer.list, "outer envelope must be an RLP list");

        // Consume the 11 unsigned fields (positions 0..=10).
        let _ = TxWip1001::rlp_decode_fields(&mut slice).expect("decode unsigned fields");

        // The remaining bytes are exactly the signature item. For a 4-byte
        // payload, RLP encodes it as `0x84 || payload`.
        assert_eq!(
            slice,
            hex!("84deadbeef").as_slice(),
            "signature must be a plain RLP byte-string at position 12 \
             (head 0x84 + 4 payload bytes); any list head (0xc0+) or extra \
             wrapping indicates a spec violation",
        );
    }
}
