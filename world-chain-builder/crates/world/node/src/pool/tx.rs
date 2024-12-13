use std::sync::Arc;

use alloy_consensus::{BlobTransactionSidecar, BlobTransactionValidationError, Transaction};
use alloy_primitives::TxHash;
use alloy_rpc_types::erc4337::ConditionalOptions;
use reth::{
    core::primitives::SignedTransaction,
    transaction_pool::{
        EthBlobTransactionSidecar, EthPoolTransaction, EthPooledTransaction, PoolTransaction,
    },
};
use reth_primitives::{
    transaction::{SignedTransactionIntoRecoveredExt, TryFromRecoveredTransactionError},
    Transaction as RethTransaction,
};
use reth_primitives::{
    PooledTransactionsElement, PooledTransactionsElementEcRecovered, RecoveredTx, TransactionSigned,
};
use revm_primitives::{AccessList, Address, KzgSettings, TxKind, U256};

use crate::pbh::payload::PbhPayload;

pub trait WorldChainPoolTransaction: EthPoolTransaction {
    fn pbh_payload(&self) -> Option<&PbhPayload>;
    fn conditional_options(&self) -> Option<&ConditionalOptions>;
}

#[derive(Debug, Clone)]
pub struct WorldChainPooledTransaction {
    pub inner: EthPooledTransaction,
    pub pbh_payload: Option<PbhPayload>,
    pub conditional_options: Option<ConditionalOptions>,
}

impl EthPoolTransaction for WorldChainPooledTransaction {
    fn take_blob(&mut self) -> EthBlobTransactionSidecar {
        self.inner.take_blob()
    }

    fn blob_count(&self) -> usize {
        self.inner.blob_count()
    }

    fn try_into_pooled_eip4844(
        self,
        sidecar: Arc<BlobTransactionSidecar>,
    ) -> Option<RecoveredTx<Self::Pooled>> {
        self.inner.try_into_pooled_eip4844(sidecar)
    }

    fn try_from_eip4844(
        tx: RecoveredTx<Self::Consensus>,
        sidecar: BlobTransactionSidecar,
    ) -> Option<Self> {
        let (tx, signer) = tx.to_components();
        let pooled = PooledTransactionsElement::try_from_blob_transaction(tx, sidecar)
            .ok()
            .map(|tx| tx.with_signer(signer))
            .map(|tx| EthPooledTransaction::from_pooled(tx));

        pooled.map(|inner| Self {
            inner,
            pbh_payload: None,
            conditional_options: None,
        })
    }

    fn validate_blob(
        &self,
        sidecar: &BlobTransactionSidecar,
        settings: &KzgSettings,
    ) -> Result<(), BlobTransactionValidationError> {
        match &self.inner.transaction().transaction {
            RethTransaction::Eip4844(tx) => tx.validate_blob(sidecar, settings),
            _ => Err(BlobTransactionValidationError::NotBlobTransaction(
                self.inner.tx_type(),
            )),
        }
    }

    fn authorization_count(&self) -> usize {
        match &self.inner.transaction().transaction {
            RethTransaction::Eip7702(tx) => tx.authorization_list.len(),
            _ => 0,
        }
    }
}

impl WorldChainPoolTransaction for WorldChainPooledTransaction {
    fn pbh_payload(&self) -> Option<&PbhPayload> {
        self.pbh_payload.as_ref()
    }
    fn conditional_options(&self) -> Option<&ConditionalOptions> {
        self.conditional_options.as_ref()
    }
}

impl From<EthPooledTransaction> for WorldChainPooledTransaction {
    fn from(tx: EthPooledTransaction) -> Self {
        Self {
            inner: tx,
            pbh_payload: None,
            conditional_options: None,
        }
    }
}

/// Conversion from the network transaction type to the pool transaction type.
impl From<PooledTransactionsElementEcRecovered> for WorldChainPooledTransaction {
    fn from(tx: PooledTransactionsElementEcRecovered) -> Self {
        let inner = EthPooledTransaction::from(tx);
        Self {
            inner,
            pbh_payload: None,
            conditional_options: None,
        }
    }
}

impl TryFrom<RecoveredTx> for WorldChainPooledTransaction {
    type Error = TryFromRecoveredTransactionError;

    fn try_from(tx: RecoveredTx) -> Result<Self, Self::Error> {
        let inner = EthPooledTransaction::try_from(tx)?;
        Ok(Self {
            inner,
            pbh_payload: None,
            conditional_options: None,
        })
    }
}

impl Into<RecoveredTx> for WorldChainPooledTransaction {
    fn into(self) -> RecoveredTx {
        self.inner.into()
    }
}

