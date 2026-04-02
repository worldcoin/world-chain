use crate::transaction::TxWorld;
use alloy_consensus::{
    EthereumTxEnvelope, Sealed, SignableTransaction, Signed, TransactionEnvelope, TxEip1559,
    TxEip2930, TxEip7702, TxLegacy, TxType, TypedTransaction,
    error::{UnsupportedTransactionType, ValueError},
    transaction::SignerRecoverable,
};
use alloy_primitives::{Address, B256, ChainId, Signature};
use alloy_rlp::BufMut;
use core::fmt;
use op_alloy_consensus::TxDeposit;

/// World Chain transaction envelope containing all supported transaction types.
///
/// Transaction types included:
/// - Legacy transactions (type 0x00)
/// - EIP-2930 access list transactions (type 0x01)
/// - EIP-1559 dynamic fee transactions (type 0x02)
/// - EIP-7702 authorization list transactions (type 0x04)
/// - OP Deposit transactions (type 0x7E)
/// - World Chain native transactions (type 0x1D) — WIP-1001
#[derive(Clone, Debug, TransactionEnvelope)]
#[envelope(
    tx_type_name = WorldChainTxType,
    typed = WorldChainTypedTransaction,
    arbitrary_cfg(any(test, feature = "arbitrary")),
    serde_cfg(feature = "serde")
)]
#[allow(clippy::large_enum_variant)]
pub enum WorldChainTxEnvelope {
    /// Legacy transaction (type 0x00)
    #[envelope(ty = 0)]
    Legacy(Signed<TxLegacy>),

    /// EIP-2930 access list transaction (type 0x01)
    #[envelope(ty = 1)]
    Eip2930(Signed<TxEip2930>),

    /// EIP-1559 dynamic fee transaction (type 0x02)
    #[envelope(ty = 2)]
    Eip1559(Signed<TxEip1559>),

    /// EIP-7702 authorization list transaction (type 0x04)
    #[envelope(ty = 4)]
    Eip7702(Signed<TxEip7702>),

    /// OP Deposit transaction (type 0x7E) — no signature, sealed only.
    #[envelope(ty = 0x7E)]
    Deposit(Sealed<TxDeposit>),

    /// World Chain native transaction (type 0x1D) — WIP-1001
    #[envelope(ty = 0x1D)]
    World(Signed<TxWorld>),
}

impl alloy_consensus::InMemorySize for WorldChainTxType {
    fn size(&self) -> usize {
        size_of::<Self>()
    }
}

impl alloy_consensus::transaction::TxHashRef for WorldChainTxEnvelope {
    fn tx_hash(&self) -> &B256 {
        match &self {
            Self::Legacy(tx) => tx.hash(),
            Self::Eip2930(tx) => tx.hash(),
            Self::Eip1559(tx) => tx.hash(),
            Self::Eip7702(tx) => tx.hash(),
            Self::World(tx) => tx.hash(),
            //TODO:
            Self::Deposit(_) => unimplemented!(
                "Deposit transactions do not have a hash until they are processed on-chain"
            ),
            // Self::Deposit(tx) => &tx.hash(),
        }
    }
}

impl SignerRecoverable for WorldChainTxEnvelope {
    fn recover_signer(&self) -> Result<Address, alloy_consensus::crypto::RecoveryError> {
        match self {
            Self::Legacy(tx) => SignerRecoverable::recover_signer(tx),
            Self::Eip2930(tx) => SignerRecoverable::recover_signer(tx),
            Self::Eip1559(tx) => SignerRecoverable::recover_signer(tx),
            Self::Eip7702(tx) => SignerRecoverable::recover_signer(tx),
            Self::Deposit(_) => Ok(Address::ZERO),
            Self::World(tx) => SignerRecoverable::recover_signer(tx),
        }
    }

    fn recover_signer_unchecked(&self) -> Result<Address, alloy_consensus::crypto::RecoveryError> {
        match self {
            Self::Legacy(tx) => SignerRecoverable::recover_signer_unchecked(tx),
            Self::Eip2930(tx) => SignerRecoverable::recover_signer_unchecked(tx),
            Self::Eip1559(tx) => SignerRecoverable::recover_signer_unchecked(tx),
            Self::Eip7702(tx) => SignerRecoverable::recover_signer_unchecked(tx),
            Self::Deposit(_) => Ok(Address::ZERO),
            Self::World(tx) => SignerRecoverable::recover_signer_unchecked(tx),
        }
    }
}

impl fmt::Display for WorldChainTxType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Legacy => write!(f, "Legacy"),
            Self::Eip2930 => write!(f, "EIP-2930"),
            Self::Eip1559 => write!(f, "EIP-1559"),
            Self::Eip7702 => write!(f, "EIP-7702"),
            Self::Deposit => write!(f, "Deposit"),
            Self::World => write!(f, "World (0x1D)"),
        }
    }
}

// ───── Conversions from Ethereum envelope ───────────────────────────────────

impl<Eip4844> TryFrom<EthereumTxEnvelope<Eip4844>> for WorldChainTxEnvelope {
    type Error = ValueError<EthereumTxEnvelope<Eip4844>>;

    fn try_from(value: EthereumTxEnvelope<Eip4844>) -> Result<Self, Self::Error> {
        match value {
            EthereumTxEnvelope::Legacy(tx) => Ok(Self::Legacy(tx)),
            EthereumTxEnvelope::Eip2930(tx) => Ok(Self::Eip2930(tx)),
            EthereumTxEnvelope::Eip1559(tx) => Ok(Self::Eip1559(tx)),
            EthereumTxEnvelope::Eip7702(tx) => Ok(Self::Eip7702(tx)),
            tx @ EthereumTxEnvelope::Eip4844(_) => Err(ValueError::new_static(
                tx,
                "EIP-4844 transactions are not supported on World Chain",
            )),
        }
    }
}

