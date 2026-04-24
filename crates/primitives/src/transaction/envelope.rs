//! The World Chain [EIP-2718] transaction envelope.
//!
//! [EIP-2718]: https://eips.ethereum.org/EIPS/eip-2718

use alloy_consensus::{
    EthereumTxEnvelope, Sealable, Sealed, SignableTransaction, Signed, TransactionEnvelope,
    TxEip1559, TxEip2930, TxEip7702, TxEnvelope, TxLegacy, Typed2718,
    error::ValueError,
    transaction::{RlpEcdsaEncodableTx, TxHashRef},
};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{B256, Bytes, ChainId, Signature, TxHash, bytes::BufMut};
use op_alloy_consensus::{OpTransaction, OpTxEnvelope, TxDeposit};

use crate::transaction::wip_1001::{SignedWip1001, TxWip1001, Wip1001Signature};

/// The World Chain [EIP-2718] transaction envelope.
///
/// This enum is structurally identical to [`op_alloy_consensus::OpTxEnvelope`] and exists as a
/// standalone type so that World Chain-specific transaction variants can be added in-repo without
/// forking `op-alloy-consensus`.
#[derive(Debug, Clone, TransactionEnvelope)]
#[envelope(tx_type_name = WorldChainTxType, typed = WorldChainTypedTransaction)]
pub enum WorldChainTxEnvelope {
    /// An untagged [`TxLegacy`].
    #[envelope(ty = 0)]
    Legacy(Signed<TxLegacy>),
    /// A [`TxEip2930`] tagged with type 1.
    #[envelope(ty = 1)]
    Eip2930(Signed<TxEip2930>),
    /// A [`TxEip1559`] tagged with type 2.
    #[envelope(ty = 2)]
    Eip1559(Signed<TxEip1559>),
    /// A [`TxEip7702`] tagged with type 4.
    #[envelope(ty = 4)]
    Eip7702(Signed<TxEip7702>),
    /// A [`TxWip1001`] tagged with type 0x1D.
    ///
    /// Wraps [`Signed<TxWip1001, Wip1001Signature>`] via [`SignedWip1001`], a
    /// thin local newtype that satisfies Rust's orphan rule for the
    /// `Encodable2718`/`Decodable2718` impls. See `wips/wip-1001.md` for the
    /// full specification.
    #[envelope(ty = 29, typed = TxWip1001)]
    Wip1001(SignedWip1001),
    /// A [`TxDeposit`] tagged with type 0x7E.
    #[envelope(ty = 126)]
    #[serde(serialize_with = "op_alloy_consensus::serde_deposit_tx_rpc")]
    Deposit(Sealed<TxDeposit>),
}

impl OpTransaction for WorldChainTxEnvelope {
    fn is_deposit(&self) -> bool {
        self.is_deposit()
    }

    fn as_deposit(&self) -> Option<&Sealed<TxDeposit>> {
        self.as_deposit()
    }
}

impl AsRef<Self> for WorldChainTxEnvelope {
    fn as_ref(&self) -> &Self {
        self
    }
}

impl From<Signed<TxLegacy>> for WorldChainTxEnvelope {
    fn from(v: Signed<TxLegacy>) -> Self {
        Self::Legacy(v)
    }
}

impl From<Signed<TxEip2930>> for WorldChainTxEnvelope {
    fn from(v: Signed<TxEip2930>) -> Self {
        Self::Eip2930(v)
    }
}

impl From<Signed<TxEip1559>> for WorldChainTxEnvelope {
    fn from(v: Signed<TxEip1559>) -> Self {
        Self::Eip1559(v)
    }
}

impl From<Signed<TxEip7702>> for WorldChainTxEnvelope {
    fn from(v: Signed<TxEip7702>) -> Self {
        Self::Eip7702(v)
    }
}

impl From<SignedWip1001> for WorldChainTxEnvelope {
    fn from(v: SignedWip1001) -> Self {
        Self::Wip1001(v)
    }
}

impl From<Signed<TxWip1001, Wip1001Signature>> for WorldChainTxEnvelope {
    fn from(v: Signed<TxWip1001, Wip1001Signature>) -> Self {
        Self::Wip1001(SignedWip1001::new(v))
    }
}

impl From<TxDeposit> for WorldChainTxEnvelope {
    fn from(v: TxDeposit) -> Self {
        v.seal_slow().into()
    }
}

impl From<Sealed<TxDeposit>> for WorldChainTxEnvelope {
    fn from(v: Sealed<TxDeposit>) -> Self {
        Self::Deposit(v)
    }
}

