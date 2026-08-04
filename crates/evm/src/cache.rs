//! Bounded in-memory cache of per-block execution witnesses.

use std::{collections::BTreeMap, sync::Arc};

use alloy_rpc_types_debug::ExecutionWitness;
use parking_lot::Mutex;

use crate::metrics::WitnessMetrics;

/// Default number of execution witnesses to retain in memory.
pub const DEFAULT_WITNESS_CAP: usize = 1024;

/// A cached witness alongside the number of bytes it retains.
#[derive(Debug)]
struct CachedWitness {
    witness: Arc<ExecutionWitness>,
    bytes: usize,
}

/// Cache contents guarded by a single lock, with a running total of retained bytes.
#[derive(Debug, Default)]
struct Inner {
    witnesses: BTreeMap<u64, CachedWitness>,
    bytes: usize,
}

/// A bounded, reorg-safe in-memory ring buffer of [`ExecutionWitness`]es keyed by block number.
#[derive(Debug)]
pub struct WitnessCache {
    inner: Mutex<Inner>,
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
            inner: Mutex::new(Inner::default()),
            depth: depth.max(1),
        }
    }

    /// Inserts (or replaces) the execution witness for `block_number`, evicting the lowest block(s)
    /// once `depth` is exceeded.
    pub fn insert(&self, block_number: u64, witness: ExecutionWitness) {
        let metrics = WitnessMetrics::get();
        let bytes = retained_bytes(&witness);
        let mut inner = self.inner.lock();

        if let Some(replaced) = inner.witnesses.insert(
            block_number,
            CachedWitness {
                witness: Arc::new(witness),
                bytes,
            },
        ) {
            inner.bytes -= replaced.bytes;
        }
        inner.bytes += bytes;

        let mut evicted = 0u64;
        while inner.witnesses.len() > self.depth {
            let Some((_, dropped)) = inner.witnesses.pop_first() else {
                break;
            };
            inner.bytes -= dropped.bytes;
            evicted += 1;
        }

        metrics.inserted.increment(1);
        metrics.evicted.increment(evicted);
        metrics.witness_bytes.record(bytes as f64);
        metrics.cache_bytes.set(inner.bytes as f64);
        metrics.cache_len.set(inner.witnesses.len() as f64);
        if let (Some((oldest, _)), Some((newest, _))) = (
            inner.witnesses.first_key_value(),
            inner.witnesses.last_key_value(),
        ) {
            metrics.cache_oldest_block.set(*oldest as f64);
            metrics.cache_newest_block.set(*newest as f64);
        }
    }

    /// Returns the execution witness for `block_number`, if cached. Zero-copy: clones only the
    /// [`Arc`].
    #[must_use]
    pub fn get(&self, block_number: u64) -> Option<Arc<ExecutionWitness>> {
        self.inner
            .lock()
            .witnesses
            .get(&block_number)
            .map(|entry| Arc::clone(&entry.witness))
    }

    /// Returns the lowest and highest cached block numbers, if any.
    #[must_use]
    pub fn bounds(&self) -> Option<(u64, u64)> {
        let inner = self.inner.lock();
        Some((
            *inner.witnesses.first_key_value()?.0,
            *inner.witnesses.last_key_value()?.0,
        ))
    }

    /// Collects the execution witnesses for the contiguous, inclusive L2 range
    /// `[start_block, end_block]` (`start_block == end_block` yields a single block).
    ///
    /// Returns `None` if the range is inverted (`end_block < start_block`) or if any block in it is
    /// missing; the range proof requires every block, so a partial range is never served. Zero-copy:
    /// only the per-block [`Arc`]s are cloned, never the witness data.
    #[must_use]
    pub fn range(&self, start_block: u64, end_block: u64) -> Option<Vec<Arc<ExecutionWitness>>> {
        let metrics = WitnessMetrics::get();
        if end_block < start_block {
            metrics.range_miss.increment(1);
            return None;
        }

        let inner = self.inner.lock();
        let witnesses: Vec<_> = inner
            .witnesses
            .range(start_block..=end_block)
            .map(|(_, entry)| Arc::clone(&entry.witness))
            .collect();
        drop(inner);

        // Every block in `[start_block, end_block]` must be present.
        let requested = end_block - start_block + 1;
        let missing = requested - witnesses.len() as u64;
        if missing > 0 {
            metrics.range_miss.increment(1);
            metrics.range_missing_blocks.increment(missing);
            return None;
        }
        metrics.range_hit.increment(1);
        Some(witnesses)
    }
}

/// Returns the number of witness-data bytes an [`ExecutionWitness`] retains.
fn retained_bytes(witness: &ExecutionWitness) -> usize {
    let total =
        |entries: &[alloy_primitives::Bytes]| entries.iter().map(|b| b.len()).sum::<usize>();
    total(&witness.state) + total(&witness.codes) + total(&witness.keys) + total(&witness.headers)
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

    /// A witness whose `state` entry is exactly `len` bytes.
    fn sized_witness(len: usize) -> ExecutionWitness {
        ExecutionWitness {
            state: vec![Bytes::from(vec![0u8; len])],
            ..Default::default()
        }
    }

    #[test]
    fn tracks_retained_bytes_across_replace_and_evict() {
        let cache = WitnessCache::with_depth(2);
        cache.insert(1, witness(1));
        cache.insert(2, witness(2));
        assert_eq!(cache.inner.lock().bytes, 2);

        // Replacing a block swaps its contribution instead of double-counting it.
        cache.insert(2, sized_witness(4));
        assert_eq!(cache.inner.lock().bytes, 5);

        // Evicting block 1 releases exactly its contribution.
        cache.insert(3, witness(3));
        assert_eq!(cache.inner.lock().bytes, 5);
        assert_eq!(cache.bounds(), Some((2, 3)));
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
