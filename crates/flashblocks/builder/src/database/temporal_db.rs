use std::sync::{
    Arc,
    atomic::{AtomicU16, Ordering},
};

use alloy_primitives::{Address, B256};
use flashblocks_primitives::access_list::FlashblockAccessList;
use revm::{
    DatabaseRef,
    bytecode::bitvec::store::BitStore,
    primitives::{HashMap, StorageKey, StorageValue},
    state::{AccountInfo, Bytecode},
};

use super::temporal_map::TemporalMap;

/// Represents the temporal state of accounts, storage, and contracts
#[derive(Clone, Debug, Default)]
pub struct TemporalState {
    /// Block state account with account info
    ///
    /// We prioritize access speed over memory usage here by having a single map
    /// for each field in AccountInfo, rather than multiple maps for each field.
    pub account_info: TemporalMap<Address, AccountInfo, u64>,
    /// Block state account with account info
    pub account_storage: HashMap<Address, TemporalMap<StorageKey, StorageValue, u64>>,
    /// Created contracts
    pub contracts: TemporalMap<B256, Bytecode, u64>,
}

/// A factory for creating TemporalDb instances at specific BlockAccessIndices
#[derive(Clone, Debug)]
pub struct TemporalDbFactory<DB: DatabaseRef> {
    /// Layer 0: Cached Pre-State from the BAL
    pub cache: TemporalState,
    /// Layer 1: The underlying database
    pub db: DB,
}

impl<DB: DatabaseRef + Clone> TemporalDbFactory<DB> {
    /// Build a new TemporalDbFactory from a FlashblockAccessList
    ///
    /// This will prepopulate the cache with the changes from the access list
    /// so that TemporalDb instances can be created for specific indices.
    pub fn new(db: DB, list: FlashblockAccessList) -> Self {
        let mut cache = TemporalState::default();

        for change in list.changes {
            // First process storage changes (these don't affect account info)
            for storage_change in change.storage_changes {
                for slot in storage_change.changes {
                    let storage_entry = cache.account_storage.entry(change.address).or_default();
                    storage_entry.insert(
                        // We add 1 to the index because this `TemporalState` acts as the DB,
                        // meaning tx `block_access_index + 1` must read the storage value written
                        // by the previous transactions, not itself.
                        // The same reasoning applies to below indexes.
                        slot.block_access_index + 1,
                        storage_change.slot.into(),
                        slot.new_value.into(),
                    );
                }
            }

            // Collect all account info changes with their indices
            // We need to process them in order of block_access_index to correctly
            // propagate values (e.g., nonce changes should be visible to later balance changes)
            let mut indexed_changes: Vec<(
                u64,
                Option<alloy_primitives::U256>,
                Option<u64>,
                Option<(B256, Bytecode)>,
            )> = Vec::new();

            for balance_change in &change.balance_changes {
                indexed_changes.push((
                    balance_change.block_access_index,
                    Some(balance_change.post_balance),
                    None,
                    None,
                ));
            }

            for nonce_change in &change.nonce_changes {
                indexed_changes.push((
                    nonce_change.block_access_index,
                    None,
                    Some(nonce_change.new_nonce),
                    None,
                ));
            }

            for code_change in &change.code_changes {
                let bytecode = Bytecode::new_raw(code_change.new_code.clone());
                let hash = bytecode.hash_slow();
                indexed_changes.push((
                    code_change.block_access_index,
                    None,
                    None,
                    Some((hash, bytecode)),
                ));
            }

            // Sort by index to process in order
            indexed_changes.sort_by_key(|(idx, _, _, _)| *idx);

            // Helper to get the current account state at a given index
            let acc_at = |cache: &TemporalState, index| {
                cache
                    .account_info
                    .get(index, &change.address)
                    .cloned()
                    .unwrap_or_else(|| db.basic_ref(change.address).unwrap().unwrap_or_default())
            };

            // Process changes in order of block_access_index
            for (index, balance, nonce, code) in indexed_changes {
                let storage_index = index + 1;
                let mut acc = acc_at(&cache, storage_index);

                if let Some(new_balance) = balance {
                    acc.balance = new_balance;
                }

                if let Some(new_nonce) = nonce {
                    acc.nonce = new_nonce;
                }

                if let Some((hash, bytecode)) = code {
                    acc.code = Some(bytecode.clone());
                    acc.code_hash = hash;
                    cache.contracts.insert(storage_index, hash, bytecode);
                }

                cache
                    .account_info
                    .insert(storage_index, change.address, acc);
            }
        }

        TemporalDbFactory { db, cache }
    }