impl From<Signed<WorldChainTypedTransaction>> for WorldChainTxEnvelope {
    fn from(value: Signed<WorldChainTypedTransaction>) -> Self {
        let (tx, sig, hash) = value.into_parts();
        match tx {
            WorldChainTypedTransaction::Legacy(tx_legacy) => {
                Self::Legacy(Signed::new_unchecked(tx_legacy, sig, hash))
            }
            WorldChainTypedTransaction::Eip2930(tx_eip2930) => {
                Self::Eip2930(Signed::new_unchecked(tx_eip2930, sig, hash))
            }
            WorldChainTypedTransaction::Eip1559(tx_eip1559) => {
                Self::Eip1559(Signed::new_unchecked(tx_eip1559, sig, hash))
            }
            WorldChainTypedTransaction::Eip7702(tx_eip7702) => {
                Self::Eip7702(Signed::new_unchecked(tx_eip7702, sig, hash))
            }
            // Default `Signature` is secp256k1; wrap it into the WIP-1001 signature enum.
            WorldChainTypedTransaction::Wip1001(tx_wip) => Self::Wip1001(SignedWip1001::new(
                Signed::new_unchecked(tx_wip, Wip1001Signature::Secp256k1(sig), hash),
            )),
            WorldChainTypedTransaction::Deposit(tx) => {
                Self::Deposit(Sealed::new_unchecked(tx, hash))
            }
        }
    }
}

impl From<(WorldChainTypedTransaction, Signature)> for WorldChainTxEnvelope {
    fn from(value: (WorldChainTypedTransaction, Signature)) -> Self {
        Self::new_unhashed(value.0, value.1)
    }
}

impl<T> TryFrom<EthereumTxEnvelope<T>> for WorldChainTxEnvelope {
    type Error = EthereumTxEnvelope<T>;

    fn try_from(value: EthereumTxEnvelope<T>) -> Result<Self, Self::Error> {
        Self::try_from_eth_envelope(value)
    }
}

impl TryFrom<WorldChainTxEnvelope> for TxEnvelope {
    type Error = ValueError<WorldChainTxEnvelope>;

    fn try_from(value: WorldChainTxEnvelope) -> Result<Self, Self::Error> {
        value.try_into_eth_envelope()
    }
}

impl From<OpTxEnvelope> for WorldChainTxEnvelope {
    fn from(value: OpTxEnvelope) -> Self {
        match value {
            OpTxEnvelope::Legacy(tx) => Self::Legacy(tx),
            OpTxEnvelope::Eip2930(tx) => Self::Eip2930(tx),
            OpTxEnvelope::Eip1559(tx) => Self::Eip1559(tx),
            OpTxEnvelope::Eip7702(tx) => Self::Eip7702(tx),
            OpTxEnvelope::Deposit(tx) => Self::Deposit(tx),
        }
    }
}

impl TryFrom<WorldChainTxEnvelope> for OpTxEnvelope {
    type Error = ValueError<WorldChainTxEnvelope>;

    fn try_from(value: WorldChainTxEnvelope) -> Result<Self, Self::Error> {
        match value {
            WorldChainTxEnvelope::Legacy(tx) => Ok(Self::Legacy(tx)),
            WorldChainTxEnvelope::Eip2930(tx) => Ok(Self::Eip2930(tx)),
            WorldChainTxEnvelope::Eip1559(tx) => Ok(Self::Eip1559(tx)),
            WorldChainTxEnvelope::Eip7702(tx) => Ok(Self::Eip7702(tx)),
            WorldChainTxEnvelope::Deposit(tx) => Ok(Self::Deposit(tx)),
            tx @ WorldChainTxEnvelope::Wip1001(_) => Err(ValueError::new(
                tx,
                "WIP-1001 transactions cannot be converted to an Optimism transaction",
            )),
        }
    }
}

impl WorldChainTxEnvelope {
    /// Creates a new enveloped transaction from the given transaction, signature and hash.
    ///
    /// Caution: This assumes the given hash is the correct transaction hash.
    pub fn new_unchecked(
        transaction: WorldChainTypedTransaction,
        signature: Signature,
        hash: B256,
    ) -> Self {
        Signed::new_unchecked(transaction, signature, hash).into()
    }

    /// Creates a new signed transaction from the given typed transaction and signature without the
    /// hash.
    ///
    /// Note: this only calculates the hash on the first [`WorldChainTxEnvelope::hash`] call.
    pub fn new_unhashed(transaction: WorldChainTypedTransaction, signature: Signature) -> Self {
        transaction.into_signed(signature).into()
    }

    /// Returns true if the transaction is a legacy transaction.
    #[inline]
    pub const fn is_legacy(&self) -> bool {
        matches!(self, Self::Legacy(_))
    }

    /// Returns true if the transaction is an EIP-2930 transaction.
    #[inline]
    pub const fn is_eip2930(&self) -> bool {
        matches!(self, Self::Eip2930(_))
    }

    /// Returns true if the transaction is an EIP-1559 transaction.
    #[inline]
    pub const fn is_eip1559(&self) -> bool {
        matches!(self, Self::Eip1559(_))
    }

