use alloy_consensus::BlobTransactionValidationError;
use reth_primitives::transaction::TryFromRecoveredTransactionError;
use reth_primitives::{TransactionSignedEcRecovered, TxKind, U256};
use reth_rpc_types::BlobTransactionSidecar;
use reth_transaction_pool::{
    EthBlobTransactionSidecar, EthPoolTransaction, EthPooledTransaction, PoolTransaction,
};
use revm_primitives::KzgSettings;

use crate::pbh::semaphore::SemaphoreProof;
use crate::primitives::WorldChainPooledTransactionsElementEcRecovered;

pub trait WorldChainPoolTransaction {
    fn semaphore_proof(&self) -> Option<&SemaphoreProof>;

    /// Extracts the blob sidecar from the transaction.
    fn take_blob(&mut self) -> EthBlobTransactionSidecar;

    /// Returns the number of blobs this transaction has.
    fn blob_count(&self) -> usize;

    /// Validates the blob sidecar of the transaction with the given settings.
    fn validate_blob(
        &self,
        blob: &BlobTransactionSidecar,
        settings: &KzgSettings,
    ) -> Result<(), BlobTransactionValidationError>;

    /// Returns the number of authorizations this transaction has.
    fn authorization_count(&self) -> usize;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldChainPooledTransaction {
    pub inner: EthPooledTransaction,
    pub semaphore_proof: Option<SemaphoreProof>,
}

impl WorldChainPoolTransaction for WorldChainPooledTransaction {
    fn semaphore_proof(&self) -> Option<&SemaphoreProof> {
        self.semaphore_proof.as_ref()
    }

    fn take_blob(&mut self) -> EthBlobTransactionSidecar {
        todo!()
    }

    fn blob_count(&self) -> usize {
        todo!()
    }

    fn validate_blob(
        &self,
        blob: &BlobTransactionSidecar,
        settings: &KzgSettings,
    ) -> Result<(), BlobTransactionValidationError> {
        todo!()
    }

    fn authorization_count(&self) -> usize {
        todo!()
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
            semaphore_proof: None,
        })
    }
}

impl From<WorldChainPooledTransactionsElementEcRecovered> for WorldChainPooledTransaction {
    fn from(tx: WorldChainPooledTransactionsElementEcRecovered) -> Self {
        Self {
            inner: EthPooledTransaction::from_pooled(tx.inner),
            semaphore_proof: tx.semaphore_proof,
        }
    }
}

impl PoolTransaction for WorldChainPooledTransaction {
    type TryFromConsensusError = <EthPooledTransaction as PoolTransaction>::TryFromConsensusError;

    type Consensus = TransactionSignedEcRecovered;

    type Pooled = WorldChainPooledTransactionsElementEcRecovered;

    fn try_from_consensus(tx: Self::Consensus) -> Result<Self, Self::TryFromConsensusError> {
        EthPooledTransaction::try_from_consensus(tx).map(|inner| Self {
            inner,
            semaphore_proof: None,
        })
    }

    fn into_consensus(self) -> Self::Consensus {
        self.inner.into_consensus()
    }

    fn from_pooled(pooled: Self::Pooled) -> Self {
        Self::from(pooled)
    }

    fn hash(&self) -> &reth_primitives::TxHash {
        self.inner.hash()
    }

    fn sender(&self) -> reth_primitives::Address {
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

    fn access_list(&self) -> Option<&reth_primitives::AccessList> {
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

// impl EthPoolTransaction for WorldChainPooledTransaction {
//     fn take_blob(&mut self) -> reth_transaction_pool::EthBlobTransactionSidecar {
//         self.inner.take_blob()
//     }
//
//     fn blob_count(&self) -> usize {
//         self.inner.blob_count()
//     }
//
//     fn validate_blob(
//         &self,
//         blob: &reth_primitives::BlobTransactionSidecar,
//         settings: &reth_primitives::kzg::KzgSettings,
//     ) -> Result<(), reth_primitives::BlobTransactionValidationError> {
//         self.inner.validate_blob(blob, settings)
//     }
//
//     fn authorization_count(&self) -> usize {
//         self.inner.authorization_count()
//     }
// }
