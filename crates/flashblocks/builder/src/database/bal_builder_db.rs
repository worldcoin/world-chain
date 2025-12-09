use std::thread::JoinHandle;

use alloy_primitives::{Address, B256};
use crossbeam_channel::Sender;
use reth_evm::block::StateDB;
use revm::{
    Database, DatabaseCommit, DatabaseRef,
    database::{BundleState, states::bundle_state::BundleRetention},
    primitives::{HashMap, StorageKey, StorageValue},
    state::{Account, AccountInfo, Bytecode},
};
use tracing::error;

use crate::access_list::FlashblockAccessListConstruction;

/// Messages sent to the background builder for recording reads/writes.
enum BalBuilderMsg {
    StorageRead(Address, StorageKey),
    Commit(HashMap<Address, revm::state::Account>),
    SetIndex(u16),
    MergeAcccessList(FlashblockAccessListConstruction),
}

/// A wrapper around a database that builds a Flashblock
/// Access List during execution.
///
/// Commiting to this DB will both construct the underlying access list
/// and commit to the inner database. Between transactions, call
/// [`BalBuilderDb::set_index`] with the block-local transaction index so changes can be
/// attributed correctly.
#[derive(Debug)]
pub struct BalBuilderDb<DB>
where
    DB: DatabaseCommit + Database,
{
    /// Underlying cached database.
    db: DB,
    /// The Flashblock Access List under construction.
    access_list: FlashblockAccessListConstruction,
    /// The current index being built
    index: u16,
    /// The most recent error generated. We store this error in order to be compliant
    /// with [`DatabaseCommit`], and return it on [`BalBuilderDb::finish`]
    error: Option<<Self as Database>::Error>,
}

impl<DB> BalBuilderDb<DB>
where
    DB: DatabaseCommit + Database,
{
    /// Creates a new BalBuilderDb around the given database.
    pub fn new(db: DB) -> Self {
        Self {
            db,
            access_list: Default::default(),
            index: 0,
            error: None,
        }
    }

    /// Returns a reference to the underlying database.
    pub fn db(&self) -> &DB {
        &self.db
    }

    /// Returns a mutable reference to the underlying database.
    pub fn db_mut(&mut self) -> &mut DB {
        &mut self.db
    }

    /// Updates the current transaction index used to tag future changes.
    pub fn set_index(&mut self, index: u16) {
        self.index = index;
    }

    /// Merges the access lists
    pub fn merge_access_list(&mut self, access_list: FlashblockAccessListConstruction) {
        self.access_list.merge(access_list);
    }

    /// Records a storage read for the given address and slot.
    fn handle_storage_read(&mut self, address: Address, index: StorageKey) {
        let mut account = self.access_list.changes.entry(address).or_default();
        account.storage_reads.insert(index);
    }

    /// Applies account/storage changes, comparing against the DB to
    /// capture only new values in the access list.
    fn try_commit(
        &mut self,
        changes: HashMap<Address, revm::state::Account>,
    ) -> Result<(), <DB as Database>::Error> {
        // When we commit new account state we must first load the previous account state. Only
        // what's changed should be published to the access list.
        changes
            .iter()
            // Pre-load all accounts into the cache using the mutable `basic` method.
            // This is required because `State::commit` expects all accounts to be present
            // in the cache (it panics with "All accounts should be present inside cache" otherwise).
            // The `DatabaseRef::basic_ref` method does NOT populate the cache, only `Database::basic` does.
            // .par_bridge()
            .try_for_each(|(address, account)| {
                let mut acc_changes = self.access_list.changes.entry(*address).or_default();

                // There is an edge case here where we could change an account, then change it
                // back. This would result in appending a value to the access list that isn't strctly
                // required. For this reason we should dedup consecutive identical entries when
                // finalizing the access list.
                match self.db.basic(*address)? {
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
                            let bytecode = match account.info.code.clone() {
                                Some(code) => code,
                                None => self.db.code_by_hash(account.info.code_hash)?,
                            };
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
                        let bytecode = match account.info.code.clone() {
                            Some(code) => code,
                            None => self.db.code_by_hash(account.info.code_hash)?,
                        };
                        acc_changes.code_changes.insert(self.index, bytecode);
                    }
                }

                account.storage.iter().try_for_each(|(key, value)| {
                    let previous_value = self.db.storage(*address, *key)?;
                    if previous_value != value.present_value {
                        acc_changes
                            .storage_changes
                            .entry(*key)
                            .or_default()
                            .insert(self.index, value.present_value);
                    }
                    Result::<(), <DB as Database>::Error>::Ok(())
                })?;

                Ok(())
            })?;

        self.db.commit(changes);

        Ok(())
    }

    /// Consumes self and returns the constructed access list.
    pub fn finish(self) -> Result<FlashblockAccessListConstruction, <Self as Database>::Error> {
        if let Some(e) = self.error {
            return Err(e);
        }

        Ok(self.access_list)
    }
}

