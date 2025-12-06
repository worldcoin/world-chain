use std::{
    collections::{BTreeMap, HashMap, btree_map::Entry},
    fmt::{self},
    hash::Hash,
    ops::Bound::{Included, Unbounded},
};

use crate::access_list::BlockAccessIndex;

/// A map where each key has versions indexed by some ordered index `I`.
/// `get(index, key)` returns the value last set at or before `index`.
#[derive(Clone, Debug, Default)]
pub struct TemporalMap<K, V, I = BlockAccessIndex> {
    inner: HashMap<K, BTreeMap<I, V>>,
}

impl<K, V, I> TemporalMap<K, V, I>
where
    K: fmt::Debug + Eq + Hash,
    I: Ord + Default + Clone + fmt::Debug,
    V: fmt::Debug,
{
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    /// Insert a value versioned at `index` for `key`.
    pub fn insert(&mut self, index: I, key: K, value: V) {
        self.inner.entry(key).or_default().insert(index, value);
    }

    pub fn entry(&mut self, index: I, key: K) -> Entry<'_, I, V> {
        self.inner.entry(key).or_default().entry(index)
    }

    /// Get the value for `key` as of `index` (i.e., last set at or before `index`).
    pub fn get(&self, index: I, key: &K) -> Option<&V> {
        self.get_with_index(index, key).map(|(_, v)| v)
    }

    /// Returns the latest value in the map up to and including the given index.
    ///
    /// Optional: get also returns the index it came from.
    pub fn get_with_index(&self, index: I, key: &K) -> Option<(&I, &V)> {
        self.inner
            .get(key)
            .and_then(|versions| versions.range((Unbounded, Included(index))).next_back())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        let mut map = TemporalMap::<&str, &str, i32>::new();

        map.insert(1, "key_a", "val_a");
        map.insert(5, "key_b", "val_b");
        map.insert(6, "key_a", "val_c");

        // TemporalMap.get(index, key) returns the value at or before the index (inclusive)
        assert_eq!(map.get(0, &"key_a"), None); // no value at or before 0
        assert_eq!(map.get(1, &"key_a"), Some(&"val_a")); // value at 1
        assert_eq!(map.get(2, &"key_a"), Some(&"val_a")); // value at 1 (last before 2)
        assert_eq!(map.get(5, &"key_b"), Some(&"val_b")); // value at 5
        assert_eq!(map.get(6, &"key_a"), Some(&"val_c")); // value at 6
        assert_eq!(map.get(7, &"key_a"), Some(&"val_c")); // value at 6 (last before 7)
        assert_eq!(map.get(10, &"key_b"), Some(&"val_b")); // value at 5 (last before 10)

        // Insert later version for key_a and query around it
        map.insert(7, "key_a", "val_a2");
        assert_eq!(map.get(6, &"key_a"), Some(&"val_c")); // value at 6
        assert_eq!(map.get(7, &"key_a"), Some(&"val_a2")); // value at 7
        assert_eq!(map.get(100, &"key_a"), Some(&"val_a2")); // value at 7 (last before 100)
    }
}
