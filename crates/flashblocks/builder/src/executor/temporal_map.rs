use std::{
    collections::{btree_map::Entry, BTreeMap, HashMap},
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

    /// Optional: get also returns the index it came from.
    pub fn get_with_index(&self, index: I, key: &K) -> Option<(&I, &V)> {
        let res = self.inner.get(key).and_then(|versions| {
            versions
                .range((Unbounded, Included(index)))
                .next_back()
                .map(|(i, v)| (i, v))
        });

        res
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, U256};
    use flashblocks_primitives::access_list::FlashblockAccessListData;
    use reth::revm::State;

    use revm::{
        database::{BundleState, InMemoryDB},
        primitives::KECCAK_EMPTY,
        DatabaseRef,
    };
    use serde_json::json;

    use crate::executor::temporal_db::TemporalDbFactory;

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

    #[test]
    fn construct_db() {
        reth_tracing::init_test_tracing();

        let access_list = json!({
          "access_list": {
            "changes": [
              {
                "address": "0x0000f90827f1c53a10cb7a02335b175320002935",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x0",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x0",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x000f3df6d732807ef1319fb7b8bb8522d0beac02",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x0",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x0",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x14dc79964da2c08b23698b3d3cc7ca32193d9955",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x9",
                    "postBalance": "0xd3c21bcd4ef0c231bf9c"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x9",
                    "newNonce": "0x1"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x9",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x15d34aaf54267db7d7c367839aaf71a00a2c6a65",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x6",
                    "postBalance": "0xd3c21bcd4ef0c231bf9c"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x6",
                    "newNonce": "0x1"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x6",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x23618e81e3f5cdf7f54c3d65f7fbc0abf5b21e8f",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0xa",
                    "postBalance": "0xd3c21bcd4ef0c231bf9c"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0xa",
                    "newNonce": "0x1"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0xa",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x249d8d9f75d448bfc5eb26fbf67dfc9851561a27",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0xa",
                    "postBalance": "0x64"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0xa",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0xa",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x363f1bb32154eccc385af6e331756af7ab2f341b",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x3",
                    "postBalance": "0x64"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x3",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x3",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x4",
                    "postBalance": "0xd3c21bcd4ef0c231bf9c"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x4",
                    "newNonce": "0x1"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x4",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x3f913ba8631a4bcbd5bd6813d2efa68cb579fe15",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x7",
                    "postBalance": "0x64"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x7",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x7",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x4200000000000000000000000000000000000019",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "postBalance": "0x10b643590600"
                  },
                  {
                    "blockAccessIndex": "0x3",
                    "postBalance": "0x216c86b20c00"
                  },
                  {
                    "blockAccessIndex": "0x4",
                    "postBalance": "0x3222ca0b1200"
                  },
                  {
                    "blockAccessIndex": "0x5",
                    "postBalance": "0x42d90d641800"
                  },
                  {
                    "blockAccessIndex": "0x6",
                    "postBalance": "0x538f50bd1e00"
                  },
                  {
                    "blockAccessIndex": "0x7",
                    "postBalance": "0x644594162400"
                  },
                  {
                    "blockAccessIndex": "0x8",
                    "postBalance": "0x74fbd76f2a00"
                  },
                  {
                    "blockAccessIndex": "0x9",
                    "postBalance": "0x85b21ac83000"
                  },
                  {
                    "blockAccessIndex": "0xa",
                    "postBalance": "0x96685e213600"
                  },
                  {
                    "blockAccessIndex": "0xb",
                    "postBalance": "0xa71ea17a3c00"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x3",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x4",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x5",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x6",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x7",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x8",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x9",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0xa",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0xb",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x3",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x4",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x5",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x6",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x7",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x8",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x9",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0xa",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0xb",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x420000000000000000000000000000000000001a",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x3",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x4",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x5",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x6",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x7",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x8",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x9",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0xa",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0xb",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x3",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x4",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x5",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x6",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x7",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x8",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x9",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0xa",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0xb",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x420000000000000000000000000000000000001b",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x3",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x4",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x5",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x6",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x7",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x8",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x9",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0xa",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0xb",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x3",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x4",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x5",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x6",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x7",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x8",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x9",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0xa",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0xb",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x540542cd400951444b73336eba6e6e45e2b662ec",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x4",
                    "postBalance": "0x64"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x4",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x4",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x595e640c30a8943cdb4a73719b2368872a2b27cb",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x9",
                    "postBalance": "0x64"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x9",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x9",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x69a2c894856cef6610b04fbfe40b53b90e2dba4c",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "postBalance": "0x64"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x70997970c51812dc3a010c7d01b50e0d17dc79c8",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x3",
                    "postBalance": "0xd3c21bcd4ef0c231bf9c"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x3",
                    "newNonce": "0x1"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x3",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x9009303d7371c2dddeece582de74fe1141136d79",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x6",
                    "postBalance": "0x64"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x6",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x6",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x90f79bf6eb2c4f870365e785982e1f101e93b906",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x5",
                    "postBalance": "0xd3c21bcd4ef0c231bf9c"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x5",
                    "newNonce": "0x1"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x5",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x976ea74026e726554db657fa54763abd0c3a0aa9",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x8",
                    "postBalance": "0xd3c21bcd4ef0c231bf9c"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x8",
                    "newNonce": "0x1"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x8",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0x9965507d1a55bcc2695c58ba16fb37d819b0a4dc",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x7",
                    "postBalance": "0xd3c21bcd4ef0c231bf9c"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x7",
                    "newNonce": "0x1"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x7",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0xa0ee7a142d267c1f36714e4a8f75612f20a79720",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0xb",
                    "postBalance": "0xd3c21bcd4ef0c231bf9c"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0xb",
                    "newNonce": "0x1"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0xb",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0xac44b08ae2e55e1fa7f0fbf455a65cd538ddd5c4",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "postBalance": "0x16d469b753a00"
                  },
                  {
                    "blockAccessIndex": "0x3",
                    "postBalance": "0x2da8d36ea7400"
                  },
                  {
                    "blockAccessIndex": "0x4",
                    "postBalance": "0x447d3d25fae00"
                  },
                  {
                    "blockAccessIndex": "0x5",
                    "postBalance": "0x5b51a6dd4e800"
                  },
                  {
                    "blockAccessIndex": "0x6",
                    "postBalance": "0x72261094a2200"
                  },
                  {
                    "blockAccessIndex": "0x7",
                    "postBalance": "0x88fa7a4bf5c00"
                  },
                  {
                    "blockAccessIndex": "0x8",
                    "postBalance": "0x9fcee40349600"
                  },
                  {
                    "blockAccessIndex": "0x9",
                    "postBalance": "0xb6a34dba9d000"
                  },
                  {
                    "blockAccessIndex": "0xa",
                    "postBalance": "0xcd77b771f0a00"
                  },
                  {
                    "blockAccessIndex": "0xb",
                    "postBalance": "0xe44c212944400"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x3",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x4",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x5",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x6",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x7",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x8",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0x9",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0xa",
                    "newNonce": "0x0"
                  },
                  {
                    "blockAccessIndex": "0xb",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x3",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x4",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x5",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x6",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x7",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x8",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0x9",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0xa",
                    "newCode": "0x"
                  },
                  {
                    "blockAccessIndex": "0xb",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0xb34898ac6ccd4cf0cbadb5b7f6d41cd1dd9e48d0",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x8",
                    "postBalance": "0x64"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x8",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x8",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0xc6e07bf79c57eee0c6837f93f5d7074c78d88dbb",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x5",
                    "postBalance": "0x64"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x5",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x5",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0xdbd5212285d6f40819b9a75e313106993475123f",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0xb",
                    "postBalance": "0x64"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0xb",
                    "newNonce": "0x0"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0xb",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0xdeaddeaddeaddeaddeaddeaddeaddeaddead0001",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x1",
                    "newNonce": "0x1"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x1",
                    "newCode": "0x"
                  }
                ]
              },
              {
                "address": "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266",
                "storageChanges": [],
                "storageReads": [],
                "balanceChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "postBalance": "0x161c77b7ebbbf9c"
                  }
                ],
                "nonceChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "newNonce": "0x1"
                  }
                ],
                "codeChanges": [
                  {
                    "blockAccessIndex": "0x2",
                    "newCode": "0x"
                  }
                ]
              }
            ],
            "min_tx_index": 0,
            "max_tx_index": 12
          },
          "access_list_hash": "0x923f08b9864c21f3d63c123c3e2e31349d82c4215d94dc66281052fcbf13dbd1"
        });

        let access_list = serde_json::from_value::<FlashblockAccessListData>(access_list).unwrap();
        let database = InMemoryDB::default();

        let bundle = BundleState::default();
        let database = TemporalDbFactory::new(&database, access_list.access_list.clone(), &bundle);

        // Test at block access index 3 - should see values < 3 (i.e., indices 0, 1, 2)
        let db = database.db(3);
        let state = State::builder().with_database_ref(&db).build();

        // Address with multiple balance changes - should have balance at index 2 (last one < 3)
        let account = address!("0x4200000000000000000000000000000000000019");
        let account_info = state.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("10b643590600", 16).unwrap()
        ); // index 2
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Address accessed at index 0
        let account = address!("0x000f3df6d732807ef1319fb7b8bb8522d0beac02");
        let account_info = state.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(account_info.balance, U256::ZERO);
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Address with changes at index 2 - should be visible
        let account = address!("0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266");
        let account_info = state.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 1);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("161c77b7ebbbf9c", 16).unwrap()
        ); // index 2
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Recipient at index 2
        let account = address!("0x69a2c894856cef6610b04fbfe40b53b90e2dba4c");
        let account_info = state.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("64", 16).unwrap()
        );
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Address with changes at index 3 should NOT be visible
        let account = address!("0x70997970c51812dc3a010c7d01b50e0d17dc79c8");
        assert!(state.basic_ref(account).unwrap().is_none());

        // Sequencer fee vault with multiple changes - should see index 2
        let account = address!("0xac44b08ae2e55e1fa7f0fbf455a65cd538ddd5c4");
        let account_info = state.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("16d469b753a00", 16).unwrap()
        ); // index 2
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // L1 fee vault with multiple changes - should see index 2
        let account = address!("0x420000000000000000000000000000000000001a");
        let account_info = state.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(account_info.balance, U256::ZERO);
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Test at block access index 0 - should see NO values (< 0 means nothing)
        let db_0 = database.db(0);
        let state_0 = State::builder().with_database_ref(&db_0).build();

        // Even index 0 addresses should not be visible
        let account = address!("0x000f3df6d732807ef1319fb7b8bb8522d0beac02");
        assert!(state_0.basic_ref(account).unwrap().is_none());

        // Address with changes at index 2 should not be visible at index 0
        let account = address!("0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266");
        assert!(state_0.basic_ref(account).unwrap().is_none());

        // Test at block access index 1 - should see only index 0
        let db_1 = database.db(1);
        let state_1 = State::builder().with_database_ref(&db_1).build();

        let account = address!("0x000f3df6d732807ef1319fb7b8bb8522d0beac02");
        let account_info = state_1.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(account_info.balance, U256::ZERO);
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Test at block access index 10 (0xa) - should see values < 10 (i.e., indices 0-9)
        let db_10 = database.db(10);
        let state_10 = State::builder().with_database_ref(&db_10).build();

        // Address with change at index 9 - should be visible
        let account = address!("0x14dc79964da2c08b23698b3d3cc7ca32193d9955");
        let account_info = state_10.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 1);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("d3c21bcd4ef0c231bf9c", 16).unwrap()
        );
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Recipient at index 9
        let account = address!("0x595e640c30a8943cdb4a73719b2368872a2b27cb");
        let account_info = state_10.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("64", 16).unwrap()
        );
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Address with change at index 10 should NOT be visible
        let account = address!("0x23618e81e3f5cdf7f54c3d65f7fbc0abf5b21e8f");
        assert!(state_10.basic_ref(account).unwrap().is_none());

        // L1 fee recipient with multiple balance changes - should see index 9 (last < 10)
        let account = address!("0x4200000000000000000000000000000000000019");
        let account_info = state_10.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("85b21ac83000", 16).unwrap()
        ); // index 9
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Sequencer fee vault at index 9
        let account = address!("0xac44b08ae2e55e1fa7f0fbf455a65cd538ddd5c4");
        let account_info = state_10.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("b6a34dba9d000", 16).unwrap()
        ); // index 9
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Test at block access index 11 (0xb) - should see values < 11 (i.e., indices 0-10)
        let db_11 = database.db(11);
        let state_11 = State::builder().with_database_ref(&db_11).build();

        // Sender at index 10 - should be visible
        let account = address!("0x23618e81e3f5cdf7f54c3d65f7fbc0abf5b21e8f");
        let account_info = state_11.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 1);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("d3c21bcd4ef0c231bf9c", 16).unwrap()
        );
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Recipient at index 10
        let account = address!("0x249d8d9f75d448bfc5eb26fbf67dfc9851561a27");
        let account_info = state_11.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("64", 16).unwrap()
        );
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Address with changes at index 11 should NOT be visible
        let account = address!("0xa0ee7a142d267c1f36714e4a8f75612f20a79720");
        assert!(state_11.basic_ref(account).unwrap().is_none());

        // L1 fee recipient balance at index 10 (last < 11)
        let account = address!("0x4200000000000000000000000000000000000019");
        let account_info = state_11.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("96685e213600", 16).unwrap()
        ); // index 10
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Sequencer fee vault balance at index 10
        let account = address!("0xac44b08ae2e55e1fa7f0fbf455a65cd538ddd5c4");
        let account_info = state_11.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("cd77b771f0a00", 16).unwrap()
        ); // index 10
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Test querying at an index between changes - db(5) sees indices 0-4
        let db_5 = database.db(5);
        let state_5 = State::builder().with_database_ref(&db_5).build();

        // Should see index 4 state (last < 5)
        let account = address!("0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc");
        let account_info = state_5.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 1);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("d3c21bcd4ef0c231bf9c", 16).unwrap()
        );
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Recipient at index 4
        let account = address!("0x540542cd400951444b73336eba6e6e45e2b662ec");
        let account_info = state_5.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("64", 16).unwrap()
        );
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Should see index 3 state for addresses that changed at index 3
        let account = address!("0x70997970c51812dc3a010c7d01b50e0d17dc79c8");
        let account_info = state_5.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 1);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("d3c21bcd4ef0c231bf9c", 16).unwrap()
        );
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Should not see index 5 changes
        let account = address!("0x90f79bf6eb2c4f870365e785982e1f101e93b906");
        assert!(state_5.basic_ref(account).unwrap().is_none());

        // Should not see index 6 changes
        let account = address!("0x15d34aaf54267db7d7c367839aaf71a00a2c6a65");
        assert!(state_5.basic_ref(account).unwrap().is_none());

        // Test at block access index 12 - should see all values (indices 0-11)
        let db_12 = database.db(12);
        let state_12 = State::builder().with_database_ref(&db_12).build();

        // Should see the last transaction at index 11
        let account = address!("0xa0ee7a142d267c1f36714e4a8f75612f20a79720");
        let account_info = state_12.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 1);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("d3c21bcd4ef0c231bf9c", 16).unwrap()
        );
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Recipient at index 11
        let account = address!("0xdbd5212285d6f40819b9a75e313106993475123f");
        let account_info = state_12.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("64", 16).unwrap()
        );
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Final L1 fee recipient balance at index 11
        let account = address!("0x4200000000000000000000000000000000000019");
        let account_info = state_12.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("a71ea17a3c00", 16).unwrap()
        ); // index 11
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);

        // Final sequencer fee vault balance at index 11
        let account = address!("0xac44b08ae2e55e1fa7f0fbf455a65cd538ddd5c4");
        let account_info = state_12.basic_ref(account).unwrap().unwrap();
        assert_eq!(account_info.nonce, 0);
        assert_eq!(
            account_info.balance,
            U256::from_str_radix("e44c212944400", 16).unwrap()
        ); // index 11
        assert_eq!(account_info.code_hash, KECCAK_EMPTY);
    }
}