    /// Returns true if the transaction is a WIP-1001 transaction.
    #[inline]
    pub const fn is_wip1001(&self) -> bool {
        matches!(self, Self::Wip1001(_))
    }

    /// Returns true if the transaction is a deposit transaction.
    #[inline]
    pub const fn is_deposit(&self) -> bool {
        matches!(self, Self::Deposit(_))
    }

    /// Returns true if the transaction is a system transaction.
    #[inline]
    pub const fn is_system_transaction(&self) -> bool {
        match self {
            Self::Deposit(tx) => tx.inner().is_system_transaction,
            _ => false,
        }
    }

    /// Returns the [`TxLegacy`] variant if the transaction is a legacy transaction.
    pub const fn as_legacy(&self) -> Option<&Signed<TxLegacy>> {
        match self {
            Self::Legacy(tx) => Some(tx),
            _ => None,
        }
    }

    /// Returns the [`TxEip2930`] variant if the transaction is an EIP-2930 transaction.
    pub const fn as_eip2930(&self) -> Option<&Signed<TxEip2930>> {
        match self {
            Self::Eip2930(tx) => Some(tx),
            _ => None,
        }
    }

    /// Returns the [`TxEip1559`] variant if the transaction is an EIP-1559 transaction.
    pub const fn as_eip1559(&self) -> Option<&Signed<TxEip1559>> {
        match self {
            Self::Eip1559(tx) => Some(tx),
            _ => None,
        }
    }

    /// Returns the [`TxWip1001`] variant if the transaction is a WIP-1001 transaction.
    pub const fn as_wip1001(&self) -> Option<&SignedWip1001> {
        match self {
            Self::Wip1001(tx) => Some(tx),
            _ => None,
        }
    }

    /// Returns the [`TxDeposit`] variant if the transaction is a deposit transaction.
    pub const fn as_deposit(&self) -> Option<&Sealed<TxDeposit>> {
        match self {
            Self::Deposit(tx) => Some(tx),
            _ => None,
        }
    }

    /// Return the reference to signature.
    ///
    /// Returns `None` for deposit transactions (unsigned) and for WIP-1001
    /// transactions (which use [`Wip1001Signature`], obtainable via
    /// [`Self::as_wip1001`]).
    pub const fn signature(&self) -> Option<&Signature> {
        match self {
            Self::Legacy(tx) => Some(tx.signature()),
            Self::Eip2930(tx) => Some(tx.signature()),
            Self::Eip1559(tx) => Some(tx.signature()),
            Self::Eip7702(tx) => Some(tx.signature()),
            Self::Wip1001(_) | Self::Deposit(_) => None,
        }
    }

    /// Return the [`WorldChainTxType`] of the inner txn.
    pub const fn tx_type(&self) -> WorldChainTxType {
        match self {
            Self::Legacy(_) => WorldChainTxType::Legacy,
            Self::Eip2930(_) => WorldChainTxType::Eip2930,
            Self::Eip1559(_) => WorldChainTxType::Eip1559,
            Self::Eip7702(_) => WorldChainTxType::Eip7702,
            Self::Wip1001(_) => WorldChainTxType::Wip1001,
            Self::Deposit(_) => WorldChainTxType::Deposit,
        }
    }

    /// Returns the inner transaction hash.
    pub fn hash(&self) -> &B256 {
        match self {
            Self::Legacy(tx) => tx.hash(),
            Self::Eip1559(tx) => tx.hash(),
            Self::Eip2930(tx) => tx.hash(),
            Self::Eip7702(tx) => tx.hash(),
            Self::Wip1001(tx) => tx.hash(),
            Self::Deposit(tx) => tx.hash_ref(),
        }
    }

    /// Returns the inner transaction hash.
    pub fn tx_hash(&self) -> B256 {
        *self.hash()
    }

    /// Return the length of the inner txn, including type byte length.
    pub fn eip2718_encoded_length(&self) -> usize {
        match self {
            Self::Legacy(t) => t.eip2718_encoded_length(),
            Self::Eip2930(t) => t.eip2718_encoded_length(),
            Self::Eip1559(t) => t.eip2718_encoded_length(),
            Self::Eip7702(t) => t.eip2718_encoded_length(),
            Self::Wip1001(t) => t.encode_2718_len(),
            Self::Deposit(t) => t.eip2718_encoded_length(),
        }
    }