impl From<Signed<TxLegacy>> for WorldChainTxEnvelope {
    fn from(value: Signed<TxLegacy>) -> Self {
        Self::Legacy(value)
    }
}

impl From<Signed<TxEip2930>> for WorldChainTxEnvelope {
    fn from(value: Signed<TxEip2930>) -> Self {
        Self::Eip2930(value)
    }
}

impl From<Signed<TxEip1559>> for WorldChainTxEnvelope {
    fn from(value: Signed<TxEip1559>) -> Self {
        Self::Eip1559(value)
    }
}

impl From<Signed<TxEip7702>> for WorldChainTxEnvelope {
    fn from(value: Signed<TxEip7702>) -> Self {
        Self::Eip7702(value)
    }
}

impl From<Sealed<TxDeposit>> for WorldChainTxEnvelope {
    fn from(value: Sealed<TxDeposit>) -> Self {
        Self::Deposit(value)
    }
}

impl From<Signed<TxWorld>> for WorldChainTxEnvelope {
    fn from(value: Signed<TxWorld>) -> Self {
        Self::World(value)
    }
}

impl SignableTransaction<Signature> for WorldChainTypedTransaction {
    fn set_chain_id(&mut self, chain_id: ChainId) {
        self.as_dyn_signable_mut().set_chain_id(chain_id);
    }

    fn encode_for_signing(&self, out: &mut dyn BufMut) {
        match self {
            Self::Legacy(tx) => tx.encode_for_signing(out),
            Self::Eip2930(tx) => tx.encode_for_signing(out),
            Self::Eip1559(tx) => tx.encode_for_signing(out),
            Self::Eip7702(tx) => tx.encode_for_signing(out),
            Self::World(tx) => tx.encode_for_signing(out),
            // Deposits are not signable
            Self::Deposit(_) => {}
        }
    }

    fn payload_len_for_signature(&self) -> usize {
        match self {
            Self::Legacy(tx) => tx.payload_len_for_signature(),
            Self::Eip2930(tx) => tx.payload_len_for_signature(),
            Self::Eip1559(tx) => tx.payload_len_for_signature(),
            Self::Eip7702(tx) => tx.payload_len_for_signature(),
            Self::World(tx) => tx.payload_len_for_signature(),
            Self::Deposit(_) => 0,
        }
    }
}

impl WorldChainTypedTransaction {
    /// Converts this typed transaction into a signed [`WorldChainTxEnvelope`].
    pub fn into_envelope(self, sig: Signature) -> WorldChainTxEnvelope {
        match self {
            Self::Legacy(tx) => tx.into_signed(sig).into(),
            Self::Eip2930(tx) => tx.into_signed(sig).into(),
            Self::Eip1559(tx) => tx.into_signed(sig).into(),
            Self::Eip7702(tx) => tx.into_signed(sig).into(),
            Self::World(tx) => tx.into_signed(sig).into(),
            Self::Deposit(tx) => Sealed::new(tx).into(),
        }
    }

    /// Returns a dyn mutable reference to the underlying signable transaction.
    pub fn as_dyn_signable_mut(&mut self) -> &mut dyn SignableTransaction<Signature> {
        match self {
            Self::Legacy(tx) => tx,
            Self::Eip2930(tx) => tx,
            Self::Eip1559(tx) => tx,
            Self::Eip7702(tx) => tx,
            Self::World(tx) => tx,
            // Deposits are not signable — this will panic if called on a deposit.
            // Callers should check the variant first.
            Self::Deposit(_) => panic!("Deposit transactions are not signable"),
        }
    }
}

// ───── Ethereum TypedTransaction conversions ────────────────────────────────

impl TryFrom<TypedTransaction> for WorldChainTypedTransaction {
    type Error = UnsupportedTransactionType<TxType>;

    fn try_from(value: TypedTransaction) -> Result<Self, Self::Error> {
        Ok(match value {
            TypedTransaction::Legacy(tx) => Self::Legacy(tx),
            TypedTransaction::Eip2930(tx) => Self::Eip2930(tx),
            TypedTransaction::Eip1559(tx) => Self::Eip1559(tx),
            TypedTransaction::Eip4844(..) => {
                return Err(UnsupportedTransactionType::new(TxType::Eip4844));
            }
            TypedTransaction::Eip7702(tx) => Self::Eip7702(tx),
        })
    }
}

impl From<WorldChainTxEnvelope> for WorldChainTypedTransaction {
    fn from(value: WorldChainTxEnvelope) -> Self {
        match value {
            WorldChainTxEnvelope::Legacy(tx) => Self::Legacy(tx.into_parts().0),
            WorldChainTxEnvelope::Eip2930(tx) => Self::Eip2930(tx.into_parts().0),
            WorldChainTxEnvelope::Eip1559(tx) => Self::Eip1559(tx.into_parts().0),
            WorldChainTxEnvelope::Eip7702(tx) => Self::Eip7702(tx.into_parts().0),
            WorldChainTxEnvelope::Deposit(tx) => Self::Deposit(tx.into_inner()),
            WorldChainTxEnvelope::World(tx) => Self::World(tx.into_parts().0),
        }
    }
}

impl From<TxWorld> for WorldChainTypedTransaction {
    fn from(value: TxWorld) -> Self {
        Self::World(value)
    }
}
