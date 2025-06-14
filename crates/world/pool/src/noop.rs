use std::{collections::HashSet, sync::Arc};

use super::tx::WorldChainPooledTransaction;
use alloy_eips::eip4844::BlobAndProofV1;
use alloy_primitives::{Address, TxHash, B256};
use reth::transaction_pool::{
    error::PoolError, noop::NoopTransactionPool, AllPoolTransactions, AllTransactionsEvents,
    BestTransactions, BestTransactionsAttributes, BlobStoreError, BlockInfo,
    GetPooledTransactionLimit, NewBlobSidecar, NewTransactionEvent, PoolResult, PoolSize,
    PoolTransaction, PropagatedTransactions, TransactionEvents, TransactionListenerKind,
    TransactionOrigin, TransactionPool, ValidPoolTransaction,
};
use reth_eth_wire_types::HandleMempoolData;
use reth_primitives::Recovered;
use tokio::sync::mpsc::{self, Receiver};

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct NoopWorldChainTransactionPool {
    inner: NoopTransactionPool,
}

impl TransactionPool for NoopWorldChainTransactionPool {
    type Transaction = WorldChainPooledTransaction;

    fn get_blob(
        &self,
        _tx_hash: TxHash,
    ) -> Result<Option<Arc<alloy_eips::eip7594::BlobTransactionSidecarVariant>>, BlobStoreError>
    {
        Ok(None)
    }

    fn get_all_blobs(
        &self,
        _tx_hashes: Vec<TxHash>,
    ) -> Result<
        Vec<(
            TxHash,
            Arc<alloy_eips::eip7594::BlobTransactionSidecarVariant>,
        )>,
        BlobStoreError,
    > {
        Ok(vec![])
    }

    fn get_all_blobs_exact(
        &self,
        _tx_hashes: Vec<TxHash>,
    ) -> Result<Vec<Arc<alloy_eips::eip7594::BlobTransactionSidecarVariant>>, BlobStoreError> {
        Ok(vec![])
    }
    fn get_blobs_for_versioned_hashes_v1(
        &self,
        _versioned_hashes: &[B256],
    ) -> Result<Vec<Option<BlobAndProofV1>>, BlobStoreError> {
        Ok(vec![])
    }

    fn get_blobs_for_versioned_hashes_v2(
        &self,
        _versioned_hashes: &[B256],
    ) -> Result<Option<Vec<alloy_eips::eip4844::BlobAndProofV2>>, BlobStoreError> {
        Ok(Some(vec![]))
    }

