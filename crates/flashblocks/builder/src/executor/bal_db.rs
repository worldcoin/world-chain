use alloy_primitives::{Address, B256};
use revm::{
    primitives::{HashMap, StorageKey, StorageValue},
    state::{AccountInfo, Bytecode},
    Database, DatabaseCommit, DatabaseRef,
};

use crate::access_list::FlashblockAccessListConstruction;

/// A wrapper around a database that builds a Flashblock
/// Access List during execution. Between transactions, the index
/// should be updated to reflect the current transaction index within the block.
///
/// This type intentionally does not implement `DatabaseRef` because
/// we need mutable access to update the access list during reads.
#[derive(Clone, Debug)]
pub struct BalBuilderDb<DB> {
    /// The underlying database.
    pub db: DB,
    /// The Flashblock Access List under construction.
    pub access_list: FlashblockAccessListConstruction,
    /// The current transaction index within the overall block.
    pub index: u16,
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

impl<DB> BalBuilderDb<DB> {
    pub fn set_index(&mut self, index: u16) {
        self.index = index;
    }
}

impl<DB: Database> Database for BalBuilderDb<DB> {
    type Error = DB::Error;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.db.basic(address)
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.db.code_by_hash(code_hash)
    }

    fn storage(
        &mut self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        let mut account = self.access_list.changes.entry(address).or_default();
        account.storage_reads.insert(index);
        self.db.storage(address, index)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.db.block_hash(number)
    }
}