impl PoolTransaction for WorldChainPooledTransaction {
    type TryFromConsensusError = TryFromRecoveredTransactionError;

    type Consensus = TransactionSigned;

    type Pooled = PooledTransactionsElement;

    fn clone_into_consensus(&self) -> RecoveredTx<Self::Consensus> {
        self.inner.transaction().clone()
    }

    fn try_consensus_into_pooled(
        tx: RecoveredTx<Self::Consensus>,
    ) -> Result<RecoveredTx<Self::Pooled>, Self::TryFromConsensusError> {
        let (tx, signer) = tx.to_components();
        let pooled = tx
            .try_into_pooled()
            .map_err(|_| TryFromRecoveredTransactionError::BlobSidecarMissing)?;
        Ok(RecoveredTx::from_signed_transaction(pooled, signer))
    }

    /// Returns hash of the transaction.
    fn hash(&self) -> &TxHash {
        let transaction = self.inner.transaction();
        &transaction.tx_hash()
    }

    /// Returns the Sender of the transaction.
    fn sender(&self) -> Address {
        self.inner.transaction().signer()
    }

    /// Returns a reference to the Sender of the transaction.
    fn sender_ref(&self) -> &Address {
        self.inner.transaction().signer_ref()
    }

    /// Returns the nonce for this transaction.
    fn nonce(&self) -> u64 {
        self.inner.transaction().nonce()
    }

    /// Returns the cost that this transaction is allowed to consume:
    ///
    /// For EIP-1559 transactions: `max_fee_per_gas * gas_limit + tx_value`.
    /// For legacy transactions: `gas_price * gas_limit + tx_value`.
    /// For EIP-4844 blob transactions: `max_fee_per_gas * gas_limit + tx_value +
    /// max_blob_fee_per_gas * blob_gas_used`.
    fn cost(&self) -> &U256 {
        &self.inner.cost()
    }

    /// Amount of gas that should be used in executing this transaction. This is paid up-front.
    fn gas_limit(&self) -> u64 {
        self.inner.transaction().gas_limit()
    }

    /// Returns the EIP-1559 Max base fee the caller is willing to pay.
    ///
    /// For legacy transactions this is `gas_price`.
    ///
    /// This is also commonly referred to as the "Gas Fee Cap" (`GasFeeCap`).
    fn max_fee_per_gas(&self) -> u128 {
        self.inner.transaction().transaction.max_fee_per_gas()
    }

    fn access_list(&self) -> Option<&AccessList> {
        self.inner.transaction().access_list()
    }

    /// Returns the EIP-1559 Priority fee the caller is paying to the block author.
    ///
    /// This will return `None` for non-EIP1559 transactions
    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        self.inner
            .transaction()
            .transaction
            .max_priority_fee_per_gas()
    }

    fn max_fee_per_blob_gas(&self) -> Option<u128> {
        self.inner.transaction().max_fee_per_blob_gas()
    }

    /// Returns the effective tip for this transaction.
    ///
    /// For EIP-1559 transactions: `min(max_fee_per_gas - base_fee, max_priority_fee_per_gas)`.
    /// For legacy transactions: `gas_price - base_fee`.
    fn effective_tip_per_gas(&self, base_fee: u64) -> Option<u128> {
        self.inner.transaction().effective_tip_per_gas(base_fee)
    }

    /// Returns the max priority fee per gas if the transaction is an EIP-1559 transaction, and
    /// otherwise returns the gas price.
    fn priority_fee_or_price(&self) -> u128 {
        self.inner.transaction().priority_fee_or_price()
    }

    /// Returns the transaction's [`TxKind`], which is the address of the recipient or
    /// [`TxKind::Create`] if the transaction is a contract creation.
    fn kind(&self) -> TxKind {
        self.inner.transaction().kind()
    }

    /// Returns true if the transaction is a contract creation.
    fn is_create(&self) -> bool {
        self.inner.transaction().is_create()
    }

    fn input(&self) -> &[u8] {
        self.inner.transaction().input()
    }

    /// Returns a measurement of the heap usage of this type and all its internals.
    fn size(&self) -> usize {
        self.inner.transaction().transaction.input().len()
    }

    /// Returns the transaction type
    fn tx_type(&self) -> u8 {
        self.inner.transaction().tx_type().into()
    }

    /// Returns the length of the rlp encoded object
    fn encoded_length(&self) -> usize {
        self.inner.encoded_length()
    }

    /// Returns `chain_id`
    fn chain_id(&self) -> Option<u64> {
        self.inner.transaction().chain_id()
    }
}