    /// Attempts to convert the World Chain variant into an ethereum [`TxEnvelope`].
    ///
    /// Returns the envelope as error if it is a variant unsupported on ethereum
    /// (e.g. [`TxDeposit`] or [`TxWip1001`]).
    pub fn try_into_eth_envelope(self) -> Result<TxEnvelope, ValueError<Self>> {
        match self {
            Self::Legacy(tx) => Ok(tx.into()),
            Self::Eip2930(tx) => Ok(tx.into()),
            Self::Eip1559(tx) => Ok(tx.into()),
            Self::Eip7702(tx) => Ok(tx.into()),
            tx @ Self::Wip1001(_) => Err(ValueError::new(
                tx,
                "WIP-1001 transactions cannot be converted to ethereum transaction",
            )),
            tx @ Self::Deposit(_) => Err(ValueError::new(
                tx,
                "Deposit transactions cannot be converted to ethereum transaction",
            )),
        }
    }

    /// Attempts to convert an ethereum [`EthereumTxEnvelope`] into the World Chain variant.
    ///
    /// Returns the given envelope as error if [`WorldChainTxEnvelope`] doesn't support the variant
    /// (e.g. EIP-4844).
    #[allow(clippy::result_large_err)]
    pub fn try_from_eth_envelope<T>(
        tx: EthereumTxEnvelope<T>,
    ) -> Result<Self, EthereumTxEnvelope<T>> {
        match tx {
            EthereumTxEnvelope::Legacy(tx) => Ok(tx.into()),
            EthereumTxEnvelope::Eip2930(tx) => Ok(tx.into()),
            EthereumTxEnvelope::Eip1559(tx) => Ok(tx.into()),
            tx @ EthereumTxEnvelope::<T>::Eip4844(_) => Err(tx),
            EthereumTxEnvelope::Eip7702(tx) => Ok(tx.into()),
        }
    }

    /// Returns mutable access to the input bytes.
    ///
    /// Caution: modifying this will cause side-effects on the hash.
    #[doc(hidden)]
    pub const fn input_mut(&mut self) -> &mut Bytes {
        match self {
            Self::Eip1559(tx) => &mut tx.tx_mut().input,
            Self::Eip2930(tx) => &mut tx.tx_mut().input,
            Self::Legacy(tx) => &mut tx.tx_mut().input,
            Self::Eip7702(tx) => &mut tx.tx_mut().input,
            Self::Wip1001(tx) => &mut SignedWip1001::tx_mut(tx).input,
            Self::Deposit(tx) => &mut tx.inner_mut().input,
        }
    }
}

impl TxHashRef for WorldChainTxEnvelope {
    fn tx_hash(&self) -> &B256 {
        Self::hash(self)
    }
}

impl alloy_consensus::transaction::SignerRecoverable for WorldChainTxEnvelope {
    fn recover_signer(
        &self,
    ) -> Result<alloy_primitives::Address, alloy_consensus::crypto::RecoveryError> {
        let (signature, signature_hash) = match self {
            Self::Legacy(tx) => (tx.signature(), tx.signature_hash()),
            Self::Eip2930(tx) => (tx.signature(), tx.signature_hash()),
            Self::Eip1559(tx) => (tx.signature(), tx.signature_hash()),
            Self::Eip7702(tx) => (tx.signature(), tx.signature_hash()),
            Self::Wip1001(tx) => (
                wip1001_secp256k1_sig(tx.signature())?,
                tx.tx().signing_hash(),
            ),
            // Optimism's Deposit transaction does not have a signature. Directly return the
            // `from` address.
            Self::Deposit(tx) => return Ok(tx.from),
        };
        alloy_consensus::crypto::secp256k1::recover_signer(signature, signature_hash)
    }

    fn recover_signer_unchecked(
        &self,
    ) -> Result<alloy_primitives::Address, alloy_consensus::crypto::RecoveryError> {
        let (signature, signature_hash) = match self {
            Self::Legacy(tx) => (tx.signature(), tx.signature_hash()),
            Self::Eip2930(tx) => (tx.signature(), tx.signature_hash()),
            Self::Eip1559(tx) => (tx.signature(), tx.signature_hash()),
            Self::Eip7702(tx) => (tx.signature(), tx.signature_hash()),
            Self::Wip1001(tx) => (
                wip1001_secp256k1_sig(tx.signature())?,
                tx.tx().signing_hash(),
            ),
            // Optimism's Deposit transaction does not have a signature. Directly return the
            // `from` address.
            Self::Deposit(tx) => return Ok(tx.from),
        };
        alloy_consensus::crypto::secp256k1::recover_signer_unchecked(signature, signature_hash)
    }

    fn recover_unchecked_with_buf(
        &self,
        buf: &mut Vec<u8>,
    ) -> Result<alloy_primitives::Address, alloy_consensus::crypto::RecoveryError> {
        match self {
            Self::Legacy(tx) => {
                alloy_consensus::transaction::SignerRecoverable::recover_unchecked_with_buf(tx, buf)
            }
            Self::Eip2930(tx) => {
                alloy_consensus::transaction::SignerRecoverable::recover_unchecked_with_buf(tx, buf)
            }
            Self::Eip1559(tx) => {
                alloy_consensus::transaction::SignerRecoverable::recover_unchecked_with_buf(tx, buf)
            }
            Self::Eip7702(tx) => {
                alloy_consensus::transaction::SignerRecoverable::recover_unchecked_with_buf(tx, buf)
            }
            Self::Wip1001(tx) => {
                let sig = wip1001_secp256k1_sig(tx.signature())?;
                alloy_consensus::crypto::secp256k1::recover_signer_unchecked(
                    sig,
                    tx.tx().signing_hash(),
                )
            }
            Self::Deposit(tx) => Ok(tx.from),
        }
    }
}

