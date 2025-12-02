use std::thread::JoinHandle;

use alloy_primitives::{Address, B256};
use crossbeam_channel::{Receiver, Sender};
use rayon::iter::{ParallelBridge, ParallelIterator};
use revm::{
    Database, DatabaseCommit, DatabaseRef,
    primitives::{HashMap, StorageKey, StorageValue},
    state::{AccountInfo, Bytecode},
};

use crate::access_list::FlashblockAccessListConstruction;

/// A wrapper around a database that builds a Flashblock
/// Access List during execution.
///
/// Commiting to this DB will both construct the underlying access list
/// and commit to the inner database. Between transactions, call
/// [`BalBuilderDb::set_index`] with the block-local transaction index so changes can be
/// attributed correctly.
#[derive(Debug)]
pub struct BalBuilderDb<DB> {
    /// The underlying read/write database.
    db: DB,
    /// The sender to the builder thread.
    tx: Sender<BalBuilderMsg>,
    // /// The Flashblock Access List under construction.
    // access_list: FlashblockAccessListConstruction,
    // /// The current transaction index within the overall block.
    // index: u16,
    /// Join hande for the builder thread
    handle: JoinHandle<FlashblockAccessListConstruction>,
}

#[derive(Clone, Debug)]
struct BalBuilder<DB: DatabaseRef> {
    /// Underlying cached database.
    db: DB,
    /// The Flashblock Access List under construction.
    access_list: FlashblockAccessListConstruction,
    /// The current index being built
    index: u16,
}

/// Messages sent to the background builder for recording reads/writes.
enum BalBuilderMsg {
    StorageRead(Address, StorageKey),
    Commit(HashMap<Address, revm::state::Account>),
    SetIndex(u16),
}

impl<DB> BalBuilderDb<DB> {
    /// Creates a new builder around a writable DB plus a dummy mirror that
    /// the background thread uses to compare state when deriving changes. The dummy will
    /// be commited to so the caller should likely wrap in a caching layer.
    pub fn new<DDB: DatabaseRef + DatabaseCommit + Send + Sync + 'static>(
        db: DB,
        dummy_db: DDB,
    ) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<BalBuilderMsg>();
        let handle = BalBuilder::spawn(dummy_db, rx);

        Self { db, tx, handle }
    }

    /// Updates the current transaction index used to tag future changes.
    pub fn set_index(&mut self, index: u16) {
        let _ = self.tx.send(BalBuilderMsg::SetIndex(index));
    }

    /// Signals the background thread to finish and returns the constructed
    /// access list.
    pub fn finish(self) -> FlashblockAccessListConstruction {
        drop(self.tx);
        self.handle.join().unwrap()
    }
}

impl<DB: DatabaseRef + DatabaseCommit + Send + Sync + 'static> BalBuilder<DB> {
    /// Spawns a background thread that receives read/write events and builds
    /// the access list. `db` should probably have a chaching layer for performance reasons
    pub fn spawn(
        db: DB,
        rx: Receiver<BalBuilderMsg>,
    ) -> JoinHandle<FlashblockAccessListConstruction> {
        std::thread::spawn(move || {
            let mut bal_builder = BalBuilder {
                db,
                access_list: Default::default(),
                index: 0,
            };

            while let Ok(msg) = rx.recv() {
                match msg {
                    BalBuilderMsg::StorageRead(address, index) => {
                        bal_builder.storage_read(address, index);
                    }
                    BalBuilderMsg::Commit(changes) => {
                        bal_builder.commit(changes);
                    }
                    BalBuilderMsg::SetIndex(index) => {
                        bal_builder.index = index;
                    }
                }
            }

            bal_builder.access_list
        })
    }

    /// Records a storage read for the given address and slot.
    fn storage_read(&mut self, address: Address, index: StorageKey) {
        let mut account = self.access_list.changes.entry(address).or_default();
        account.storage_reads.insert(index);
    }

    /// Applies account/storage changes, comparing against the dummy DB to
    /// capture only new values in the access list.
    fn commit(&mut self, changes: HashMap<Address, revm::state::Account>) {
        // When we commit new account state we must first load the previous account state. Only
        // what's changed should be published to the access list.
        changes.iter().par_bridge().for_each(|(address, account)| {
            let mut acc_changes = self.access_list.changes.entry(*address).or_default();

            // There is an edge case here where we could change an account, then change it
            // back. This would result in appending a value to the access list that isn't strctly
            // required. For this reason we should dedup consecutive identical entries when
            // finalizing the access list.
            match self.db.basic_ref(*address).unwrap() {
                Some(previous) => {
                    if previous.balance != account.info.balance {
                        acc_changes
                            .balance_changes
                            .insert(self.index, account.info.balance);
                    }
                    if previous.nonce != account.info.nonce {
                        acc_changes
                            .nonce_changes
                            .insert(self.index, account.info.nonce);
                    }
                    if previous.code_hash != account.info.code_hash {
                        let bytecode = account.info.code.clone().unwrap_or_else(|| {
                            self.db.code_by_hash_ref(account.info.code_hash).unwrap()
                        });
                        acc_changes.code_changes.insert(self.index, bytecode);
                    }
                }
                None => {
                    acc_changes
                        .balance_changes
                        .insert(self.index, account.info.balance);
                    acc_changes
                        .nonce_changes
                        .insert(self.index, account.info.nonce);
                    let bytecode = account.info.code.clone().unwrap_or_else(|| {
                        self.db.code_by_hash_ref(account.info.code_hash).unwrap()
                    });
                    acc_changes.code_changes.insert(self.index, bytecode);
                }
            }

            account.storage.iter().for_each(|(key, value)| {
                let previous_value = self.db.storage_ref(*address, *key).unwrap();
                if previous_value != value.present_value {
                    acc_changes
                        .storage_changes
                        .entry(*key)
                        .or_default()
                        .insert(self.index, value.present_value);
                }
            });
        });

        self.db.commit(changes)
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
        self.tx
            .send(BalBuilderMsg::StorageRead(address, index))
            .expect("BalBuilder thread has terminated unexpectedly");
        self.db.storage(address, index)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.db.block_hash(number)
    }
}

