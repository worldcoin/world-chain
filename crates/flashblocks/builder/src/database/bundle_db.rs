
use alloy_primitives::{Address, B256};
use revm::{
    DatabaseRef,
    database::BundleState,
    primitives::{StorageKey, StorageValue},
    state::{AccountInfo, Bytecode},
};

/// A layered database for executing a bundle over an underlying database.
#[derive(Clone, Debug)]
pub struct BundleDb<DB: DatabaseRef> {
    /// Layer 1: Underlying [`BundleState`] from prior flashblocks _or_ the pre-execution changes.
    pub bundle: BundleState,
    /// Layer 2: The underlying database
    pub db: DB,
}

impl<DB: DatabaseRef> BundleDb<DB> {
    /// Creates a new [`BundleDb`] from the given bundle state and underlying database.
    pub fn new(db: DB, bundle: BundleState) -> Self {
        Self { bundle, db }
    }
}

impl<DB: DatabaseRef> DatabaseRef for BundleDb<DB> {
    type Error = DB::Error;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        if let Some(account) = self.bundle.account(&address) {
            let present_info = account.account_info();
            if let Some(info) = present_info {
                return Ok(Some(info));
            }
        };

        self.db.basic_ref(address)
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if let Some(bytecode) = self.bundle.bytecode(&code_hash) {
            return Ok(bytecode);
        }

        self.db.code_by_hash_ref(code_hash)
    }

    fn storage_ref(
        &self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        if let Some(account) = self.bundle.account(&address)
            && let Some(storage) = account.storage_slot(index)
        {
            return Ok(storage);
        }

        self.db.storage_ref(address, index)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.db.block_hash_ref(number)
    }
}