/// Projects a [`Wip1001Signature`] down to its secp256k1 [`Signature`] if possible.
///
/// Recovery paths for non-secp256k1 variants (P256, WebAuthn, EdDSA) are out of
/// scope for this implementation and surfaced as a recovery error.
fn wip1001_secp256k1_sig(
    sig: &Wip1001Signature,
) -> Result<&Signature, alloy_consensus::crypto::RecoveryError> {
    sig.as_secp256k1()
        .ok_or_else(alloy_consensus::crypto::RecoveryError::new)
}

impl WorldChainTypedTransaction {
    /// Calculates the signing hash for the transaction.
    ///
    /// Returns `None` if the tx is a deposit transaction.
    pub fn checked_signature_hash(&self) -> Option<B256> {
        match self {
            Self::Legacy(tx) => Some(tx.signature_hash()),
            Self::Eip2930(tx) => Some(tx.signature_hash()),
            Self::Eip1559(tx) => Some(tx.signature_hash()),
            Self::Eip7702(tx) => Some(tx.signature_hash()),
            Self::Wip1001(tx) => Some(tx.signing_hash()),
            Self::Deposit(_) => None,
        }
    }

    /// Calculate the transaction hash for the given signature.
    ///
    /// Note: returns the regular tx hash if this is a deposit variant. For
    /// WIP-1001 transactions the signature is interpreted as the secp256k1
    /// variant of [`Wip1001Signature`].
    pub fn tx_hash(&self, signature: &Signature) -> TxHash {
        match self {
            Self::Legacy(tx) => tx.tx_hash(signature),
            Self::Eip2930(tx) => tx.tx_hash(signature),
            Self::Eip1559(tx) => tx.tx_hash(signature),
            Self::Eip7702(tx) => tx.tx_hash(signature),
            Self::Wip1001(tx) => tx.tx_hash(&Wip1001Signature::Secp256k1(*signature)),
            Self::Deposit(tx) => tx.tx_hash(),
        }
    }

    /// Convenience function to convert this typed transaction into a [`WorldChainTxEnvelope`].
    ///
    /// Note: if this is a [`WorldChainTypedTransaction::Deposit`] variant, the signature will be
    /// ignored.
    pub fn into_envelope(self, signature: Signature) -> WorldChainTxEnvelope {
        self.into_signed(signature).into()
    }
}

impl RlpEcdsaEncodableTx for WorldChainTypedTransaction {
    fn rlp_encoded_fields_length(&self) -> usize {
        match self {
            Self::Legacy(tx) => tx.rlp_encoded_fields_length(),
            Self::Eip2930(tx) => tx.rlp_encoded_fields_length(),
            Self::Eip1559(tx) => tx.rlp_encoded_fields_length(),
            Self::Eip7702(tx) => tx.rlp_encoded_fields_length(),
            Self::Wip1001(tx) => tx.rlp_encoded_fields_length(),
            Self::Deposit(tx) => deposit_rlp_encoded_fields_length(tx),
        }
    }

    fn rlp_encode_fields(&self, out: &mut dyn alloy_rlp::BufMut) {
        match self {
            Self::Legacy(tx) => tx.rlp_encode_fields(out),
            Self::Eip2930(tx) => tx.rlp_encode_fields(out),
            Self::Eip1559(tx) => tx.rlp_encode_fields(out),
            Self::Eip7702(tx) => tx.rlp_encode_fields(out),
            Self::Wip1001(tx) => tx.rlp_encode_fields(out),
            Self::Deposit(tx) => deposit_rlp_encode_fields(tx, out),
        }
    }

    fn eip2718_encode_with_type(&self, signature: &Signature, _ty: u8, out: &mut dyn BufMut) {
        match self {
            Self::Legacy(tx) => tx.eip2718_encode_with_type(signature, tx.ty(), out),
            Self::Eip2930(tx) => tx.eip2718_encode_with_type(signature, tx.ty(), out),
            Self::Eip1559(tx) => tx.eip2718_encode_with_type(signature, tx.ty(), out),
            Self::Eip7702(tx) => tx.eip2718_encode_with_type(signature, tx.ty(), out),
            Self::Wip1001(tx) => tx.eip2718_encode(&Wip1001Signature::Secp256k1(*signature), out),
            Self::Deposit(tx) => tx.encode_2718(out),
        }
    }

