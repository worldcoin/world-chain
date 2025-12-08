use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU16, Ordering},
    },
    thread::JoinHandle,
    u16,
};

use alloy_primitives::{Address, B256, U256};
use crossbeam_channel::Sender;
use reth_evm::{
    OnStateHook,
    block::{BlockExecutionError, StateChangePostBlockSource, StateChangeSource, StateDB},
};
use revm::{
    Database, DatabaseCommit, DatabaseRef,
    bytecode::bitvec::store::BitStore,
    database::{BundleState, states::bundle_state::BundleRetention},
    primitives::{HashMap, StorageKey, StorageValue},
    state::{AccountInfo, Bytecode},
};
use tracing::error;

use crate::access_list::FlashblockAccessListConstruction;

/// A thread safe shared counter tracking the current transaction index in the block.
#[derive(Debug, Clone, Default)]
pub struct AccessIndex {
    /// The current index of the transaction being processed.
    ///
    /// If we're currently building the 0th tx, this will be 0.
    /// If we're reading the 0th
    index: Arc<AtomicU16>,
    is_init: bool,
}

impl AccessIndex {
    /// Creates a new AccessIndex starting at zero.
    pub fn new(index: u16) -> Self {
        Self {
            index: Arc::new(AtomicU16::new(index)),
            is_init: true,
        }
    }

    /// Returns a clone of the internal Arc<AtomicU16>.
    pub fn index(&self) -> u16 {
        self.index.load(Ordering::SeqCst)
    }

    /// Sets the current index value.
    pub fn set(&self, index: u16) {
        self.index.store(index, Ordering::SeqCst);
    }

    /// Increments the index and returns the new value.
    pub fn increment(&self) -> u16 {
        self.index.fetch_add(1, Ordering::SeqCst) + 1
    }
}

impl OnStateHook for AccessIndex {
    fn on_state(&mut self, source: StateChangeSource, _state: &revm::state::EvmState) {
        match source {
            StateChangeSource::Transaction(_) => {
                if self.is_init {
                    self.is_init = false
                } else {
                    self.increment();
                }
            }
            StateChangeSource::PreBlock(_) => {
                self.set(0);
            }
            _ => {}
        }
    }
}

/// Messages sent to the background builder for recording reads/writes.
enum BalBuilderMsg {
    StorageRead(Address, StorageKey),
    Commit(HashMap<Address, revm::state::Account>),
    MergeAcccessList(FlashblockAccessListConstruction),
    SetIndex(u16),
}

/// A wrapper around a database that builds a Flashblock
/// Access List during execution.
///
/// The current index is read from a [`SharedBlockIndex`] which is updated
/// by a [`BalIndexHook`] during EVM execution. This ensures the index is
/// always synchronized with the execution phase.
#[derive(Clone, Debug)]
pub struct BalBuilderDb<DB> {
    /// Underlying cached database.
    db: DB,
    /// The Flashblock Access List under construction.
    access_list: FlashblockAccessListConstruction,
    /// The current index being built
    index: AccessIndex,
}

