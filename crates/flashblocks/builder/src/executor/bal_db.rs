use alloy_primitives::{Address, B256};
use flashblocks_primitives::access_list::FlashblockAccessList;
use revm::{
    primitives::{HashMap, StorageKey, StorageValue},
    state::{AccountInfo, Bytecode},
    DatabaseCommit, DatabaseRef,
};

use crate::access_list::FlashblockAccessListConstruction;

#[derive(Clone, Debug)]
pub struct BalBuilderDb<DB> {
    pub db: DB,
    pub access_list: FlashblockAccessListConstruction,
    pub index: u64,
}

impl<DB> BalBuilderDb<DB> {
    pub fn new(db: DB) -> Self {
        Self {
            db,
            access_list: Default::default(),
            index: 0,
        }
    }
}

impl<DB: DatabaseRef> DatabaseRef for BalBuilderDb<DB> {
    type Error = DB::Error;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.db.basic_ref(address)
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.db.code_by_hash_ref(code_hash)
    }

    fn storage_ref(
        &self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        let mut account = self.access_list.changes.entry(address).or_default();
        account.storage_reads.insert(index);
        self.db.storage_ref(address, index)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.db.block_hash_ref(number)
    }
}
