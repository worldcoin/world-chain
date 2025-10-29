use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::ops::Bound::{Included, Unbounded};

/// A map where each key has versions indexed by some ordered index `I`.
/// `get(index, key)` returns the value last set at or before `index`.
#[derive(Debug, Default)]
pub struct TemporalMap<K, I, V> {
    inner: HashMap<K, BTreeMap<I, V>>,
}

impl<K, I, V> TemporalMap<K, I, V>
where
    K: Eq + Hash,
    I: Ord + Clone,
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

    /// Get the value for `key` as of `index` (i.e., last set at or before `index`).
    pub fn get(&self, index: I, key: &K) -> Option<&V> {
        self.inner.get(key).and_then(|versions| {
            versions
                .range((Unbounded, Included(index)))
                .next_back()
                .map(|(_, v)| v)
        })
    }

    /// Optional: get also returns the index it came from.
    pub fn get_with_index(&self, index: I, key: &K) -> Option<(&I, &V)> {
        self.inner.get(key).and_then(|versions| {
            versions
                .range((Unbounded, Included(index)))
                .next_back()
                .map(|(i, v)| (i, v))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        let mut map = TemporalMap::<&str, i32, &str>::new();

        map.insert(1, "key_a", "val_a");
        map.insert(5, "key_b", "val_b");

        assert_eq!(map.get(0, &"key_a"), None);
        assert_eq!(map.get(2, &"key_a"), Some(&"val_a"));
        assert_eq!(map.get(5, &"key_b"), Some(&"val_b"));
        assert_eq!(map.get(10, &"key_b"), Some(&"val_b"));

        // Insert later version for key_a and query around it
        map.insert(7, "key_a", "val_a2");
        assert_eq!(map.get(6, &"key_a"), Some(&"val_a"));
        assert_eq!(map.get(7, &"key_a"), Some(&"val_a2"));
        assert_eq!(map.get(100, &"key_a"), Some(&"val_a2"));
    }
}
