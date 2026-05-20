//! Defines the exact transaction variants that are allowed to be propagated over the eth p2p
//! protocol in world chain.

use crate::transaction::{SignedWip1001, TxWip1001, WorldChainTxEnvelope};
use alloy_consensus::{
    Extended, InMemorySize, SignableTransaction, Signed, TransactionEnvelope, TxEip7702,
    crypto::RecoveryError,
    error::ValueError,
    transaction::{TxEip1559, TxEip2930, TxHashRef, TxLegacy},
};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Address, B256, Signature, TxHash, bytes};
use core::hash::Hash;

/// All possible transactions that can be included in a response to `GetPooledTransactions`.
/// A response to `GetPooledTransactions`. This can include a typed signed transaction, but cannot
/// include a deposit transaction or EIP-4844 transaction.
///
/// The difference between this and the [`WorldChainTxEnvelope`] is that this type does not have the deposit
/// transaction variant, which is not expected to be pooled.
#[derive(Clone, Debug, TransactionEnvelope)]
#[envelope(tx_type_name = WorldChainPooledTxType,)]
pub enum WorldChainPooledTransaction {
    /// An untagged [`TxLegacy`].
    #[envelope(ty = 0)]
    Legacy(Signed<TxLegacy>),
    /// A [`TxEip2930`] transaction tagged with type 1.
    #[envelope(ty = 1)]
    Eip2930(Signed<TxEip2930>),
    /// A [`TxEip1559`] transaction tagged with type 2.
    #[envelope(ty = 2)]
    Eip1559(Signed<TxEip1559>),
    /// A [`TxEip7702`] transaction tagged with type 4.
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
}

impl WorldChainPooledTransaction {
    /// Heavy operation that returns the signature hash over rlp encoded transaction. It is only
    /// for signature signing or signer recovery.
    pub fn signature_hash(&self) -> B256 {
        match self {
            Self::Legacy(tx) => tx.signature_hash(),
            Self::Eip2930(tx) => tx.signature_hash(),
            Self::Eip1559(tx) => tx.signature_hash(),
            Self::Eip7702(tx) => tx.signature_hash(),
            Self::Wip1001(tx) => tx.signature_hash(),
        }
    }

    /// Reference to transaction hash. Used to identify transaction.
    pub fn hash(&self) -> &TxHash {
        match self {
            Self::Legacy(tx) => tx.hash(),
            Self::Eip2930(tx) => tx.hash(),
            Self::Eip1559(tx) => tx.hash(),
            Self::Eip7702(tx) => tx.hash(),
            Self::Wip1001(tx) => tx.hash(),
        }
    }

    /// Returns the signature of the transaction.
    pub fn signature(&self) -> &Signature {
        match self {
            Self::Legacy(tx) => tx.signature(),
            Self::Eip2930(tx) => tx.signature(),
            Self::Eip1559(tx) => tx.signature(),
            Self::Eip7702(tx) => tx.signature(),
            Self::Wip1001(_tx) => {
                unreachable!("WIP1001 txs don't have a standard ECDSA signature!")
            }
        }
    }

    /// This encodes the transaction _without_ the signature, and is only suitable for creating a
    /// hash intended for signing.
    pub fn encode_for_signing(&self, out: &mut dyn bytes::BufMut) {
        match self {
            Self::Legacy(tx) => tx.tx().encode_for_signing(out),
            Self::Eip2930(tx) => tx.tx().encode_for_signing(out),
            Self::Eip1559(tx) => tx.tx().encode_for_signing(out),
            Self::Eip7702(tx) => tx.tx().encode_for_signing(out),
            Self::Wip1001(tx) => {
                <TxWip1001 as SignableTransaction<Signature>>::encode_for_signing(tx.tx(), out)
            }
        }
    }

    /// Converts the transaction into the ethereum [`TxEnvelope`].
    pub fn into_envelope(self) -> WorldChainTxEnvelope {
        match self {
            Self::Legacy(tx) => tx.into(),
            Self::Eip2930(tx) => tx.into(),
            Self::Eip1559(tx) => tx.into(),
            Self::Eip7702(tx) => tx.into(),
            Self::Wip1001(tx) => tx.into(),
        }
    }

    /// Converts the transaction into the optimism [`OpTxEnvelope`].
    pub fn into_world_chain_envelope(self) -> WorldChainTxEnvelope {
        match self {
            Self::Legacy(tx) => tx.into(),
            Self::Eip2930(tx) => tx.into(),
            Self::Eip1559(tx) => tx.into(),
            Self::Eip7702(tx) => tx.into(),
            Self::Wip1001(tx) => tx.into(),
        }
    }

