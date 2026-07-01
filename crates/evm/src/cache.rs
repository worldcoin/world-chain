//! Bounded in-memory cache of per-block execution witnesses.

use std::{collections::BTreeMap, sync::Arc};

use alloy_rpc_types_debug::ExecutionWitness;
use parking_lot::Mutex;

/// Default number of execution witnesses to retain in memory.
pub const DEFAULT_WITNESS_CAP: usize = 1024;

/// A bounded, reorg-safe in-memory ring buffer of [`ExecutionWitness`]es keyed by block number.
#[derive(Debug)]
pub struct WitnessCache {
    inner: Mutex<BTreeMap<u64, Arc<ExecutionWitness>>>,
    depth: usize,
}

impl Default for WitnessCache {
    fn default() -> Self {
        Self::new()
    }
}

impl WitnessCache {
    /// Creates an empty cache with the default ring-buffer depth ([`DEFAULT_WITNESS_CAP`]).
    #[must_use]
    pub fn new() -> Self {
        Self::with_depth(DEFAULT_WITNESS_CAP)
    }

    /// Creates an empty cache with a runtime ring-buffer `depth` (the maximum number of witnesses
    /// retained).
    ///
    /// A `depth` of zero is clamped to one, since a cache that retains nothing is never useful.
    #[must_use]
    pub fn with_depth(depth: usize) -> Self {
        Self {
            inner: Mutex::new(BTreeMap::new()),
            depth: depth.max(1),
        }
    }

    /// Inserts (or replaces) the execution witness for `block_number`, evicting the lowest block(s)
    /// once `depth` is exceeded.
    pub fn insert(&self, block_number: u64, witness: ExecutionWitness) {
        let mut inner = self.inner.lock();
        inner.insert(block_number, Arc::new(witness));
        while inner.len() > self.depth {
            inner.pop_first();
        }
    }

    /// Returns the execution witness for `block_number`, if cached. Zero-copy: clones only the
    /// [`Arc`].
    #[must_use]
    pub fn get(&self, block_number: u64) -> Option<Arc<ExecutionWitness>> {
        self.inner.lock().get(&block_number).cloned()
    }

    /// Returns the lowest and highest cached block numbers, if any.
    #[must_use]
    pub fn bounds(&self) -> Option<(u64, u64)> {
        let inner = self.inner.lock();
        Some((*inner.first_key_value()?.0, *inner.last_key_value()?.0))
    }

    /// Collects the execution witnesses for the contiguous, inclusive L2 range
    /// `[start_block, end_block]` (`start_block == end_block` yields a single block).
    ///
    /// Returns `None` if the range is inverted (`end_block < start_block`) or if any block in it is
    /// missing; the range proof requires every block, so a partial range is never served. Zero-copy:
    /// only the per-block [`Arc`]s are cloned, never the witness data.
    #[must_use]
    pub fn range(&self, start_block: u64, end_block: u64) -> Option<Vec<Arc<ExecutionWitness>>> {
        if end_block < start_block {
            return None;
        }
        let inner = self.inner.lock();
        let witnesses: Vec<_> = inner
            .range(start_block..=end_block)
            .map(|(_, witness)| Arc::clone(witness))
            .collect();
        // Every block in `[start_block, end_block]` must be present.
        (witnesses.len() as u64 == end_block - start_block + 1).then_some(witnesses)
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Bytes;

    use super::*;

    /// Tags a witness with a distinguishable `keys` entry so identity/order can be asserted.
    fn witness(tag: u8) -> ExecutionWitness {
        ExecutionWitness {
            keys: vec![Bytes::from(vec![tag])],
            ..Default::default()
        }
    }

    #[test]
    fn evicts_lowest_over_depth() {
        let cache = WitnessCache::with_depth(3);
        for n in 1..=5 {
            cache.insert(n, witness(n as u8));
        }
        assert!(cache.get(1).is_none());
        assert!(cache.get(2).is_none());
        assert!(cache.get(3).is_some());
        assert_eq!(cache.bounds(), Some((3, 5)));
    }

    #[test]
    fn zero_depth_is_clamped() {
        let cache = WitnessCache::with_depth(0);
        cache.insert(7, witness(7));
        cache.insert(8, witness(8));
        assert_eq!(cache.bounds(), Some((8, 8)));
    }

    #[test]
    fn reorg_overwrites_in_place() {
        let cache = WitnessCache::with_depth(8);
        cache.insert(10, witness(1));
        cache.insert(10, witness(2));
        assert_eq!(cache.get(10).unwrap().keys, vec![Bytes::from(vec![2])]);
        assert_eq!(cache.bounds(), Some((10, 10)));
    }

    #[test]
    fn range_requires_every_block() {
        let cache = WitnessCache::with_depth(16);
        for n in [11u64, 12, 14] {
            cache.insert(n, witness(n as u8));
        }

        // [11, 12] inclusive = {11, 12}, in ascending order.
        let range = cache.range(11, 12).expect("contiguous range present");
        assert_eq!(
            range.iter().map(|w| w.keys.clone()).collect::<Vec<_>>(),
            vec![vec![Bytes::from(vec![11])], vec![Bytes::from(vec![12])]],
        );

        // `start == end` yields a single block.
        assert_eq!(cache.range(12, 12).unwrap().len(), 1);
        // [12, 14] is missing 13.
        assert!(cache.range(12, 14).is_none());
        // Inverted ranges are rejected.
        assert!(cache.range(14, 12).is_none());
    }
}