    fn eip2718_encode(&self, signature: &Signature, out: &mut dyn BufMut) {
        match self {
            Self::Legacy(tx) => tx.eip2718_encode(signature, out),
            Self::Eip2930(tx) => tx.eip2718_encode(signature, out),
            Self::Eip1559(tx) => tx.eip2718_encode(signature, out),
            Self::Eip7702(tx) => tx.eip2718_encode(signature, out),
            Self::Wip1001(tx) => tx.eip2718_encode(&Wip1001Signature::Secp256k1(*signature), out),
            Self::Deposit(tx) => tx.encode_2718(out),
        }
    }

    fn network_encode_with_type(&self, signature: &Signature, _ty: u8, out: &mut dyn BufMut) {
        match self {
            Self::Legacy(tx) => tx.network_encode_with_type(signature, tx.ty(), out),
            Self::Eip2930(tx) => tx.network_encode_with_type(signature, tx.ty(), out),
            Self::Eip1559(tx) => tx.network_encode_with_type(signature, tx.ty(), out),
            Self::Eip7702(tx) => tx.network_encode_with_type(signature, tx.ty(), out),
            Self::Wip1001(tx) => {
                wip1001_network_encode(tx, &Wip1001Signature::Secp256k1(*signature), out)
            }
            Self::Deposit(tx) => tx.network_encode(out),
        }
    }

    fn network_encode(&self, signature: &Signature, out: &mut dyn BufMut) {
        match self {
            Self::Legacy(tx) => tx.network_encode(signature, out),
            Self::Eip2930(tx) => tx.network_encode(signature, out),
            Self::Eip1559(tx) => tx.network_encode(signature, out),
            Self::Eip7702(tx) => tx.network_encode(signature, out),
            Self::Wip1001(tx) => {
                wip1001_network_encode(tx, &Wip1001Signature::Secp256k1(*signature), out)
            }
            Self::Deposit(tx) => tx.network_encode(out),
        }
    }

    fn tx_hash_with_type(&self, signature: &Signature, _ty: u8) -> TxHash {
        match self {
            Self::Legacy(tx) => tx.tx_hash_with_type(signature, tx.ty()),
            Self::Eip2930(tx) => tx.tx_hash_with_type(signature, tx.ty()),
            Self::Eip1559(tx) => tx.tx_hash_with_type(signature, tx.ty()),
            Self::Eip7702(tx) => tx.tx_hash_with_type(signature, tx.ty()),
            Self::Wip1001(tx) => tx.tx_hash(&Wip1001Signature::Secp256k1(*signature)),
            Self::Deposit(tx) => tx.tx_hash(),
        }
    }

    fn tx_hash(&self, signature: &Signature) -> TxHash {
        match self {
            Self::Legacy(tx) => tx.tx_hash(signature),
            Self::Eip2930(tx) => tx.tx_hash(signature),
            Self::Eip1559(tx) => tx.tx_hash(signature),
            Self::Eip7702(tx) => tx.tx_hash(signature),
            Self::Wip1001(tx) => tx.tx_hash(&Wip1001Signature::Secp256k1(*signature)),
            Self::Deposit(tx) => tx.tx_hash(),
        }
    }
}

/// Wraps a [`TxWip1001`] EIP-2718 encoding with an outer RLP byte-string header,
/// matching the legacy-style `network_encode` used by [`RlpEcdsaEncodableTx`].
fn wip1001_network_encode(tx: &TxWip1001, signature: &Wip1001Signature, out: &mut dyn BufMut) {
    use alloy_rlp::Header;
    let payload_length = tx.eip2718_encoded_length(signature);
    Header {
        list: false,
        payload_length,
    }
    .encode(out);
    tx.eip2718_encode(signature, out);
}

impl SignableTransaction<Signature> for WorldChainTypedTransaction {
    fn set_chain_id(&mut self, chain_id: ChainId) {
        match self {
            Self::Legacy(tx) => tx.set_chain_id(chain_id),
            Self::Eip2930(tx) => tx.set_chain_id(chain_id),
            Self::Eip1559(tx) => tx.set_chain_id(chain_id),
            Self::Eip7702(tx) => tx.set_chain_id(chain_id),
            Self::Wip1001(tx) => {
                <TxWip1001 as SignableTransaction<Wip1001Signature>>::set_chain_id(tx, chain_id)
            }
            Self::Deposit(_) => {}
        }
    }

    fn encode_for_signing(&self, out: &mut dyn BufMut) {
        match self {
            Self::Legacy(tx) => tx.encode_for_signing(out),
            Self::Eip2930(tx) => tx.encode_for_signing(out),
            Self::Eip1559(tx) => tx.encode_for_signing(out),
            Self::Eip7702(tx) => tx.encode_for_signing(out),
            Self::Wip1001(tx) => {
                <TxWip1001 as SignableTransaction<Wip1001Signature>>::encode_for_signing(tx, out)
            }
            Self::Deposit(_) => {}
        }
    }

