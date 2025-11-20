use alloy_primitives::{Address, B256};
use flashblocks_primitives::access_list::FlashblockAccessList;
use revm::{
    database::BundleState,
    primitives::{HashMap, StorageKey, StorageValue},
    state::{AccountInfo, Bytecode},
    DatabaseRef,
};

use crate::executor::temporal_map::TemporalMap;

#[derive(Clone, Debug, Default)]
pub struct TemporalState {
    /// Block state account with account info
    pub account_info: TemporalMap<Address, AccountInfo, u64>,
    /// Block state account with account info
    pub account_storage: HashMap<Address, TemporalMap<StorageKey, StorageValue, u64>>,
    /// Created contracts
    pub contracts: TemporalMap<B256, Bytecode, u64>,
    /// Has EIP-161 state clear enabled (Spurious Dragon hardfork)
    pub has_state_clear: bool,
}

#[derive(Clone, Debug)]
pub struct TemporalDbFactory<'a, DB: DatabaseRef> {
    /// Layer 0: Cached Pre-State from the BAL
    pub cache: TemporalState,
    /// Layer 1: Underlying [`BundleState`] from prior flashblocks _or_ the pre-execution changes.
    pub bundle: &'a BundleState,
    /// Layer 2: The underlying database
    pub db: &'a DB,
}

impl<'a, DB: DatabaseRef> TemporalDbFactory<'a, DB> {
    /// Build a new TemporalDbFactory from a FlashblockAccessList
    ///
    /// This will prepopulate the cache with the changes from the access list
    /// so that TemporalDb instances can be created for specific indices.
    pub fn new(db: &'a DB, list: FlashblockAccessList, bundle: &'a BundleState) -> Self {
        let mut cache = TemporalState::default();

        for change in list.changes {
            for storage_change in change.storage_changes {
                for slot in storage_change.changes {
                    let storage_entry = cache.account_storage.entry(change.address).or_default();
                    storage_entry.insert(
                        slot.block_access_index + 1,
                        storage_change.slot.into(),
                        slot.new_value.into(),
                    );
                }
            }
            // TODO: We can prewarm these
            // for storage_reads in change.storage_reads {}
            for balance_change in change.balance_changes {
                cache
                    .account_info
                    .entry(balance_change.block_access_index + 1, change.address)
                    .and_modify(|acc| acc.balance = balance_change.post_balance)
                    .or_insert(AccountInfo {
                        balance: balance_change.post_balance,
                        ..Default::default()
                    });
            }

            for nonce_change in change.nonce_changes {
                cache
                    .account_info
                    .entry(nonce_change.block_access_index + 1, change.address)
                    .and_modify(|acc| acc.nonce = nonce_change.new_nonce)
                    .or_insert(AccountInfo {
                        nonce: nonce_change.new_nonce,
                        ..Default::default()
                    });
            }

            for code_change in change.code_changes {
                let bytecode = Bytecode::new_raw(code_change.new_code.clone());
                cache.contracts.insert(
                    code_change.block_access_index + 1,
                    bytecode.hash_slow(),
                    bytecode,
                );
            }
        }

        TemporalDbFactory { db, cache, bundle }
    }

    /// Creates a new [`TemporalDb`] at a given [`BlockAccessIndex`]
    pub fn db(&'a self, index: u64) -> TemporalDb<'a, DB> {
        TemporalDb::new(self.db, &self.cache, self.bundle, index)
    }
}

#[derive(Clone, Debug)]
pub struct TemporalDb<'a, DB: DatabaseRef> {
    /// Layer 0: Cached Pre-State from the BAL
    pub cache: &'a TemporalState,
    /// Layer 1: Underlying [`BundleState`] from prior flashblocks _or_ the pre-execution changes.
    pub bundle: &'a BundleState,
    /// Layer 2: The underlying database
    pub db: &'a DB,
    /// The index being referenced inside the [`TemporalState`]
    pub index: u64,
}

impl<'a, DB: DatabaseRef> TemporalDb<'a, DB> {
    pub fn new(db: &'a DB, cache: &'a TemporalState, bundle: &'a BundleState, index: u64) -> Self {
        TemporalDb {
            db,
            cache,
            bundle,
            index,
        }
    }
}

impl<'a, DB: DatabaseRef> DatabaseRef for TemporalDb<'a, DB> {
    type Error = DB::Error;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        match self.cache.account_info.get_with_index(self.index, &address) {
            Some((_, acc)) => Ok(Some(acc.clone())),
            None => {
                if let Some(account) = self.bundle.account(&address) {
                    let present_info = account.account_info();
                    if let Some(info) = present_info {
                        return Ok(Some(info));
                    }
                };

                self.db.basic_ref(address)
            }
        }
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        match self.cache.contracts.get(self.index, &code_hash) {
            Some(entry) => Ok(entry.clone()),
            None => {
                // check the bundle state
                if let Some(bytecode) = self.bundle.bytecode(&code_hash) {
                    return Ok(bytecode);
                }

                self.db.code_by_hash_ref(code_hash)
            }
        }
    }

    fn storage_ref(
        &self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        // TODO: check this impl
        match self.cache.account_storage.get(&address) {
            Some(storage) => match storage.get(self.index, &index) {
                Some(val) => Ok(*val),
                None => self.db.storage_ref(address, index),
            },
            None => {
                if let Some(account) = self.bundle.account(&address) {
                    if let Some(storage) = account.storage_slot(index) {
                        return Ok(storage);
                    }
                }

                self.db.storage_ref(address, index)
            }
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
    use alloy_primitives::{address, b256, bytes, U256};
    use revm::{
        database::{CacheDB, EmptyDB},
        primitives::KECCAK_EMPTY,
    };

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
        let bundle = BundleState::default();
        let factory = TemporalDbFactory::new(&db, access_list, &bundle);

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

        let bundle = BundleState::default();
        let factory = TemporalDbFactory::new(&db, access_list, &bundle);
        // Before the change
        let temporal_db_1 = factory.db(1);
        let account_1 = temporal_db_1.basic_ref(addr).unwrap().unwrap();
        assert_eq!(account_1.balance, initial_balance);

        // At the change index
        let temporal_db_2 = factory.db(2);
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
        let bundle = BundleState::default();
        let factory = TemporalDbFactory::new(&db, access_list, &bundle);
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

        let bundle = BundleState::default();
        let factory = TemporalDbFactory::new(&db, access_list, &bundle);

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

        let bundle = BundleState::default();
        let factory = TemporalDbFactory::new(&db, access_list, &bundle);

        // At index 4, all changes should be visible
        let temporal_db = factory.db(4);
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
        let bundle = BundleState::default();

        let factory = TemporalDbFactory::new(&db, FlashblockAccessList::default(), &bundle);
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
        let bundle = BundleState::default();
        let factory = TemporalDbFactory::new(&db, FlashblockAccessList::default(), &bundle);
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
        let bundle = BundleState::default();
        let factory = TemporalDbFactory::new(&db, FlashblockAccessList::default(), &bundle);
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

        let bundle = BundleState::default();
        let factory = TemporalDbFactory::new(&db, FlashblockAccessList::default(), &bundle);

        let temporal_db = factory.db(0);
        // No storage in cache for this address, should fall back to DB
        let result = temporal_db.storage_ref(addr, slot.into()).unwrap();
        assert_eq!(result, value);
    }
}
