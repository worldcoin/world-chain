use alloy_primitives::{Address, B256};
use flashblocks_primitives::access_list::FlashblockAccessList;
use revm::{
    database::AccountStatus,
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

impl TemporalState {
    fn init_or_load<'a, DB: DatabaseRef>(
        &mut self,
        db: &'a DB,
        address: Address,
        index: u64,
    ) -> AccountInfo {
        match self.account_info.get(index, &address) {
            Some(a) => a.clone(),
            None => {
                let base = db.basic_ref(address).unwrap();
                self.account_info
                    .insert(0, address, base.clone().unwrap_or_default());
                base.unwrap_or_default()
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheAccountInfo {
    /// Account information and storage, if account exists.
    pub account: Option<AccountInfo>,
    /// Account status flags.
    pub status: AccountStatus,
}

#[derive(Clone, Debug)]
pub struct TemporalDbFactory<'a, DB: DatabaseRef> {
    pub db: &'a DB,
    pub cache: TemporalState,
}

impl<'a, DB: DatabaseRef> TemporalDbFactory<'a, DB> {
    /// Build a new TemporalDbFactory from a FlashblockAccessList
    ///
    /// This will prepopulate the cache with the changes from the access list
    /// so that TemporalDb instances can be created for specific indices.
    pub fn new(db: &'a DB, list: FlashblockAccessList) -> Self {
        let mut cache = TemporalState::default();
        for change in list.changes {
            for storage_change in change.storage_changes {
                for slot in storage_change.changes {
                    let storage_entry = cache.account_storage.entry(change.address).or_default();
                    storage_entry.insert(
                        slot.block_access_index,
                        storage_change.slot.into(),
                        slot.new_value.into(),
                    );
                }
            }
            // TODO: We can prewarm these
            // for storage_reads in change.storage_reads {}
            for balance_change in change.balance_changes {
                let mut account =
                    cache.init_or_load(db, change.address, balance_change.block_access_index);
                account.balance = balance_change.post_balance;
                cache.account_info.insert(
                    balance_change.block_access_index + 1,
                    change.address,
                    account,
                );
            }
            for nonce_change in change.nonce_changes {
                let mut account =
                    cache.init_or_load(db, change.address, nonce_change.block_access_index);
                account.nonce = nonce_change.new_nonce;
                cache.account_info.insert(
                    nonce_change.block_access_index + 1,
                    change.address,
                    account,
                );
            }
            for code_change in change.code_changes {
                let mut account =
                    cache.init_or_load(db, change.address, code_change.block_access_index);
                let bytecode = Bytecode::new_raw(code_change.new_code);
                account.code_hash = bytecode.hash_slow();
                account.code = Some(bytecode);
                cache.account_info.insert(
                    code_change.block_access_index + 1,
                    change.address,
                    account,
                );
            }
        }

        TemporalDbFactory { db, cache }
    }

    pub fn db(&'a self, index: u64) -> TemporalDb<'a, DB> {
        TemporalDb::new(self.db, &self.cache, index)
    }
}

#[derive(Clone, Debug)]
pub struct TemporalDb<'a, DB: DatabaseRef> {
    pub db: &'a DB,
    pub cache: &'a TemporalState,
    pub index: u64,
}

impl<'a, DB: DatabaseRef> TemporalDb<'a, DB> {
    pub fn new(db: &'a DB, cache: &'a TemporalState, index: u64) -> Self {
        TemporalDb { db, cache, index }
    }
}

impl<'a, DB: DatabaseRef> DatabaseRef for TemporalDb<'a, DB> {
    type Error = DB::Error;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        match self.cache.account_info.get(self.index, &address) {
            Some(acc) => Ok(Some(acc.clone())),
            None => self.db.basic_ref(address),
        }
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        match self.cache.contracts.get(self.index, &code_hash) {
            Some(entry) => Ok(entry.clone()),
            None => self.db.code_by_hash_ref(code_hash),
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
            None => self.db.storage_ref(address, index),
        }
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.db.block_hash_ref(number)
    }
}