    fn payload_len_for_signature(&self) -> usize {
        match self {
            Self::Legacy(tx) => tx.payload_len_for_signature(),
            Self::Eip2930(tx) => tx.payload_len_for_signature(),
            Self::Eip1559(tx) => tx.payload_len_for_signature(),
            Self::Eip7702(tx) => tx.payload_len_for_signature(),
            Self::Wip1001(tx) => {
                <TxWip1001 as SignableTransaction<Wip1001Signature>>::payload_len_for_signature(tx)
            }
            Self::Deposit(_) => 0,
        }
    }

    fn into_signed(self, signature: Signature) -> Signed<Self, Signature>
    where
        Self: Sized,
    {
        let hash = self.tx_hash(&signature);
        Signed::new_unchecked(self, signature, hash)
    }
}

/// Length of the RLP-encoded fields of a [`TxDeposit`], without a list header.
///
/// Mirrors the crate-private `TxDeposit::rlp_encoded_fields_length` in `op-alloy-consensus`.
/// See: <https://github.com/ethereum-optimism/specs/blob/main/specs/protocol/deposits.md#the-deposited-transaction-type>.
fn deposit_rlp_encoded_fields_length(tx: &TxDeposit) -> usize {
    use alloy_rlp::Encodable;
    tx.source_hash.length()
        + tx.from.length()
        + tx.to.length()
        + tx.mint.length()
        + tx.value.length()
        + tx.gas_limit.length()
        + tx.is_system_transaction.length()
        + tx.input.0.length()
}