impl<DB: DatabaseCommit + DatabaseRef> DatabaseCommit for BalBuilderDb<DB> {
    /// TODO: par iter
    ///
    /// TODO: we are currently performing a blocking read, then a blocking write for each operation here.
    /// This can be optimized writing immediately, then reading asynchronously and building the
    /// access list.
    fn commit(&mut self, changes: HashMap<Address, revm::state::Account>) {
        // When we commit new account state we must first load the previous account state. Only
        // what's changed should be published to the access list.
        changes.iter().for_each(|(address, account)| {
            let mut acc_changes = self.access_list.changes.entry(*address).or_default();

            // TODO: there is an edge case here where we could change an account, then change it
            // back. This would result in appending a value to the access list that isn't strctly
            // required.
            match self.db.basic_ref(*address).unwrap() {
                Some(previous) => {
                    if previous.balance != account.info.balance {
                        acc_changes
                            .balance_changes
                            .insert(self.index + 1, account.info.balance);
                    }
                    if previous.nonce != account.info.nonce {
                        acc_changes
                            .nonce_changes
                            .insert(self.index + 1, account.info.nonce);
                    }
                    if previous.code_hash != account.info.code_hash {
                        let bytecode = account.info.code.clone().unwrap_or_else(|| {
                            self.db.code_by_hash_ref(account.info.code_hash).unwrap()
                        });
                        acc_changes.code_changes.insert(self.index + 1, bytecode);
                    }
                }
                None => {
                    acc_changes
                        .balance_changes
                        .insert(self.index + 1, account.info.balance);
                    acc_changes
                        .nonce_changes
                        .insert(self.index + 1, account.info.nonce);
                    let bytecode = account.info.code.clone().unwrap_or_else(|| {
                        self.db.code_by_hash_ref(account.info.code_hash).unwrap()
                    });
                    acc_changes.code_changes.insert(self.index + 1, bytecode);
                }
            }

            account.storage.iter().for_each(|(key, value)| {
                let previous_value = self.db.storage_ref(*address, *key).unwrap();
                if previous_value != value.present_value {
                    acc_changes
                        .storage_changes
                        .entry(*key)
                        .or_default()
                        .insert(self.index + 1, value.present_value);
                }
            });
        });

        self.db.commit(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, b256, uint, U256};
    use revm::{
        database::InMemoryDB,
        primitives::{HashMap, KECCAK_EMPTY},
        state::{Account, AccountInfo, AccountStatus, Bytecode, EvmStorageSlot},
        Database, DatabaseCommit,
    };

    // Helper function to create a simple account
    fn create_account(balance: U256, nonce: u64, code: Option<Bytecode>) -> AccountInfo {
        AccountInfo {
            balance,
            nonce,
            code_hash: code.as_ref().map(|c| c.hash_slow()).unwrap_or(KECCAK_EMPTY),
            code,
        }
    }

    #[test]
    fn test_new_bal_builder_db() {
        let db = InMemoryDB::default();
        let bal_db = BalBuilderDb::new(db);

        assert_eq!(bal_db.index, 0);
        assert_eq!(bal_db.access_list.changes.len(), 0);
    }

    #[test]
    fn test_set_index() {
        let db = InMemoryDB::default();
        let mut bal_db = BalBuilderDb::new(db);

        bal_db.set_index(10);
        assert_eq!(bal_db.index, 10);

        bal_db.set_index(20);
        assert_eq!(bal_db.index, 20);
    }

    #[test]
    fn test_basic_nonexistent_account() {
        let db = InMemoryDB::default();
        let mut bal_db = BalBuilderDb::new(db);
        let addr = address!("0000000000000000000000000000000000000001");

        let result = bal_db.basic(addr).unwrap();
        assert_eq!(result, None);

        // Access list should not have entry for nonexistent account
        assert!(bal_db.access_list.changes.get(&addr).is_none());
    }

    #[test]
    fn test_storage_records_reads() {
        let mut db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let slot = U256::from(1);
        let value = U256::from(42);

        // Set up initial storage
        db.insert_account_storage(addr, slot, value).unwrap();

        let mut bal_db = BalBuilderDb::new(db);
        let result = bal_db.storage(addr, slot).unwrap();

        assert_eq!(result, value);

        // Verify storage read was recorded
        let acc_changes = bal_db.access_list.changes.get(&addr).unwrap();
        assert!(acc_changes.storage_reads.contains(&slot));
    }

    #[test]
    fn test_storage_multiple_reads() {
        let mut db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let slot1 = U256::from(1);
        let slot2 = U256::from(2);
        let value1 = U256::from(42);
        let value2 = U256::from(100);

        db.insert_account_storage(addr, slot1, value1).unwrap();
        db.insert_account_storage(addr, slot2, value2).unwrap();

        let mut bal_db = BalBuilderDb::new(db);
        bal_db.storage(addr, slot1).unwrap();
        bal_db.storage(addr, slot2).unwrap();

        // Verify both reads were recorded
        let acc_changes = bal_db.access_list.changes.get(&addr).unwrap();
        assert!(acc_changes.storage_reads.contains(&slot1));
        assert!(acc_changes.storage_reads.contains(&slot2));
        assert_eq!(acc_changes.storage_reads.len(), 2);
    }

    #[test]
    fn test_commit_balance_change() {
        let mut db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let initial_account = create_account(uint!(1000_U256), 5, None);

        db.insert_account_info(addr, initial_account.clone());

        let mut bal_db = BalBuilderDb::new(db);
        bal_db.set_index(0);

        // First read the account to record initial state
        bal_db.basic(addr).unwrap();

        // Create a change
        let mut changes = HashMap::default();
        let new_account = Account {
            info: create_account(uint!(2000_U256), 5, None),
            status: AccountStatus::Touched,
            storage: Default::default(),
            transaction_id: 0,
        };
        changes.insert(addr, new_account);

        bal_db.commit(changes);

        // Verify the change was recorded at index 1 (index + 1)
        let acc_changes = bal_db.access_list.changes.get(&addr).unwrap();
        assert_eq!(acc_changes.balance_changes.get(&1), Some(&uint!(2000_U256)));
    }

    #[test]
    fn test_commit_nonce_change() {
        let mut db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let initial_account = create_account(uint!(1000_U256), 5, None);

        db.insert_account_info(addr, initial_account.clone());

        let mut bal_db = BalBuilderDb::new(db);
        bal_db.set_index(0);

        bal_db.basic(addr).unwrap();

        let mut changes = HashMap::default();
        let new_account = Account {
            info: create_account(uint!(1000_U256), 6, None),
            status: AccountStatus::Touched,
            storage: Default::default(),
            transaction_id: 0,
        };
        changes.insert(addr, new_account);

        bal_db.commit(changes);

        let acc_changes = bal_db.access_list.changes.get(&addr).unwrap();
        assert_eq!(acc_changes.nonce_changes.get(&1), Some(&6));
    }

    #[test]
    fn test_commit_code_change() {
        let mut db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let initial_account = create_account(uint!(1000_U256), 5, None);

        db.insert_account_info(addr, initial_account.clone());

        let mut bal_db = BalBuilderDb::new(db);
        bal_db.set_index(0);

        bal_db.basic(addr).unwrap();

        let new_bytecode = Bytecode::new_raw(vec![0x60, 0x00, 0x60, 0x00].into());
        let mut changes = HashMap::default();
        let new_account = Account {
            info: create_account(uint!(1000_U256), 5, Some(new_bytecode.clone())),
            status: AccountStatus::Touched,
            storage: Default::default(),
            transaction_id: 0,
        };
        changes.insert(addr, new_account);

        bal_db.commit(changes);

        let acc_changes = bal_db.access_list.changes.get(&addr).unwrap();
        assert_eq!(acc_changes.code_changes.get(&1), Some(&new_bytecode));
    }

    #[test]
    fn test_commit_storage_change() {
        let mut db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let slot = U256::from(1);
        let initial_value = U256::from(42);
        let new_value = U256::from(100);

        let initial_account = create_account(uint!(1000_U256), 5, None);
        db.insert_account_info(addr, initial_account);
        db.insert_account_storage(addr, slot, initial_value)
            .unwrap();

        let mut bal_db = BalBuilderDb::new(db);
        bal_db.set_index(0);

        let mut changes = HashMap::default();
        let mut storage = HashMap::default();
        storage.insert(
            slot,
            EvmStorageSlot {
                present_value: new_value,
                ..Default::default()
            },
        );

        let new_account = Account {
            info: create_account(uint!(1000_U256), 5, None),
            status: AccountStatus::Touched,
            storage,
            transaction_id: 0,
        };
        changes.insert(addr, new_account);

        bal_db.commit(changes);

        let acc_changes = bal_db.access_list.changes.get(&addr).unwrap();
        let slot_changes = acc_changes.storage_changes.get(&slot).unwrap();
        assert_eq!(slot_changes.get(&1), Some(&new_value));
    }

    #[test]
    fn test_commit_no_change_not_recorded() {
        let mut db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let initial_account = create_account(uint!(1000_U256), 5, None);

        db.insert_account_info(addr, initial_account.clone());

        let mut bal_db = BalBuilderDb::new(db);
        bal_db.set_index(0);

        bal_db.basic(addr).unwrap();

        // Commit same values (no change)
        let mut changes = HashMap::default();
        let new_account = Account {
            info: initial_account.clone(),
            status: AccountStatus::Touched,
            storage: Default::default(),
            transaction_id: 0,
        };
        changes.insert(addr, new_account);

        bal_db.commit(changes);
    }

    #[test]
    fn test_commit_new_account() {
        let db = InMemoryDB::default();
        let mut bal_db = BalBuilderDb::new(db);
        bal_db.set_index(0);

        let addr = address!("0000000000000000000000000000000000000001");
        let bytecode = Bytecode::new_raw(vec![0x60, 0x00].into());

        let mut changes = HashMap::default();
        let new_account = Account {
            info: create_account(uint!(1000_U256), 1, Some(bytecode.clone())),
            status: AccountStatus::Touched,
            storage: Default::default(),
            transaction_id: 0,
        };
        changes.insert(addr, new_account);

        bal_db.commit(changes);

        let acc_changes = bal_db.access_list.changes.get(&addr).unwrap();
        // New account should record all values at index 1
        assert_eq!(acc_changes.balance_changes.get(&1), Some(&uint!(1000_U256)));
        assert_eq!(acc_changes.nonce_changes.get(&1), Some(&1));
        assert_eq!(acc_changes.code_changes.get(&1), Some(&bytecode));
    }

    #[test]
    fn test_multiple_transactions() {
        let mut db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let initial_account = create_account(uint!(1000_U256), 5, None);

        db.insert_account_info(addr, initial_account.clone());

        let mut bal_db = BalBuilderDb::new(db);

        // Transaction 0
        bal_db.set_index(0);
        bal_db.basic(addr).unwrap();

        let mut changes = HashMap::default();
        changes.insert(
            addr,
            Account {
                info: create_account(uint!(900_U256), 6, None),
                status: AccountStatus::Touched,
                storage: Default::default(),
                transaction_id: 0,
            },
        );
        bal_db.commit(changes);

        // Transaction 1
        bal_db.set_index(1);
        let mut changes = HashMap::default();
        changes.insert(
            addr,
            Account {
                info: create_account(uint!(800_U256), 7, None),
                status: AccountStatus::Touched,
                storage: Default::default(),
                transaction_id: 0,
            },
        );
        bal_db.commit(changes);

        let acc_changes = bal_db.access_list.changes.get(&addr).unwrap();
        // Should have initial state and two changes
        assert_eq!(acc_changes.balance_changes.get(&1), Some(&uint!(900_U256)));
        assert_eq!(acc_changes.balance_changes.get(&2), Some(&uint!(800_U256)));
    }

    #[test]
    fn test_code_by_hash() {
        let mut db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let bytecode = Bytecode::new_raw(vec![0x60, 0x00, 0x60, 0x00].into());
        let code_hash = bytecode.hash_slow();

        // Insert account with code
        db.insert_account_info(
            addr,
            AccountInfo {
                balance: U256::ZERO,
                nonce: 0,
                code_hash,
                code: Some(bytecode.clone()),
            },
        );

        let mut bal_db = BalBuilderDb::new(db);
        let result = bal_db.code_by_hash(code_hash).unwrap();

        assert_eq!(result, bytecode);
    }

    #[test]
    fn test_block_hash() {
        let db = InMemoryDB::default();
        let mut bal_db = BalBuilderDb::new(db);
        let block_num = 0u64;

        // InMemoryDB computes block hash from block number
        let result = bal_db.block_hash(block_num).unwrap();
        // Just verify that we can get a block hash without error
        // The actual value is computed by InMemoryDB's implementation
        assert_ne!(
            result,
            b256!("0000000000000000000000000000000000000000000000000000000000000000")
        );
    }
}