    /// Creates a new [`TemporalDb`] at a given [`BlockAccessIndex`]
    pub fn db(&self, index: u16) -> TemporalDb<DB> {
        TemporalDb::new(self.db.clone(), self.cache.clone(), index)
    }
}

#[derive(Clone, Debug)]
pub struct TemporalDb<DB: DatabaseRef> {
    /// Layer 0: Cached Pre-State from the BAL
    pub cache: TemporalState,
    /// Layer 1: The underlying database
    pub db: DB,
    /// The index being referenced inside the [`TemporalState`]
    pub index: u16,
}

impl<DB: DatabaseRef> TemporalDb<DB> {
    pub fn set_index(&mut self, index: u16) {
        self.index = index
    }
}

impl<DB: DatabaseRef> TemporalDb<DB> {
    pub fn new(db: DB, cache: TemporalState, index: u16) -> Self {
        TemporalDb { db, cache, index }
    }
}

impl<DB: DatabaseRef> DatabaseRef for TemporalDb<DB> {
    type Error = DB::Error;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        match self
            .cache
            .account_info
            .get_with_index(self.index as u64, &address)
        {
            Some((_, acc)) => Ok(Some(acc.clone())),
            None => self.db.basic_ref(address),
        }
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        match self.cache.contracts.get(self.index as u64, &code_hash) {
            Some(entry) => Ok(entry.clone()),
            None => self.db.code_by_hash_ref(code_hash),
        }
    }

    fn storage_ref(
        &self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        match self.cache.account_storage.get(&address) {
            Some(storage) => match storage.get(self.index as u64, &index) {
                Some(val) => Ok(*val),
                None => self.db.storage_ref(address, index),
            },
            None => self.db.storage_ref(address, index),
        }
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.db.block_hash_ref(number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_eip7928::{
        AccountChanges, BalanceChange, CodeChange, NonceChange, SlotChanges, StorageChange,
    };
    use alloy_primitives::{U256, address, b256, bytes};
    use flashblocks_primitives::access_list::FlashblockAccessListData;
    use lazy_static::lazy_static;
    use reth::revm::State;
    use revm::{
        database::{CacheDB, EmptyDB, InMemoryDB},
        primitives::KECCAK_EMPTY,
    };
    use serde_json::json;

    #[test]
    fn test_temporal_db_factory_with_storage_changes() {
        let addr = address!("0000000000000000000000000000000000000001");
        let slot = b256!("0000000000000000000000000000000000000000000000000000000000000001");
        let value1 = U256::from(100);
        let value2 = U256::from(200);

        let access_list = FlashblockAccessList {
            changes: vec![AccountChanges {
                address: addr,
                storage_changes: vec![SlotChanges {
                    slot,
                    changes: vec![
                        StorageChange {
                            block_access_index: 1,
                            new_value: value1.into(),
                        },
                        StorageChange {
                            block_access_index: 3,
                            new_value: value2.into(),
                        },
                    ],
                }],
                storage_reads: vec![],
                balance_changes: vec![],
                nonce_changes: vec![],
                code_changes: vec![],
            }],
            min_tx_index: 0,
            max_tx_index: 5,
        };

        let db = EmptyDB::new();
        let factory = TemporalDbFactory::new(db, access_list);

        // Check storage at different indices
        let temporal_db_1 = factory.db(1);
        assert_eq!(temporal_db_1.storage_ref(addr, slot.into()).unwrap(), 0);

        let temporal_db_2 = factory.db(2);
        assert_eq!(
            temporal_db_2.storage_ref(addr, slot.into()).unwrap(),
            value1
        );

        let temporal_db_3 = factory.db(3);
        assert_eq!(
            temporal_db_3.storage_ref(addr, slot.into()).unwrap(),
            value1
        );

        let temporal_db_4 = factory.db(4);
        assert_eq!(
            temporal_db_4.storage_ref(addr, slot.into()).unwrap(),
            value2
        );
    }

    #[test]
    fn test_temporal_db_factory_with_balance_changes() {
        let addr = address!("0000000000000000000000000000000000000001");
        let initial_balance = U256::from(1000);
        let new_balance = U256::from(500);

        let initial_account = AccountInfo {
            balance: initial_balance,
            nonce: 1,
            code_hash: KECCAK_EMPTY,
            code: None,
        };

        let access_list = FlashblockAccessList {
            changes: vec![AccountChanges {
                address: addr,
                storage_changes: vec![],
                storage_reads: vec![],
                balance_changes: vec![BalanceChange {
                    block_access_index: 2,
                    post_balance: new_balance,
                }],
                nonce_changes: vec![],
                code_changes: vec![],
            }],
            min_tx_index: 0,
            max_tx_index: 5,
        };

        let mut db = CacheDB::new(EmptyDB::new());
        db.insert_account_info(addr, initial_account);

        let factory = TemporalDbFactory::new(db, access_list);

        // Before the change
        let temporal_db_1 = factory.db(0);
        let account_1 = temporal_db_1.basic_ref(addr).unwrap().unwrap();
        assert_eq!(account_1.balance, initial_balance);

        // At the change index
        let temporal_db_2 = factory.db(0);
        let account_2 = temporal_db_2.basic_ref(addr).unwrap().unwrap();
        assert_eq!(account_2.balance, initial_balance);

        // After the change
        let temporal_db_3 = factory.db(3);
        let account_3 = temporal_db_3.basic_ref(addr).unwrap().unwrap();
        assert_eq!(account_3.balance, new_balance);
    }

    #[test]
    fn test_temporal_db_factory_with_nonce_changes() {
        let addr = address!("0000000000000000000000000000000000000001");
        let initial_nonce = 5u64;
        let new_nonce = 10u64;

        let initial_account = AccountInfo {
            balance: U256::from(1000),
            nonce: initial_nonce,
            code_hash: KECCAK_EMPTY,
            code: None,
        };

        let access_list = FlashblockAccessList {
            changes: vec![AccountChanges {
                address: addr,
                storage_changes: vec![],
                storage_reads: vec![],
                balance_changes: vec![],
                nonce_changes: vec![NonceChange {
                    block_access_index: 2,
                    new_nonce,
                }],
                code_changes: vec![],
            }],
            min_tx_index: 0,
            max_tx_index: 5,
        };

        let mut db = CacheDB::new(EmptyDB::new());
        db.insert_account_info(addr, initial_account);
        let factory = TemporalDbFactory::new(db, access_list);
        // Before the change
        let temporal_db_1 = factory.db(1);
        let account_1 = temporal_db_1.basic_ref(addr).unwrap().unwrap();
        assert_eq!(account_1.nonce, initial_nonce);

        // After the change
        let temporal_db_3 = factory.db(3);
        let account_3 = temporal_db_3.basic_ref(addr).unwrap().unwrap();
        assert_eq!(account_3.nonce, new_nonce);
    }

    #[test]
    fn test_temporal_db_factory_with_code_changes() {
        let addr = address!("0000000000000000000000000000000000000001");
        let new_code = bytes!("60806040");
        let new_bytecode = Bytecode::new_raw(new_code.clone());
        let new_code_hash = new_bytecode.hash_slow();

        let initial_account = AccountInfo {
            balance: U256::from(1000),
            nonce: 1,
            code_hash: KECCAK_EMPTY,
            code: None,
        };

        let access_list = FlashblockAccessList {
            changes: vec![AccountChanges {
                address: addr,
                storage_changes: vec![],
                storage_reads: vec![],
                balance_changes: vec![],
                nonce_changes: vec![],
                code_changes: vec![CodeChange {
                    block_access_index: 2,
                    new_code: new_code.clone(),
                }],
            }],
            min_tx_index: 0,
            max_tx_index: 5,
        };

        let mut db = CacheDB::new(EmptyDB::new());
        db.insert_account_info(addr, initial_account);

        let factory = TemporalDbFactory::new(db, access_list);

        // Before the change
        let temporal_db_1 = factory.db(1);
        let account_1 = temporal_db_1.basic_ref(addr).unwrap().unwrap();
        assert_eq!(account_1.code_hash, KECCAK_EMPTY);

        // After the change
        let temporal_db_3 = factory.db(3);
        let account_3 = temporal_db_3.basic_ref(addr).unwrap().unwrap();
        assert_eq!(account_3.code_hash, new_code_hash);
        assert!(account_3.code.is_some());
        // Check that the bytecode starts with the expected code
        let code_ref = account_3.code.as_ref().unwrap();
        assert!(code_ref.bytecode().starts_with(&new_code));
    }

    #[test]
    fn test_temporal_db_factory_with_multiple_changes() {
        let addr = address!("0000000000000000000000000000000000000001");
        let slot = b256!("0000000000000000000000000000000000000000000000000000000000000001");

        let initial_account = AccountInfo {
            balance: U256::from(1000),
            nonce: 1,
            code_hash: KECCAK_EMPTY,
            code: None,
        };

        let access_list = FlashblockAccessList {
            changes: vec![AccountChanges {
                address: addr,
                storage_changes: vec![SlotChanges {
                    slot,
                    changes: vec![StorageChange {
                        block_access_index: 1,
                        new_value: U256::from(100).into(),
                    }],
                }],
                storage_reads: vec![],
                balance_changes: vec![BalanceChange {
                    block_access_index: 2,
                    post_balance: U256::from(500),
                }],
                nonce_changes: vec![NonceChange {
                    block_access_index: 3,
                    new_nonce: 10,
                }],
                code_changes: vec![],
            }],
            min_tx_index: 0,
            max_tx_index: 5,
        };

        let mut db = CacheDB::new(EmptyDB::new());
        db.insert_account_info(addr, initial_account);

        let factory = TemporalDbFactory::new(db, access_list);

        // At index 4, all changes should be visible
        let temporal_db = factory.db(4);
        dbg!(&temporal_db);
        let account = temporal_db.basic_ref(addr).unwrap().unwrap();
        assert_eq!(account.balance, U256::from(500));
        assert_eq!(account.nonce, 10);

        let storage = temporal_db.storage_ref(addr, slot.into()).unwrap();
        assert_eq!(storage, U256::from(100));
    }

    #[test]
    fn test_temporal_db_basic_ref_fallback_to_db() {
        let addr = address!("0000000000000000000000000000000000000001");
        let other_addr = address!("0000000000000000000000000000000000000002");

        let account_info = AccountInfo {
            balance: U256::from(999),
            nonce: 7,
            code_hash: KECCAK_EMPTY,
            code: None,
        };

        let mut db = CacheDB::new(EmptyDB::new());
        db.insert_account_info(other_addr, account_info.clone());

        let factory = TemporalDbFactory::new(db, FlashblockAccessList::default());
        let temporal_db = factory.db(0);

        // Address not in cache should fall back to DB
        let result = temporal_db.basic_ref(other_addr).unwrap().unwrap();
        assert_eq!(result.balance, U256::from(999));
        assert_eq!(result.nonce, 7);

        // Address not in cache or DB should return None
        let result = temporal_db.basic_ref(addr).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_temporal_db_storage_ref_fallback_to_db() {
        let addr = address!("0000000000000000000000000000000000000001");
        let slot = b256!("0000000000000000000000000000000000000000000000000000000000000001");
        let value = U256::from(777);

        let mut db = CacheDB::new(EmptyDB::new());
        let _ = db.insert_account_storage(addr, slot.into(), value);
        let factory = TemporalDbFactory::new(db, FlashblockAccessList::default());
        let temporal_db = factory.db(0);
        let result = temporal_db.storage_ref(addr, slot.into()).unwrap();
        assert_eq!(result, value);
    }

    #[test]
    fn test_temporal_db_code_by_hash_ref_fallback_to_db() {
        let code = bytes!("60806040");
        let mut account = AccountInfo::default().with_code(Bytecode::new_raw(code.clone()));

        let mut db = CacheDB::new(EmptyDB::new());
        db.insert_contract(&mut account);
        let factory = TemporalDbFactory::new(db, FlashblockAccessList::default());
        let temporal_db = factory.db(0);
        let result = temporal_db.code_by_hash_ref(account.code_hash).unwrap();
        // Check that the bytecode starts with the expected code
        assert!(result.bytecode().starts_with(&code));
    }

    #[test]
    fn test_temporal_db_storage_ref_no_account_storage() {
        let addr = address!("0000000000000000000000000000000000000001");
        let slot = b256!("0000000000000000000000000000000000000000000000000000000000000001");
        let value = U256::from(555);

        let mut db = CacheDB::new(EmptyDB::new());
        let _ = db.insert_account_storage(addr, slot.into(), value);

        let factory = TemporalDbFactory::new(db, FlashblockAccessList::default());

        let temporal_db = factory.db(0);
        // No storage in cache for this address, should fall back to DB
        let result = temporal_db.storage_ref(addr, slot.into()).unwrap();
        assert_eq!(result, value);
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

        let database = TemporalDbFactory::new(database, access_list.access_list.clone());

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

    /// Test the exact scenario from proptest: new contract deployed at index 1, queried at index 2
    #[test]
    fn test_new_contract_deployment_at_index_1_query_at_index_2() {
        // This simulates the exact proptest scenario:
        // - tx1 at index 1 deploys new ChaosTest at 0x2e63...
        // - tx2 at index 2 needs to see and execute this code via DELEGATECALL

        let deployed_addr = address!("2e63c703ce06b779ed2e7250825c5a810be4ccc6");
        let new_code = bytes!(
            "6080604052348015600e575f5ffd5b50600436106026575f3560e01c8063c6c2ea1714602a575b5f5ffd5b6039603536600460a0565b604b565b60405190815260200160405180910390f35b5f6001818155600255818015608e576001811460955760025b6001840181101560825780545f198201540160019091019081556064565b5082600101549150609a565b5f9150609a565b600191505b50919050565b5f6020828403121560af575f5ffd5b503591905056"
        );
        let new_bytecode = Bytecode::new_raw(new_code.clone());
        let expected_code_hash = new_bytecode.hash_slow();

        let access_list = FlashblockAccessList {
            changes: vec![AccountChanges {
                address: deployed_addr,
                storage_changes: vec![],
                storage_reads: vec![],
                balance_changes: vec![],
                nonce_changes: vec![NonceChange {
                    block_access_index: 1,
                    new_nonce: 1,
                }],
                code_changes: vec![CodeChange {
                    block_access_index: 1,
                    new_code: new_code.clone(),
                }],
            }],
            min_tx_index: 0,
            max_tx_index: 3,
        };

        // Empty DB - the contract doesn't exist initially
        let db = EmptyDB::new();
        let factory = TemporalDbFactory::new(db, access_list);

        // Verify the cache was populated correctly
        assert!(
            factory
                .cache
                .account_info
                .keys()
                .any(|&a| a == deployed_addr),
            "Cache should contain the deployed address"
        );

        // Query at index 1 - the deploying transaction itself
        // It should NOT see its own code yet (code is available AFTER index 1, i.e., at storage_index = 2)
        let temporal_db_1 = factory.db(1);
        let account_at_1 = temporal_db_1.basic_ref(deployed_addr).unwrap();
        assert!(
            account_at_1.is_none(),
            "At index 1, the contract should not exist yet (it's being created)"
        );

        // Query at index 2 - the next transaction should see the deployed code
        let temporal_db_2 = factory.db(2);
        let account_at_2 = temporal_db_2.basic_ref(deployed_addr).unwrap();
        assert!(
            account_at_2.is_some(),
            "At index 2, the deployed contract should be visible"
        );

        let account_info = account_at_2.unwrap();
        assert_eq!(
            account_info.code_hash, expected_code_hash,
            "Code hash should match deployed code"
        );
        assert!(
            account_info.code.is_some(),
            "Account should have code attached"
        );
        assert_eq!(
            account_info.nonce, 1,
            "Nonce should be 1 after contract creation"
        );

        // Verify bytecode lookup works
        let bytecode = temporal_db_2.code_by_hash_ref(expected_code_hash).unwrap();
        assert!(
            bytecode.bytecode().starts_with(&new_code),
            "Bytecode should match deployed code"
        );
    }
}