/// RLP-encodes the fields of a [`TxDeposit`] into the given buffer, without a list header.
///
/// Mirrors the crate-private `TxDeposit::rlp_encode_fields` in `op-alloy-consensus`.
fn deposit_rlp_encode_fields(tx: &TxDeposit, out: &mut dyn alloy_rlp::BufMut) {
    use alloy_rlp::Encodable;
    tx.source_hash.encode(out);
    tx.from.encode(out);
    tx.to.encode(out);
    tx.mint.encode(out);
    tx.value.encode(out);
    tx.gas_limit.encode(out);
    tx.is_system_transaction.encode(out);
    tx.input.encode(out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::transaction::SignerRecoverable;
    use alloy_eips::{Decodable2718, eip2930::AccessList};
    use alloy_primitives::{Address, U256, address, hex};
    use alloy_rlp::Encodable as _;
    use alloy_signer_local::PrivateKeySigner;
    use op_alloy_network::TxSignerSync;

    fn sample_eip1559() -> TxEip1559 {
        TxEip1559 {
            chain_id: 480,
            nonce: 1,
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 2_000_000_000,
            gas_limit: 21_000,
            to: Address::with_last_byte(0xAB).into(),
            value: U256::from(7u64),
            input: Bytes::default(),
            access_list: AccessList::default(),
        }
    }

    fn sample_wip1001() -> TxWip1001 {
        TxWip1001 {
            chain_id: 480,
            nonce: 3,
            max_priority_fee_per_gas: 2_000_000_000,
            max_fee_per_gas: 4_000_000_000,
            gas_limit: 100_000,
            to: address!("6069a6c32cf691f5982febae4faf8a6f3ab2f0f6").into(),
            value: U256::from(42u64),
            input: hex!("deadbeef").into(),
            access_list: AccessList::default(),
            keyring: address!("000000000000000000000000000000000000001d"),
        }
    }

    fn sign_wip1001(signer: &PrivateKeySigner, tx: TxWip1001) -> SignedWip1001 {
        // Sign the WIP-1001 signing hash via the `SignableTransaction<Signature>`
        // bridge and wrap the secp256k1 result as a Wip1001Signature.
        let mut tx = tx;
        let signature = signer
            .sign_transaction_sync(&mut tx)
            .expect("signing works");
        SignedWip1001::new_signed(tx, Wip1001Signature::Secp256k1(signature))
    }

    #[test]
    fn envelope_wip1001_variant_accessors() {
        let signer = PrivateKeySigner::random();
        let signed = sign_wip1001(&signer, sample_wip1001());
        let envelope: WorldChainTxEnvelope = signed.clone().into();

        assert!(envelope.is_wip1001());
        assert!(!envelope.is_eip1559());
        assert!(!envelope.is_deposit());
        assert_eq!(envelope.tx_type(), WorldChainTxType::Wip1001);
        assert_eq!(envelope.hash(), signed.hash());
        assert_eq!(envelope.eip2718_encoded_length(), signed.encode_2718_len());
        assert!(
            envelope.signature().is_none(),
            "use as_wip1001 for WIP-1001 sig"
        );
        let via_accessor = envelope.as_wip1001().expect("is wip1001");
        assert_eq!(via_accessor, &signed);
    }

    #[test]
    fn envelope_wip1001_eip2718_round_trip() {
        let signer = PrivateKeySigner::random();
        let signed = sign_wip1001(&signer, sample_wip1001());
        let envelope: WorldChainTxEnvelope = signed.clone().into();

        let mut buf = Vec::new();
        envelope.encode_2718(&mut buf);
        assert_eq!(buf[0], crate::transaction::wip_1001::WIP_1001_TX_TYPE);

        let decoded =
            WorldChainTxEnvelope::decode_2718(&mut buf.as_slice()).expect("envelope decode_2718");
        assert_eq!(decoded.hash(), envelope.hash());
        let decoded_wip = decoded.as_wip1001().expect("is wip1001");
        assert_eq!(decoded_wip.tx(), signed.tx());
        assert_eq!(decoded_wip.signature(), signed.signature());
    }

    #[test]
    fn envelope_wip1001_network_round_trip() {
        use alloy_rlp::Decodable as _;

        let signer = PrivateKeySigner::random();
        let signed = sign_wip1001(&signer, sample_wip1001());
        let envelope: WorldChainTxEnvelope = signed.clone().into();

        // network_encode wraps the 2718 bytes in an outer byte-string header.
        let mut buf = Vec::new();
        envelope.encode(&mut buf);
        let decoded =
            WorldChainTxEnvelope::decode(&mut buf.as_slice()).expect("envelope network decode");
        assert_eq!(decoded.hash(), envelope.hash());
    }

    #[test]
    fn envelope_wip1001_signer_recoverable() {
        let signer = PrivateKeySigner::random();
        let expected = signer.address();
        let signed = sign_wip1001(&signer, sample_wip1001());
        let envelope: WorldChainTxEnvelope = signed.into();

        let recovered = envelope.recover_signer().expect("recover");
        assert_eq!(recovered, expected);
    }

    #[test]
    fn envelope_wip1001_try_into_eth_envelope_rejected() {
        let signer = PrivateKeySigner::random();
        let signed = sign_wip1001(&signer, sample_wip1001());
        let envelope: WorldChainTxEnvelope = signed.into();

        let err = envelope
            .try_into_eth_envelope()
            .expect_err("wip1001 must not convert to eth envelope");
        assert!(err.to_string().contains("WIP-1001"));
    }

    #[test]
    fn envelope_wip1001_try_into_op_envelope_rejected() {
        let signer = PrivateKeySigner::random();
        let signed = sign_wip1001(&signer, sample_wip1001());
        let envelope: WorldChainTxEnvelope = signed.into();

        let err =
            OpTxEnvelope::try_from(envelope).expect_err("wip1001 must not convert to op envelope");
        assert!(err.to_string().contains("WIP-1001"));
    }

    #[test]
    fn envelope_eip1559_still_round_trips() {
        // Regression: make sure adding the WIP-1001 variant didn't disturb the
        // existing `Signed<T>` variants' RLP/2718 behavior.
        let signer = PrivateKeySigner::random();
        let mut tx = sample_eip1559();
        let signature = signer
            .sign_transaction_sync(&mut tx)
            .expect("signing works");
        let signed = Signed::new_unhashed(tx, signature);
        let envelope = WorldChainTxEnvelope::from(signed.clone());

        let mut buf = Vec::new();
        envelope.encode_2718(&mut buf);
        let decoded = WorldChainTxEnvelope::decode_2718(&mut buf.as_slice()).expect("decode_2718");
        assert!(decoded.is_eip1559());
        assert_eq!(decoded.hash(), envelope.hash());

        let recovered = envelope.recover_signer().expect("recover");
        assert_eq!(recovered, signer.address());
    }

    #[test]
    fn envelope_wip1001_via_typed_transaction_path() {
        // Exercise the `Signed<WorldChainTypedTransaction>` -> `WorldChainTxEnvelope`
        // conversion for the WIP-1001 variant: the default secp256k1 Signature
        // should be wrapped as `Wip1001Signature::Secp256k1`.
        let signer = PrivateKeySigner::random();
        let tx = sample_wip1001();
        let mut typed = WorldChainTypedTransaction::Wip1001(tx.clone());
        let signature = signer
            .sign_transaction_sync(&mut typed)
            .expect("signing works");
        let envelope: WorldChainTxEnvelope = Signed::new_unhashed(typed, signature).into();

        let wip = envelope.as_wip1001().expect("is wip1001");
        assert_eq!(wip.tx(), &tx);
        match wip.signature() {
            Wip1001Signature::Secp256k1(inner) => assert_eq!(*inner, signature),
        }
        assert_eq!(envelope.recover_signer().unwrap(), signer.address());
    }
}