impl<DB: DatabaseCommit + DatabaseRef> DatabaseCommit for BalBuilderDb<DB> {
    fn commit(&mut self, changes: HashMap<Address, revm::state::Account>) {
        self.tx
            .send(BalBuilderMsg::Commit(changes.clone()))
            .expect("BalBuilder thread has terminated unexpectedly");
        self.db.commit(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{U256, address, b256, uint};
    use revm::{
        Database, DatabaseCommit,
        database::InMemoryDB,
        primitives::{HashMap, KECCAK_EMPTY},
        state::{Account, AccountInfo, AccountStatus, Bytecode, EvmStorageSlot},
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

    fn bal_db_with_mirror(db: InMemoryDB) -> BalBuilderDb<InMemoryDB> {
        let read_db = db.clone();
        BalBuilderDb::new(db, read_db)
    }

    #[test]
    fn test_new_bal_builder_db() {
        let db = InMemoryDB::default();
        let bal_db = bal_db_with_mirror(db);

        let access_list = bal_db.finish();
        assert!(access_list.changes.is_empty());
    }

    #[test]
    fn test_set_index() {
        let db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let mut bal_db = bal_db_with_mirror(db);

        bal_db.set_index(10);
        let mut changes = HashMap::default();
        changes.insert(
            addr,
            Account {
                info: create_account(uint!(1_U256), 0, None),
                status: AccountStatus::Touched,
                storage: Default::default(),
                transaction_id: 0,
            },
        );
        bal_db.commit(changes);

        bal_db.set_index(20);
        let mut changes = HashMap::default();
        changes.insert(
            addr,
            Account {
                info: create_account(uint!(2_U256), 0, None),
                status: AccountStatus::Touched,
                storage: Default::default(),
                transaction_id: 1,
            },
        );
        bal_db.commit(changes);

        let access_list = bal_db.finish();
        let acc_changes = access_list.changes.get(&addr).unwrap();
        assert_eq!(acc_changes.balance_changes.get(&10), Some(&uint!(1_U256)));
        assert_eq!(acc_changes.balance_changes.get(&20), Some(&uint!(2_U256)));
    }

    #[test]
    fn test_basic_nonexistent_account() {
        let db = InMemoryDB::default();
        let mut bal_db = bal_db_with_mirror(db);
        let addr = address!("0000000000000000000000000000000000000001");

        let result = bal_db.basic(addr).unwrap();
        assert_eq!(result, None);

        // Access list should not have entry for nonexistent account
        let access_list = bal_db.finish();
        assert!(access_list.changes.get(&addr).is_none());
    }

    #[test]
    fn test_storage_records_reads() {
        let mut db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let slot = U256::from(1);
        let value = U256::from(42);

        // Set up initial storage
        db.insert_account_storage(addr, slot, value).unwrap();

        let mut bal_db = bal_db_with_mirror(db);
        let result = bal_db.storage(addr, slot).unwrap();

        assert_eq!(result, value);

        // Verify storage read was recorded
        let access_list = bal_db.finish();
        let acc_changes = access_list.changes.get(&addr).unwrap();
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

        let mut bal_db = bal_db_with_mirror(db);
        bal_db.storage(addr, slot1).unwrap();
        bal_db.storage(addr, slot2).unwrap();

        // Verify both reads were recorded
        let access_list = bal_db.finish();
        let acc_changes = access_list.changes.get(&addr).unwrap();
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

        let mut bal_db = bal_db_with_mirror(db);
        bal_db.set_index(0);

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

        // Verify the change was recorded at the current index
        let access_list = bal_db.finish();
        let acc_changes = access_list.changes.get(&addr).unwrap();
        assert_eq!(acc_changes.balance_changes.get(&0), Some(&uint!(2000_U256)));
    }

    #[test]
    fn test_commit_nonce_change() {
        let mut db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let initial_account = create_account(uint!(1000_U256), 5, None);

        db.insert_account_info(addr, initial_account.clone());

        let mut bal_db = bal_db_with_mirror(db);
        bal_db.set_index(0);

        let mut changes = HashMap::default();
        let new_account = Account {
            info: create_account(uint!(1000_U256), 6, None),
            status: AccountStatus::Touched,
            storage: Default::default(),
            transaction_id: 0,
        };
        changes.insert(addr, new_account);

        bal_db.commit(changes);

        let access_list = bal_db.finish();
        let acc_changes = access_list.changes.get(&addr).unwrap();
        assert_eq!(acc_changes.nonce_changes.get(&0), Some(&6));
    }

    #[test]
    fn test_commit_code_change() {
        let mut db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let initial_account = create_account(uint!(1000_U256), 5, None);

        db.insert_account_info(addr, initial_account.clone());

        let mut bal_db = bal_db_with_mirror(db);
        bal_db.set_index(0);

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

        let access_list = bal_db.finish();
        let acc_changes = access_list.changes.get(&addr).unwrap();
        assert_eq!(acc_changes.code_changes.get(&0), Some(&new_bytecode));
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

        let mut bal_db = bal_db_with_mirror(db);
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

        let access_list = bal_db.finish();
        let acc_changes = access_list.changes.get(&addr).unwrap();
        let slot_changes = acc_changes.storage_changes.get(&slot).unwrap();
        assert_eq!(slot_changes.get(&0), Some(&new_value));
    }

    #[test]
    fn test_commit_no_change_not_recorded() {
        let mut db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let initial_account = create_account(uint!(1000_U256), 5, None);

        db.insert_account_info(addr, initial_account.clone());

        let mut bal_db = bal_db_with_mirror(db);
        bal_db.set_index(0);

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

        let access_list = bal_db.finish();
        let acc_changes = access_list.changes.get(&addr).unwrap();
        assert!(acc_changes.is_empty());
    }

    #[test]
    fn test_commit_new_account() {
        let db = InMemoryDB::default();
        let mut bal_db = bal_db_with_mirror(db);
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

        let access_list = bal_db.finish();
        let acc_changes = access_list.changes.get(&addr).unwrap();
        // New account should record all values at the current index
        assert_eq!(acc_changes.balance_changes.get(&0), Some(&uint!(1000_U256)));
        assert_eq!(acc_changes.nonce_changes.get(&0), Some(&1));
        assert_eq!(acc_changes.code_changes.get(&0), Some(&bytecode));
    }

    #[test]
    fn test_multiple_transactions() {
        let mut db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let initial_account = create_account(uint!(1000_U256), 5, None);

        db.insert_account_info(addr, initial_account.clone());

        let mut bal_db = bal_db_with_mirror(db);

        // Transaction 0
        bal_db.set_index(0);

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

        let access_list = bal_db.finish();
        let acc_changes = access_list.changes.get(&addr).unwrap();
        // Should have initial state and two changes
        assert_eq!(acc_changes.balance_changes.get(&0), Some(&uint!(900_U256)));
        assert_eq!(acc_changes.balance_changes.get(&1), Some(&uint!(800_U256)));
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

        let mut bal_db = bal_db_with_mirror(db);
        let result = bal_db.code_by_hash(code_hash).unwrap();

        assert_eq!(result, bytecode);

        let access_list = bal_db.finish();
        assert!(access_list.changes.is_empty());
    }

    #[test]
    fn test_block_hash() {
        let db = InMemoryDB::default();
        let mut bal_db = bal_db_with_mirror(db);
        let block_num = 0u64;

        // InMemoryDB computes block hash from block number
        let result = bal_db.block_hash(block_num).unwrap();
        // Just verify that we can get a block hash without error
        // The actual value is computed by InMemoryDB's implementation
        assert_ne!(
            result,
            b256!("0000000000000000000000000000000000000000000000000000000000000000")
        );

        let access_list = bal_db.finish();
        assert!(access_list.changes.is_empty());
    }
}