    fn get_pending_transactions_with_predicate(
        &self,
        _predicate: impl FnMut(&ValidPoolTransaction<Self::Transaction>) -> bool,
    ) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
        vec![]
    }
    fn pending_transactions_max(
        &self,
        _max: usize,
    ) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
        vec![]
    }

    fn pool_size(&self) -> PoolSize {
        Default::default()
    }

    fn block_info(&self) -> BlockInfo {
        self.inner.block_info()
    }

    async fn add_transaction_and_subscribe(
        &self,
        _origin: TransactionOrigin,
        transaction: Self::Transaction,
    ) -> PoolResult<TransactionEvents> {
        let hash = *transaction.hash();
        Err(PoolError::other(hash, "noop insertion error"))
    }

    async fn add_transaction(
        &self,
        _origin: TransactionOrigin,
        transaction: Self::Transaction,
    ) -> PoolResult<TxHash> {
        let hash = *transaction.hash();
        Err(PoolError::other(hash, "noop insertion error"))
    }

    async fn add_transactions(
        &self,
        _origin: TransactionOrigin,
        transactions: Vec<Self::Transaction>,
    ) -> Vec<PoolResult<TxHash>> {
        transactions
            .into_iter()
            .map(|transaction| {
                let hash = *transaction.hash();
                Err(PoolError::other(hash, "noop insertion error"))
            })
            .collect()
    }

    fn transaction_event_listener(&self, _tx_hash: TxHash) -> Option<TransactionEvents> {
        None
    }

    fn all_transactions_event_listener(&self) -> AllTransactionsEvents<Self::Transaction> {
        AllTransactionsEvents::new(mpsc::channel(1).1)
    }

    fn pending_transactions_listener_for(
        &self,
        _kind: TransactionListenerKind,
    ) -> Receiver<TxHash> {
        mpsc::channel(1).1
    }

    fn new_transactions_listener(&self) -> Receiver<NewTransactionEvent<Self::Transaction>> {
        mpsc::channel(1).1
    }

    fn blob_transaction_sidecars_listener(&self) -> Receiver<NewBlobSidecar> {
        mpsc::channel(1).1
    }

    fn new_transactions_listener_for(
        &self,
        _kind: TransactionListenerKind,
    ) -> Receiver<NewTransactionEvent<Self::Transaction>> {
        mpsc::channel(1).1
    }

    fn pooled_transaction_hashes(&self) -> Vec<TxHash> {
        vec![]
    }

    fn pooled_transaction_hashes_max(&self, _max: usize) -> Vec<TxHash> {
        vec![]
    }

    fn pooled_transactions(&self) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
        vec![]
    }

    fn pooled_transactions_max(
        &self,
        _max: usize,
    ) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
        vec![]
    }

    fn get_pooled_transaction_elements(
        &self,
        _tx_hashes: Vec<TxHash>,
        _limit: GetPooledTransactionLimit,
    ) -> Vec<<Self::Transaction as PoolTransaction>::Pooled> {
        vec![]
    }

    fn get_pooled_transaction_element(
        &self,
        _tx_hash: TxHash,
    ) -> Option<Recovered<<Self::Transaction as reth::transaction_pool::PoolTransaction>::Pooled>>
    {
        None
    }

    fn best_transactions(
        &self,
    ) -> Box<dyn BestTransactions<Item = Arc<ValidPoolTransaction<Self::Transaction>>>> {
        Box::new(std::iter::empty())
    }

    fn best_transactions_with_attributes(
        &self,
        _: BestTransactionsAttributes,
    ) -> Box<dyn BestTransactions<Item = Arc<ValidPoolTransaction<Self::Transaction>>>> {
        Box::new(std::iter::empty())
    }

    fn pending_transactions(&self) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
        vec![]
    }

    fn queued_transactions(&self) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
        vec![]
    }

    fn all_transactions(&self) -> AllPoolTransactions<Self::Transaction> {
        AllPoolTransactions::default()
    }

    fn remove_transactions(
        &self,
        _hashes: Vec<TxHash>,
    ) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
        vec![]
    }

    fn retain_unknown<A>(&self, _announcement: &mut A)
    where
        A: HandleMempoolData,
    {
    }

    fn get(&self, _tx_hash: &TxHash) -> Option<Arc<ValidPoolTransaction<Self::Transaction>>> {
        None
    }

    fn get_all(&self, _txs: Vec<TxHash>) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
        vec![]
    }

    fn on_propagated(&self, _txs: PropagatedTransactions) {}

    fn get_transactions_by_sender(
        &self,
        _sender: Address,
    ) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
        vec![]
    }

    fn get_transaction_by_sender_and_nonce(
        &self,
        _sender: Address,
        _nonce: u64,
    ) -> Option<Arc<ValidPoolTransaction<Self::Transaction>>> {
        None
    }

    fn get_transactions_by_origin(
        &self,
        _origin: TransactionOrigin,
    ) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
        vec![]
    }

    fn get_pending_transactions_by_origin(
        &self,
        _origin: TransactionOrigin,
    ) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
        vec![]
    }

    fn unique_senders(&self) -> HashSet<Address> {
        Default::default()
    }

    fn get_highest_transaction_by_sender(
        &self,
        _sender: Address,
    ) -> Option<Arc<ValidPoolTransaction<Self::Transaction>>> {
        None
    }

    fn remove_transactions_and_descendants(
        &self,
        _hashes: Vec<TxHash>,
    ) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
        vec![]
    }

    fn remove_transactions_by_sender(
        &self,
        _sender: Address,
    ) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
        vec![]
    }

    fn get_queued_transactions_by_sender(
        &self,
        _sender: Address,
    ) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
        vec![]
    }

    fn get_pending_transactions_by_sender(
        &self,
        _sender: Address,
    ) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
        vec![]
    }

    fn get_highest_consecutive_transaction_by_sender(
        &self,
        _sender: Address,
        _on_chain_nonce: u64,
    ) -> Option<Arc<ValidPoolTransaction<Self::Transaction>>> {
        None
    }
}
