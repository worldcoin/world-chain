use std::collections::VecDeque;

use reth_payload_util::PayloadTransactions;

/// This type exists to yield bes transactions from the tx pool
/// while doing bookkeping so we can deterministically replay them on the following
/// flashblock
pub struct RetainingBestTxs<I>
where
    I: PayloadTransactions,
{
    inner: I,
    /// Transactions that were previously observed, they have to be yielded prior to other pool in
    /// the exact same order. Furthermore we should discard duplicate txs
    prev: VecDeque<I::Transaction>,

    /// Transactions observed during the lifetime of this struct
    /// They should be fed back during the construction of the next flashblock
    observed: Vec<I::Transaction>,
}

pub struct RetainingBestTxsGuard<'a, I>
where
    I: PayloadTransactions,
{
    retaining: &'a mut RetainingBestTxs<I>,
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
        RetainingBestTxsGuard { retaining: self }
    }
}

impl<'a, I> PayloadTransactions for RetainingBestTxsGuard<'a, I>
where
    I: PayloadTransactions,
{
    type Transaction = I::Transaction;

    fn next(&mut self, ctx: ()) -> Option<Self::Transaction> {
        // TODO: Implement
        self.retaining.inner.next(ctx)
    }

    fn mark_invalid(&mut self, sender: alloy_primitives::Address, nonce: u64) {
        self.retaining.inner.mark_invalid(sender, nonce);
    }
}
