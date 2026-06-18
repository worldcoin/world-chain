//! Bounded in-memory cache of per-block pre-image witnesses.

use std::collections::BTreeMap;

use parking_lot::Mutex;

use crate::types::{BlockWitness, RangeWitness};

/// Default number of block witnesses to retain in memory.
pub const DEFAULT_WITNESS_CAP: usize = 1024;

/// A bounded, reorg-safe in-memory cache of [`BlockWitness`]es keyed by block number.
///
/// Behaves as a circular ring buffer over block numbers: when the number of cached blocks exceeds
/// `CAPACITY`, the lowest block numbers are evicted. Inserting a block number that already exists
/// overwrites the entry, so a reorg that re-imports a height simply replaces the stale witness.
#[derive(Debug)]
pub struct WitnessCache<const CAPACITY: usize = DEFAULT_WITNESS_CAP> {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner(BTreeMap<u64, BlockWitness>);

impl<const CAPACITY: usize> Default for WitnessCache<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CAPACITY: usize> WitnessCache<CAPACITY> {
    /// Creates an empty cache holding at most `CAPACITY` block witnesses.
    ///
    /// `CAPACITY` must be non-zero; a cache that retains nothing is never useful. Instantiating
    /// the cache with a zero `CAPACITY` is a compile-time error.
    #[must_use]
    pub fn new() -> Self {
        const { assert!(CAPACITY > 0, "WitnessCache: CAPACITY must be non-zero") };
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Inserts (or replaces) the witness for its block number, evicting the lowest block numbers
    /// if the cache is over capacity.
    pub fn insert(&self, witness: BlockWitness) {
        let mut inner = self.inner.lock();
        inner.0.insert(witness.block_number, witness);
        while inner.0.len() > CAPACITY {
            inner.0.pop_first();
        }
    }

    /// Returns a clone of the witness for `block_number`, if cached.
    #[must_use]
    pub fn get(&self, block_number: u64) -> Option<BlockWitness> {
        self.inner.lock().0.get(&block_number).cloned()
    }

    /// Returns the lowest and highest cached block numbers, if any.
    #[must_use]
    pub fn bounds(&self) -> Option<(u64, u64)> {
        let inner = self.inner.lock();
        let (&lowest, _) = inner.0.first_key_value()?;
        let (&highest, _) = inner.0.last_key_value()?;
        Some((lowest, highest))
    }

    /// Collects a contiguous [`RangeWitness`] for the L2 range `(start_block, end_block]`.
    ///
    /// Returns `None` if `end_block <= start_block` or if any block in the range is missing from
    /// the cache; the range proof requires every block, so a partial range is not served.
    #[must_use]
    pub fn range(&self, start_block: u64, end_block: u64) -> Option<RangeWitness> {
        if end_block <= start_block {
            return None;
        }
        let inner = self.inner.lock();
        let mut blocks = Vec::with_capacity((end_block - start_block) as usize);
        for number in (start_block + 1)..=end_block {
            blocks.push(inner.0.get(&number)?.clone());
        }
        Some(RangeWitness {
            start_block,
            end_block,
            blocks,
        })
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::B256;
    use alloy_rpc_types_debug::ExecutionWitness;

    use super::*;

    fn witness(number: u64) -> BlockWitness {
        BlockWitness {
            block_number: number,
            block_hash: B256::with_last_byte(number as u8),
            execution_witness: ExecutionWitness::default(),
            header_rlp: Default::default(),
            transactions: Vec::new(),
        }
    }

    #[test]
    fn evicts_lowest_over_capacity() {
        let cache = WitnessCache::<3>::new();
        for n in 1..=5 {
            cache.insert(witness(n));
        }
        // 1 and 2 evicted; 3,4,5 retained.
        assert!(cache.get(1).is_none());
        assert!(cache.get(2).is_none());
        assert!(cache.get(3).is_some());
        assert_eq!(cache.bounds(), Some((3, 5)));
    }

    #[test]
    fn reorg_overwrites_in_place() {
        let cache = WitnessCache::<8>::new();
        cache.insert(witness(10));
        let original = cache.get(10).unwrap().block_hash;

        let mut replacement = witness(10);
        replacement.block_hash = B256::repeat_byte(0xab);
        cache.insert(replacement);

        let stored = cache.get(10).unwrap().block_hash;
        assert_ne!(stored, original);
        assert_eq!(stored, B256::repeat_byte(0xab));
        assert_eq!(cache.bounds(), Some((10, 10)));
    }

    #[test]
    fn range_requires_every_block() {
        let cache = WitnessCache::<16>::new();
        for n in [11, 12, 14] {
            cache.insert(witness(n));
        }
        // (10, 12] = {11, 12} present.
        let range = cache.range(10, 12).expect("contiguous range present");
        assert_eq!(range.start_block, 10);
        assert_eq!(range.end_block, 12);
        assert_eq!(
            range
                .blocks
                .iter()
                .map(|b| b.block_number)
                .collect::<Vec<_>>(),
            vec![11, 12]
        );
        // (12, 14] is missing 13.
        assert!(cache.range(12, 14).is_none());
        // empty / inverted ranges are rejected.
        assert!(cache.range(12, 12).is_none());
        assert!(cache.range(14, 12).is_none());
    }
}
