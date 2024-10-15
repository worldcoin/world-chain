use alloy_primitives::TxHash;
use reth::transaction_pool::{EthPoolTransaction, EthPooledTransaction, PoolTransaction};
use reth_primitives::transaction::TryFromRecoveredTransactionError;
use reth_primitives::{PooledTransactionsElementEcRecovered, TransactionSignedEcRecovered};
use revm_primitives::{AccessList, Address, TxKind, U256};

use crate::pbh::payload::PbhPayload;
use crate::primitives::WorldChainPooledTransactionsElementEcRecovered;

pub trait WorldChainPoolTransaction: EthPoolTransaction {
    fn pbh_payload(&self) -> Option<&PbhPayload>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldChainPooledTransaction {
    pub inner: EthPooledTransaction,
    pub pbh_payload: Option<PbhPayload>,
}

impl EthPoolTransaction for WorldChainPooledTransaction {
    fn take_blob(&mut self) -> reth::transaction_pool::EthBlobTransactionSidecar {
        self.inner.take_blob()
    }

    fn blob_count(&self) -> usize {
        self.inner.blob_count()
    }

    fn validate_blob(
        &self,
        blob: &reth_primitives::BlobTransactionSidecar,
        settings: &reth_primitives::kzg::KzgSettings,
    ) -> Result<(), reth_primitives::BlobTransactionValidationError> {
        self.inner.validate_blob(blob, settings)
    }

    fn authorization_count(&self) -> usize {
        self.inner.authorization_count()
    }
}

impl WorldChainPoolTransaction for WorldChainPooledTransaction {
    fn pbh_payload(&self) -> Option<&PbhPayload> {
        self.pbh_payload.as_ref()
    }
}

impl From<WorldChainPooledTransaction> for TransactionSignedEcRecovered {
    fn from(tx: WorldChainPooledTransaction) -> Self {
        tx.inner.into_consensus()
    }
}

impl TryFrom<TransactionSignedEcRecovered> for WorldChainPooledTransaction {
    type Error = TryFromRecoveredTransactionError;

    fn try_from(tx: TransactionSignedEcRecovered) -> Result<Self, Self::Error> {
        Ok(Self {
            inner: EthPooledTransaction::try_from(tx)?,
            pbh_payload: None,
        })
    }
}

impl From<WorldChainPooledTransactionsElementEcRecovered> for WorldChainPooledTransaction {
    fn from(tx: WorldChainPooledTransactionsElementEcRecovered) -> Self {
        Self {
            inner: EthPooledTransaction::from_pooled(tx.inner),
            pbh_payload: tx.pbh_payload,
        }
    }
}

impl From<PooledTransactionsElementEcRecovered> for WorldChainPooledTransactionsElementEcRecovered {
    fn from(value: PooledTransactionsElementEcRecovered) -> Self {
        Self {
            inner: value,
            // Incoming consensus transactions do not have a semaphore proof
            // Is this problematic?
            pbh_payload: None,
        }
    }
}

impl From<WorldChainPooledTransactionsElementEcRecovered> for PooledTransactionsElementEcRecovered {
    fn from(value: WorldChainPooledTransactionsElementEcRecovered) -> Self {
        value.inner
    }
}

impl PoolTransaction for WorldChainPooledTransaction {
    type TryFromConsensusError = <EthPooledTransaction as PoolTransaction>::TryFromConsensusError;

    type Consensus = TransactionSignedEcRecovered;

    type Pooled = WorldChainPooledTransactionsElementEcRecovered;

    fn try_from_consensus(tx: Self::Consensus) -> Result<Self, Self::TryFromConsensusError> {
        EthPooledTransaction::try_from_consensus(tx).map(|inner| Self {
            inner,
            pbh_payload: None,
        })
    }

    fn into_consensus(self) -> Self::Consensus {
        self.inner.into_consensus()
    }

    fn from_pooled(pooled: Self::Pooled) -> Self {
        Self::from(pooled)
    }

    fn hash(&self) -> &TxHash {
        self.inner.hash()
    }

    fn sender(&self) -> Address {
        self.inner.sender()
    }

    fn nonce(&self) -> u64 {
        self.inner.nonce()
    }

    fn cost(&self) -> U256 {
        self.inner.cost()
    }

    fn gas_limit(&self) -> u64 {
        self.inner.gas_limit()
    }

    fn max_fee_per_gas(&self) -> u128 {
        self.inner.max_fee_per_gas()
    }

    fn access_list(&self) -> Option<&AccessList> {
        self.inner.access_list()
    }

    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        self.inner.max_priority_fee_per_gas()
    }

    fn max_fee_per_blob_gas(&self) -> Option<u128> {
        self.inner.max_fee_per_blob_gas()
    }

    fn effective_tip_per_gas(&self, base_fee: u64) -> Option<u128> {
        self.inner.effective_tip_per_gas(base_fee)
    }

    fn priority_fee_or_price(&self) -> u128 {
        self.inner.priority_fee_or_price()
    }

    fn kind(&self) -> TxKind {
        self.inner.kind()
    }

    fn input(&self) -> &[u8] {
        self.inner.input()
    }

    fn size(&self) -> usize {
        self.inner.size()
    }

    fn tx_type(&self) -> u8 {
        self.inner.tx_type()
    }

    fn encoded_length(&self) -> usize {
        self.inner.encoded_length()
    }

    fn chain_id(&self) -> Option<u64> {
        self.inner.chain_id()
    }
}
