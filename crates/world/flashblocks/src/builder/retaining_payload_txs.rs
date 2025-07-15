use std::collections::HashSet;

use alloy_primitives::TxHash;
use reth_payload_util::PayloadTransactions;
use reth_transaction_pool::PoolTransaction;

/// This type exists to yield best transactions from the tx pool
/// while doing bookkeeping so we can deterministically replay them on the following
/// flashblock
pub struct RetainingBestTxs<I>
where
    I: PayloadTransactions,
{
    /// The inner payload transactions iterator
    inner: I,

    /// The observed tx hashes. It's a separate container because
    /// the order of transactions in `observed` must remain consistent
    observed_hashes: HashSet<TxHash>,
}

pub struct RetainingBestTxsGuard<'a, I>
where
    I: PayloadTransactions,
{
    inner: &'a mut RetainingBestTxs<I>,
}

impl<I> RetainingBestTxs<I>
where
    I: PayloadTransactions,
{
    pub fn new(inner: I) -> Self {
        Self {
            inner,
            observed_hashes: HashSet::new(),
        }
    }

    pub fn with_observed(mut self, observed: Vec<TxHash>) -> Self {
        self.observed_hashes = observed.into_iter().collect();
        self
    }

    pub fn take_observed(self) -> Vec<TxHash> {
        self.observed_hashes.into_iter().collect()
    }

    pub fn guard(&mut self) -> RetainingBestTxsGuard<'_, I> {
        RetainingBestTxsGuard { inner: self }
    }
}

impl<'a, I> PayloadTransactions for RetainingBestTxsGuard<'a, I>
where
    I: PayloadTransactions<Transaction: PoolTransaction + Clone>,
{
    type Transaction = I::Transaction;

    fn next(&mut self, ctx: ()) -> Option<Self::Transaction> {
        loop {
            let Some(n) = self.inner.inner.next(ctx) else {
                break;
            };

            if !self.inner.observed_hashes.contains(n.hash()) {
                self.inner.observed_hashes.insert(*n.hash());

                return Some(n);
            }
        }

        None
    }

    fn mark_invalid(&mut self, sender: alloy_primitives::Address, nonce: u64) {
        self.inner.inner.mark_invalid(sender, nonce);
    }
}
