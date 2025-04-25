use std::collections::{HashSet, VecDeque};

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

    /// Transactions that were previously observed, they have to be yielded prior to other pool in
    /// the exact same order. Furthermore we should discard duplicate txs
    prev: VecDeque<I::Transaction>,

    /// Transactions observed during the lifetime of this struct
    /// They should be fed back during the construction of the next flashblock
    observed: Vec<I::Transaction>,

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
            prev: VecDeque::new(),
            observed: Vec::new(),
            observed_hashes: HashSet::new(),
        }
    }

    pub fn with_prev(mut self, prev: Vec<I::Transaction>) -> Self {
        self.prev.extend(prev);
        self
    }

    pub fn take_observed(self) -> Vec<I::Transaction> {
        self.observed
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
        if let Some(n) = self.inner.prev.pop_front() {
            self.inner.observed_hashes.insert(*n.hash());
            self.inner.observed.push(n.clone());

            return Some(n);
        }

        loop {
            let Some(n) = self.inner.inner.next(ctx) else {
                break;
            };

            if !self.inner.observed_hashes.contains(n.hash()) {
                self.inner.observed_hashes.insert(*n.hash());
                self.inner.observed.push(n.clone());

                return Some(n);
            }
        }

        None
    }

    fn mark_invalid(&mut self, sender: alloy_primitives::Address, nonce: u64) {
        self.inner.inner.mark_invalid(sender, nonce);
    }
}