impl<DB> BalBuilderDb<DB>
where
    DB: DatabaseCommit + Database + DatabaseRef<Error = <DB as Database>::Error> + Send + Sync,
{
    /// Creates a new BalBuilderDb around the given database with a shared index.
    pub fn new(db: DB, index: AccessIndex) -> Self {
        Self {
            db,
            access_list: Default::default(),
            index,
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
        let index = self.index.index();
        eprintln!("index in try_commit {index}");

        // When we commit new account state we must first load the previous account state. Only
        // what's changed should be published to the access list.
        changes.iter().try_for_each(|(address, account)| {
            let mut acc_changes = self.access_list.changes.entry(*address).or_default();

            // There is an edge case here where we could change an account, then change it
            // back. This would result in appending a value to the access list that isn't strctly
            // required. For this reason we should dedup consecutive identical entries when
            // finalizing the access list.
            match self.db.basic(*address)? {
                Some(previous) => {
                    if previous.balance != account.info.balance {
                        let latest = acc_changes
                            .balance_changes
                            .get(&index)
                            .unwrap_or(&U256::ZERO);

                        if *latest != account.info.balance {
                            acc_changes
                                .balance_changes
                                .insert(index, account.info.balance);
                        }
                    }

                    if previous.nonce != account.info.nonce {
                        let latest = acc_changes.nonce_changes.get(&index).unwrap_or(&0);
                        if *latest != account.info.nonce {
                            acc_changes.nonce_changes.insert(index, account.info.nonce);
                        }
                    }

                    if previous.code_hash != account.info.code_hash {
                        let bytecode = match account.info.code.clone() {
                            Some(code) => code,
                            None => self.db.code_by_hash_ref(account.info.code_hash)?,
                        };

                        let latest = acc_changes
                            .code_changes
                            .get(&index)
                            .cloned()
                            .unwrap_or_default();

                        if latest != bytecode {
                            acc_changes.code_changes.insert(index, bytecode);
                        }
                    }
                }
                None => {
                    if !account.info.balance.is_zero() {
                        acc_changes
                            .balance_changes
                            .insert(index, account.info.balance);
                    }

                    if account.info.nonce != 0 {
                        acc_changes.nonce_changes.insert(index, account.info.nonce);
                    }

                    let bytecode = match account.info.code.clone() {
                        Some(code) => code,
                        None => self.db.code_by_hash_ref(account.info.code_hash)?,
                    };

                    if !bytecode.is_empty() {
                        acc_changes.code_changes.insert(index, bytecode);
                    }
                }
            }

            account.storage.iter().try_for_each(|(key, value)| {
                let previous_value = self.db.storage_ref(*address, *key)?;
                if previous_value != value.present_value {
                    acc_changes
                        .storage_changes
                        .entry(*key)
                        .or_default()
                        .insert(index, value.present_value);
                }
                Result::<(), <DB as Database>::Error>::Ok(())
            })?;

            Ok(())
        })?;

        self.db.commit(changes);

        Ok(())
    }

    /// Consumes self and returns the constructed access list.
    pub fn finish(self) -> FlashblockAccessListConstruction {
        self.access_list
    }
}

impl<DB> Database for BalBuilderDb<DB>
where
    DB: DatabaseCommit + Database + DatabaseRef<Error = <DB as Database>::Error> + Send + Sync,
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
    DB: DatabaseCommit + Database + DatabaseRef<Error = <DB as Database>::Error> + Send + Sync,
{
    fn commit(&mut self, changes: HashMap<Address, revm::state::Account>) {
        // TODO: perhaps we should store an optional error inside the struct
        // and return it on `finish()` instead?
        if let Err(e) = self.try_commit(changes) {
            error!("Error committing to BalBuilderDb: {:?}", e);
        }
    }
}

impl<DB> StateDB for BalBuilderDb<DB>
where
    DB: StateDB
        + DatabaseCommit
        + Database
        + DatabaseRef<Error = <DB as Database>::Error>
        + Send
        + Sync,
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
/// Commiting to this database will both commit to the inner database
/// and update the access list under construction in a background thread.
///
/// The current index is managed via a [`SharedBlockIndex`] which is updated
/// by a [`BalIndexHook`] during EVM execution.
#[derive(Debug)]
pub struct AsyncBalBuilderDb<DB> {
    /// The underlying read/write database.
    db: DB,
    /// The sender to the builder thread.
    tx: Sender<BalBuilderMsg>,
    /// Join handle for the builder thread
    handle: JoinHandle<Result<FlashblockAccessListConstruction, BlockExecutionError>>,
}

impl<DB> AsyncBalBuilderDb<DB> {
    /// Creates a new builder around a writable DB plus a dummy mirror that
    /// the background thread uses to compare state when deriving changes.
    ///
    /// The `shared_index` is shared with a [`BalIndexHook`] which updates the
    /// index based on EVM execution phase (pre-execution, transaction, post-execution).
    pub fn new<DDB>(db: DB, dummy_db: DDB, index: AccessIndex) -> Self
    where
        DDB: Database
            + DatabaseRef<Error = <DDB as Database>::Error>
            + DatabaseCommit
            + Send
            + Sync
            + 'static,
        <DDB as Database>::Error: Send + Sync + 'static,
    {
        let (tx, rx) = crossbeam_channel::unbounded::<BalBuilderMsg>();
        let mut bal_builder = BalBuilderDb::new(dummy_db, index);

        let handle = std::thread::spawn(move || {
            while let Ok(msg) = rx.recv() {
                match msg {
                    BalBuilderMsg::StorageRead(address, index) => {
                        bal_builder.handle_storage_read(address, index);
                    }
                    BalBuilderMsg::Commit(changes) => {
                        bal_builder
                            .try_commit(changes)
                            .map_err(BlockExecutionError::other)?;
                    }
                    BalBuilderMsg::MergeAcccessList(access_list) => {
                        bal_builder.merge_access_list(access_list);
                    }
                    BalBuilderMsg::SetIndex(index) => {
                        bal_builder.index.set(index);
                    }
                }
            }

            Ok(bal_builder.access_list)
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
    pub fn finish(self) -> Result<FlashblockAccessListConstruction, BlockExecutionError> {
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

impl<DB: DatabaseCommit> DatabaseCommit for AsyncBalBuilderDb<DB> {
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
        AsyncBalBuilderDb::new(db, read_db, AccessIndex::default())
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

        // Set index via set_index method
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

        // Set different index
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
