//! Bounded in-memory cache of per-block pre-image witnesses.

use std::collections::BTreeMap;

use parking_lot::Mutex;

use crate::types::{BlockWitness, RangeWitness};

/// Default number of block witnesses to retain in memory.
pub const DEFAULT_WITNESS_CAP: usize = 1024;

/// A bounded circular ring buffer of [`BlockWitness`]es keyed by block number.
#[derive(Debug)]
pub struct WitnessCache {
    inner: Mutex<Inner>,
    /// Ring-buffer depth: the maximum number of block witnesses retained before the lowest are
    /// evicted. Set at runtime from `--witness.depth`.
    depth: usize,
}

#[derive(Debug, Default)]
struct Inner(BTreeMap<u64, BlockWitness>);

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

    /// Creates an empty cache with a runtime ring-buffer `depth` (the maximum number of block
    /// witnesses retained).
    ///
    /// A `depth` of zero is clamped to one, since a cache that retains nothing is never useful.
    #[must_use]
    pub fn with_depth(depth: usize) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            depth: depth.max(1),
        }
    }

    /// Inserts (or replaces) the witness for its block number, evicting the lowest block numbers
    /// if the cache is over its [`depth`](Self::with_depth).
    pub fn insert(&self, witness: BlockWitness) {
        let mut inner = self.inner.lock();
        inner.0.insert(witness.block_number, witness);
        while inner.0.len() > self.depth {
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
        let cache = WitnessCache::with_depth(3);
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
    fn zero_depth_is_clamped() {
        // Zero depth is clamped to retain at least one block.
        let cache = WitnessCache::with_depth(0);
        cache.insert(witness(7));
        cache.insert(witness(8));
        assert_eq!(cache.bounds(), Some((8, 8)));
    }

    #[test]
    fn reorg_overwrites_in_place() {
        let cache = WitnessCache::with_depth(8);
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
        let cache = WitnessCache::with_depth(16);
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