impl<DB> Database for BalBuilderDb<DB>
where
    DB: DatabaseCommit + Database,
{
    type Error = <DB as Database>::Error;

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
        self.handle_storage_read(address, index);
        self.db.storage(address, index)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.db.block_hash(number)
    }
}

impl<DB: DatabaseCommit> DatabaseCommit for BalBuilderDb<DB>
where
    DB: DatabaseCommit + Database,
{
    fn commit(&mut self, changes: HashMap<Address, revm::state::Account>) {
        if let Err(e) = self.try_commit(changes) {
            error!("Error committing to BalBuilderDb: {:?}", e);
            self.error = Some(e);
        }
    }
}

impl<DB> StateDB for BalBuilderDb<DB>
where
    DB: StateDB + DatabaseCommit + Database,
{
    fn bundle_state(&self) -> &BundleState {
        self.db.bundle_state()
    }

    fn bundle_state_mut(&mut self) -> &mut BundleState {
        self.db.bundle_state_mut()
    }

    fn merge_transitions(&mut self, retention: BundleRetention) {
        self.db.merge_transitions(retention);
    }

    fn set_state_clear_flag(&mut self, has_state_clear: bool) {
        self.db.set_state_clear_flag(has_state_clear);
    }
}

/// An asynchronous Flashblock Access List builder around a database.
///  
/// commiting to this database will both commit to the inner database
/// and update the access list under construction in a background thread.
/// Between transactions, call [`AsyncBalBuilderDb::set_index`] with the block-local
/// transaction index so changes can be attributed correctly.
#[derive(Debug)]
pub struct AsyncBalBuilderDb<DB: Database> {
    /// The underlying read/write database.
    db: DB,
    /// The sender to the builder thread.
    tx: Sender<BalBuilderMsg>,
    /// Join hande for the builder thread
    handle: JoinHandle<Result<FlashblockAccessListConstruction, <DB as Database>::Error>>,
}

impl<DB: Database> AsyncBalBuilderDb<DB> {
    /// Creates a new builder around a writable DB plus a dummy mirror that
    /// the background thread uses to compare state when deriving changes. The dummy will
    /// be commited to so the caller should likely wrap in a caching layer.
    pub fn new<DDB>(db: DB, dummy_db: DDB) -> Self
    where
        DB: Database<Error: From<<DDB as Database>::Error>>,
        DDB: DatabaseCommit + Database + Send + Sync + 'static,
    {
        let (tx, rx) = crossbeam_channel::unbounded::<BalBuilderMsg>();
        let mut bal_builder = BalBuilderDb::new(dummy_db);

        let handle = std::thread::spawn(move || {
            while let Ok(msg) = rx.recv() {
                match msg {
                    BalBuilderMsg::StorageRead(address, index) => {
                        bal_builder.handle_storage_read(address, index);
                    }
                    BalBuilderMsg::Commit(changes) => {
                        bal_builder.try_commit(changes)?;
                    }
                    BalBuilderMsg::SetIndex(index) => {
                        bal_builder.set_index(index);
                    }
                    BalBuilderMsg::MergeAcccessList(access_list) => {
                        bal_builder.merge_access_list(access_list);
                    }
                }
            }

            Ok(bal_builder.finish()?)
        });

        Self { db, tx, handle }
    }

    /// Returns a reference to the underlying database.
    pub fn db(&self) -> &DB {
        &self.db
    }

    /// Returns a mutable reference to the underlying database.
    pub fn db_mut(&mut self) -> &mut DB {
        &mut self.db
    }

    /// Updates the current transaction index used to tag future changes.
    pub fn set_index(&mut self, index: u16) {
        let _ = self.tx.send(BalBuilderMsg::SetIndex(index));
    }

    /// Merges another access list into the current one.
    pub fn merge_access_list(&mut self, access_list: FlashblockAccessListConstruction) {
        let _ = self.tx.send(BalBuilderMsg::MergeAcccessList(access_list));
    }

    /// Signals the background thread to finish and returns the constructed
    /// access list.
    pub fn finish(self) -> Result<FlashblockAccessListConstruction, <DB as Database>::Error> {
        drop(self.tx);
        // unwrap should be safe here since builder thread can't panic
        self.handle.join().unwrap()
    }
}

impl<DB: Database> Database for AsyncBalBuilderDb<DB> {
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
        // Ignore errors from the builder channel.
        // relevent errors will be propagated through `finish()`.
        self.tx
            .send(BalBuilderMsg::StorageRead(address, index))
            .ok();
        self.db.storage(address, index)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.db.block_hash(number)
    }
}

