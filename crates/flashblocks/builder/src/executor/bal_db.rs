use std::collections::hash_map::Entry;

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
/// This function intentionally does not implement `DatabaseRef` because
/// we need mutable access to update the access list during reads.
#[derive(Clone, Debug)]
pub struct BalBuilderDb<DB> {
    /// The underlying database.
    pub db: DB,
    /// The Flashblock Access List under construction.
    pub access_list: FlashblockAccessListConstruction,
    /// The starting transaction index for this Flashblock.
    pub start_index: u16,
    /// The current transaction index within the overall block.
    pub index: u16,
}

impl<DB> BalBuilderDb<DB> {
    pub fn new(db: DB, start_index: u16) -> Self {
        Self {
            db,
            access_list: Default::default(),
            start_index,
            index: 0,
        }
    }
}

impl<DB> BalBuilderDb<DB> {
    pub fn set_index(&mut self, index: u16) {
        self.index = index;
    }
}

// impl<DB: DatabaseRef> DatabaseRef for BalBuilderDb<DB> {
//     type Error = DB::Error;
//
//     fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
//         self.db.basic_ref(address)
//     }
//
//     fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
//         self.db.code_by_hash_ref(code_hash)
//     }
//
//     fn storage_ref(
//         &self,
//         address: Address,
//         index: StorageKey,
//     ) -> Result<StorageValue, Self::Error> {
//         let mut account = self.access_list.changes.entry(address).or_default();
//         account.storage_reads.insert(index);
//         self.db.storage_ref(address, index)
//     }
//
//     fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
//         self.db.block_hash_ref(number)
//     }
// }

impl<DB: Database> Database for BalBuilderDb<DB> {
    type Error = DB::Error;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        // Check if we've read this account before
        let account = self.db.basic(address)?;

        // TODO: None of this logic is required if we don't adhere to spec
        // and require that all prestate is included in access list.
        //
        // TODO: When database returns None, do we enter nothing, or a default account into access
        // list?
        if let Some(account) = &account {
            // The account exists in the db
            let mut entry = self.access_list.changes.entry(address).or_default();

            // Check to see if there is any balance info at start index
            let balance_change = entry.balance_changes.entry(self.start_index);
            if matches!(balance_change, Entry::Vacant(_)) {
                // No balance change at start index, so we record the initial balance
                balance_change.or_insert(account.balance);

                // Since balance_changes was empty, we can assume
                // nonce and code changes have also not been recorded yet
                entry
                    .nonce_changes
                    .entry(self.start_index)
                    .or_insert(account.nonce);

                // TODO: Maybe code_changes should take options?
                if let Some(code) = &account.code {
                    entry
                        .code_changes
                        .entry(self.start_index)
                        .or_insert(code.clone());
                }
            }
        }
        Ok(account)
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.db.code_by_hash(code_hash)
    }

    fn storage(
        &mut self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        // TODO: insert initial storage values into access list
        let mut account = self.access_list.changes.entry(address).or_default();
        account.storage_reads.insert(index);
        self.db.storage(address, index)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.db.block_hash(number)
    }
}

impl<DB: DatabaseCommit + DatabaseRef> DatabaseCommit for BalBuilderDb<DB> {
    // TODO: Using a temporal db internaly here may be more efficient
    fn commit(&mut self, changes: HashMap<Address, revm::state::Account>) {
        changes.iter().for_each(|(address, account)| {
            let mut acc_changes = self.access_list.changes.entry(*address).or_default();

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
        });
        self.db.commit(changes)
    }
}