    /// Returns the [`TxLegacy`] variant if the transaction is a legacy transaction.
    pub const fn as_legacy(&self) -> Option<&TxLegacy> {
        match self {
            Self::Legacy(tx) => Some(tx.tx()),
            _ => None,
        }
    }

    /// Returns the [`TxEip2930`] variant if the transaction is an EIP-2930 transaction.
    pub const fn as_eip2930(&self) -> Option<&TxEip2930> {
        match self {
            Self::Eip2930(tx) => Some(tx.tx()),
            _ => None,
        }
    }

    /// Returns the [`TxEip1559`] variant if the transaction is an EIP-1559 transaction.
    pub const fn as_eip1559(&self) -> Option<&TxEip1559> {
        match self {
            Self::Eip1559(tx) => Some(tx.tx()),
            _ => None,
        }
    }

    /// Returns the [`TxEip7702`] variant if the transaction is an EIP-7702 transaction.
    pub const fn as_eip7702(&self) -> Option<&TxEip7702> {
        match self {
            Self::Eip7702(tx) => Some(tx.tx()),
            _ => None,
        }
    }

    /// Returns the [`TxWip1001`] variant if the transaction is an WIP-1001 transaction.
    pub const fn as_wip1001(&self) -> Option<&TxWip1001> {
        match self {
            Self::Wip1001(tx) => Some(tx.tx()),
            _ => None,
        }
    }
}

impl From<Signed<TxLegacy>> for WorldChainPooledTransaction {
    fn from(v: Signed<TxLegacy>) -> Self {
        Self::Legacy(v)
    }
}

impl From<Signed<TxEip2930>> for WorldChainPooledTransaction {
    fn from(v: Signed<TxEip2930>) -> Self {
        Self::Eip2930(v)
    }
}

impl From<Signed<TxEip1559>> for WorldChainPooledTransaction {
    fn from(v: Signed<TxEip1559>) -> Self {
        Self::Eip1559(v)
    }
}

impl From<Signed<TxEip7702>> for WorldChainPooledTransaction {
    fn from(v: Signed<TxEip7702>) -> Self {
        Self::Eip7702(v)
    }
}

impl From<SignedWip1001> for WorldChainPooledTransaction {
    fn from(v: SignedWip1001) -> Self {
        Self::Wip1001(v)
    }
}

impl TxHashRef for WorldChainPooledTransaction {
    fn tx_hash(&self) -> &B256 {
        Self::hash(self)
    }
}

impl InMemorySize for WorldChainPooledTransaction {
    fn size(&self) -> usize {
        core::mem::size_of::<Self>() + self.encode_2718_len()
    }
}

impl alloy_consensus::transaction::SignerRecoverable for WorldChainPooledTransaction {
    fn recover_signer(&self) -> Result<Address, RecoveryError> {
        if let WorldChainPooledTransaction::Wip1001(tx) = self {
            return Ok(tx.tx().world_chain_account);
        }
        let signature_hash = self.signature_hash();
        alloy_consensus::crypto::secp256k1::recover_signer(self.signature(), signature_hash)
    }

    fn recover_signer_unchecked(&self) -> Result<Address, RecoveryError> {
        if let WorldChainPooledTransaction::Wip1001(tx) = self {
            return Ok(tx.tx().world_chain_account);
        }
        let signature_hash = self.signature_hash();
        alloy_consensus::crypto::secp256k1::recover_signer_unchecked(
            self.signature(),
            signature_hash,
        )
    }

    fn recover_unchecked_with_buf(&self, buf: &mut Vec<u8>) -> Result<Address, RecoveryError> {
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
            Self::Wip1001(tx) => Ok(tx.tx().world_chain_account),
        }
    }
}

impl From<WorldChainPooledTransaction> for WorldChainTxEnvelope {
    fn from(tx: WorldChainPooledTransaction) -> Self {
        tx.into_envelope()
    }
}

impl TryFrom<WorldChainTxEnvelope> for WorldChainPooledTransaction {
    type Error = ValueError<WorldChainTxEnvelope>;

    fn try_from(value: WorldChainTxEnvelope) -> Result<Self, Self::Error> {
        value.try_into_pooled()
    }
}

impl<Tx> From<WorldChainPooledTransaction> for Extended<WorldChainTxEnvelope, Tx> {
    fn from(tx: WorldChainPooledTransaction) -> Self {
        Self::BuiltIn(tx.into())
    }
}

impl<Tx> TryFrom<Extended<WorldChainTxEnvelope, Tx>> for WorldChainPooledTransaction {
    type Error = ();

    fn try_from(_tx: Extended<WorldChainTxEnvelope, Tx>) -> Result<Self, Self::Error> {
        match _tx {
            Extended::BuiltIn(inner) => inner.try_into().map_err(|_| ()),
            Extended::Other(_tx) => Err(()),
        }
    }
}