impl<DB: Database + DatabaseCommit> DatabaseCommit for AsyncBalBuilderDb<DB> {
    fn commit(&mut self, changes: HashMap<Address, revm::state::Account>) {
        // Ignore errors from the builder channel.
        // relevent errors will be propagated through `finish()`.
        self.tx.send(BalBuilderMsg::Commit(changes.clone())).ok();
        self.db.commit(changes)
    }
}

impl<DB: StateDB> StateDB for AsyncBalBuilderDb<DB> {
    fn bundle_state(&self) -> &BundleState {
        self.db.bundle_state()
    }

    fn bundle_state_mut(&mut self) -> &mut BundleState {
        self.db.bundle_state_mut()
    }

    fn merge_transitions(&mut self, retention: BundleRetention) {
        self.db.merge_transitions(retention);
    }

    fn set_state_clear_flag(&mut self, has_state_clear: bool) {
        self.db.set_state_clear_flag(has_state_clear);
    }
}

#[derive(Debug)]
pub struct BalValidationState<DB> {
    /// Optional database that we use to fetch data from
    ///
    /// If database is not present, we will return not existing account and storage.
    ///
    /// **Note**: It is marked as Send so database can be shared between threads.
    pub database: DB,
    bundle_state: BundleState,
}

impl<DB> BalValidationState<DB> {
    /// Creates a new BalValidationState with the given database.
    pub fn new(database: DB) -> Self {
        Self {
            database,
            bundle_state: BundleState::default(),
        }
    }
}

impl<DB: Database> StateDB for BalValidationState<DB> {
    fn bundle_state(&self) -> &BundleState {
        &self.bundle_state
    }

    fn bundle_state_mut(&mut self) -> &mut BundleState {
        &mut self.bundle_state
    }

    fn set_state_clear_flag(&mut self, _has_state_clear: bool) {
        // np op
    }

    fn merge_transitions(&mut self, _retention: BundleRetention) {
        // no op
    }
}

impl<DB: Database> Database for BalValidationState<DB> {
    type Error = DB::Error;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.database.basic(address)
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.database.code_by_hash(code_hash)
    }

    fn storage(
        &mut self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        self.database.storage(address, index)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.database.block_hash(number)
    }
}

impl<DB: Database> DatabaseCommit for BalValidationState<DB> {
    fn commit(&mut self, _changes: HashMap<Address, Account>) {}
    fn commit_iter(&mut self, _changes: impl IntoIterator<Item = (Address, Account)>) {
        // np op
    }
}

