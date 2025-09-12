use std::collections::HashSet;

use alloy_primitives::{Address, TxHash};
use reth_payload_util::PayloadTransactions;
use reth_transaction_pool::PoolTransaction;

/// A wrapper around [`PayloadTransactions`] that drains previously observed transactions
/// before yielding new transactions.
pub struct BestPayloadTxns<I>
where
    I: PayloadTransactions,
{
    /// The inner payload transactions iterator
    inner: I,

    /// Transactions to be skipped that were previously observed.
    prev: HashSet<TxHash>,

    /// Any transactions that were newly observed.
    observed: Vec<TxHash>,
}

pub struct BestPayloadTxnsGuard<'a, I>
where
    I: PayloadTransactions,
{
    inner: &'a mut BestPayloadTxns<I>,
}

impl<I> BestPayloadTxns<I>
where
    I: PayloadTransactions,
{
    pub fn new(inner: I) -> Self {
        Self {
            inner,
            prev: HashSet::new(),
            observed: Vec::new(),
        }
    }

    pub fn with_prev(mut self, prev: Vec<TxHash>) -> Self {
        self.prev.extend(prev);
        self
    }

    pub fn take_observed(self) -> (impl Iterator<Item = TxHash>, impl Iterator<Item = TxHash>) {
        (self.prev.into_iter(), self.observed.into_iter())
    }

    pub fn guard(&mut self) -> BestPayloadTxnsGuard<'_, I> {
        BestPayloadTxnsGuard { inner: self }
    }
}

impl<'a, I> PayloadTransactions for BestPayloadTxnsGuard<'a, I>
where
    I: PayloadTransactions<Transaction: PoolTransaction + Clone>,
{
    type Transaction = I::Transaction;

    fn next(&mut self, ctx: ()) -> Option<Self::Transaction> {
        while let Some(n) = self.inner.inner.next(ctx) {
            // If the transaction is not in the previous set, we can yield it.
            if !self.inner.prev.contains(n.hash()) {
                self.inner.observed.push(*n.hash());
                return Some(n);
            }
        }

        None
    }

    fn mark_invalid(&mut self, sender: Address, nonce: u64) {
        self.inner.inner.mark_invalid(sender, nonce);
    }
}