impl<DB: DatabaseRef> DatabaseRef for BalValidationState<DB> {
    type Error = DB::Error;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.database.basic_ref(address)
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.database.code_by_hash_ref(code_hash)
    }

    fn storage_ref(
        &self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        self.database.storage_ref(address, index)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.database.block_hash_ref(number)
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

    fn bal_db_with_mirror(db: InMemoryDB) -> AsyncBalBuilderDb<InMemoryDB> {
        let read_db = db.clone();
        AsyncBalBuilderDb::new(db, read_db)
    }

    #[test]
    fn test_new_bal_builder_db() -> eyre::Result<()> {
        let db = InMemoryDB::default();
        let bal_db = bal_db_with_mirror(db);

        let access_list = bal_db.finish()?;
        assert!(access_list.changes.is_empty());

        Ok(())
    }

    #[test]
    fn test_set_index() -> eyre::Result<()> {
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

        let access_list = bal_db.finish()?;
        let acc_changes = access_list.changes.get(&addr).unwrap();
        assert_eq!(acc_changes.balance_changes.get(&10), Some(&uint!(1_U256)));
        assert_eq!(acc_changes.balance_changes.get(&20), Some(&uint!(2_U256)));

        Ok(())
    }

    #[test]
    fn test_basic_nonexistent_account() -> eyre::Result<()> {
        let db = InMemoryDB::default();
        let mut bal_db = bal_db_with_mirror(db);
        let addr = address!("0000000000000000000000000000000000000001");

        let result = bal_db.basic(addr).unwrap();
        assert_eq!(result, None);

        // Access list should not have entry for nonexistent account
        let access_list = bal_db.finish()?;
        assert!(access_list.changes.get(&addr).is_none());

        Ok(())
    }

    #[test]
    fn test_storage_records_reads() -> eyre::Result<()> {
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
        let access_list = bal_db.finish()?;
        let acc_changes = access_list.changes.get(&addr).unwrap();
        assert!(acc_changes.storage_reads.contains(&slot));

        Ok(())
    }

    #[test]
    fn test_storage_multiple_reads() -> eyre::Result<()> {
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
        let access_list = bal_db.finish()?;
        let acc_changes = access_list.changes.get(&addr).unwrap();
        assert!(acc_changes.storage_reads.contains(&slot1));
        assert!(acc_changes.storage_reads.contains(&slot2));
        assert_eq!(acc_changes.storage_reads.len(), 2);

        Ok(())
    }

    #[test]
    fn test_commit_balance_change() -> eyre::Result<()> {
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
        let access_list = bal_db.finish()?;
        let acc_changes = access_list.changes.get(&addr).unwrap();
        assert_eq!(acc_changes.balance_changes.get(&0), Some(&uint!(2000_U256)));

        Ok(())
    }

    #[test]
    fn test_commit_nonce_change() -> eyre::Result<()> {
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

        let access_list = bal_db.finish()?;
        let acc_changes = access_list.changes.get(&addr).unwrap();
        assert_eq!(acc_changes.nonce_changes.get(&0), Some(&6));

        Ok(())
    }

    #[test]
    fn test_commit_code_change() -> eyre::Result<()> {
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

        let access_list = bal_db.finish()?;
        let acc_changes = access_list.changes.get(&addr).unwrap();
        assert_eq!(acc_changes.code_changes.get(&0), Some(&new_bytecode));

        Ok(())
    }

    #[test]
    fn test_commit_storage_change() -> eyre::Result<()> {
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

        let access_list = bal_db.finish()?;
        let acc_changes = access_list.changes.get(&addr).unwrap();
        let slot_changes = acc_changes.storage_changes.get(&slot).unwrap();
        assert_eq!(slot_changes.get(&0), Some(&new_value));

        Ok(())
    }

    #[test]
    fn test_commit_no_change_not_recorded() -> eyre::Result<()> {
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

        let access_list = bal_db.finish()?;
        let acc_changes = access_list.changes.get(&addr).unwrap();
        assert!(acc_changes.is_empty());

        Ok(())
    }

    #[test]
    fn test_commit_updates_dummy_database() -> eyre::Result<()> {
        let mut db = InMemoryDB::default();
        let addr = address!("0000000000000000000000000000000000000001");
        let initial_account = create_account(uint!(1000_U256), 0, None);
        db.insert_account_info(addr, initial_account);

        let mut bal_db = bal_db_with_mirror(db);

        bal_db.set_index(0);
        let mut changes = HashMap::default();
        changes.insert(
            addr,
            Account {
                info: create_account(uint!(1500_U256), 0, None),
                status: AccountStatus::Touched,
                storage: Default::default(),
                transaction_id: 0,
            },
        );
        bal_db.commit(changes);

        // If the dummy DB is committed, the following identical commit should not create a new entry.
        bal_db.set_index(1);
        let mut repeat_changes = HashMap::default();
        repeat_changes.insert(
            addr,
            Account {
                info: create_account(uint!(1500_U256), 0, None),
                status: AccountStatus::Touched,
                storage: Default::default(),
                transaction_id: 1,
            },
        );
        bal_db.commit(repeat_changes);

        let access_list = bal_db.finish()?;
        let acc_changes = access_list.changes.get(&addr).unwrap();
        assert_eq!(acc_changes.balance_changes.len(), 1);
        assert_eq!(acc_changes.balance_changes.get(&0), Some(&uint!(1500_U256)));
        assert!(!acc_changes.balance_changes.contains_key(&1));

        Ok(())
    }

    #[test]
    fn test_commit_new_account() -> eyre::Result<()> {
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

        let access_list = bal_db.finish()?;
        let acc_changes = access_list.changes.get(&addr).unwrap();
        // New account should record all values at the current index
        assert_eq!(acc_changes.balance_changes.get(&0), Some(&uint!(1000_U256)));
        assert_eq!(acc_changes.nonce_changes.get(&0), Some(&1));
        assert_eq!(acc_changes.code_changes.get(&0), Some(&bytecode));

        Ok(())
    }

    #[test]
    fn test_multiple_transactions() -> eyre::Result<()> {
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

        let access_list = bal_db.finish()?;
        let acc_changes = access_list.changes.get(&addr).unwrap();
        // Should have initial state and two changes
        assert_eq!(acc_changes.balance_changes.get(&0), Some(&uint!(900_U256)));
        assert_eq!(acc_changes.balance_changes.get(&1), Some(&uint!(800_U256)));

        Ok(())
    }

    #[test]
    fn test_code_by_hash() -> eyre::Result<()> {
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

        let access_list = bal_db.finish()?;
        assert!(access_list.changes.is_empty());

        Ok(())
    }

    #[test]
    fn test_block_hash() -> eyre::Result<()> {
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

        let access_list = bal_db.finish()?;
        assert!(access_list.changes.is_empty());

        Ok(())
    }
}
