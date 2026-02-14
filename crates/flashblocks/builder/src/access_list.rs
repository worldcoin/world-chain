use alloy_eip7928::{
    AccountChanges, BalanceChange, CodeChange, NonceChange, SlotChanges, StorageChange,
};
use alloy_primitives::{Address, U256};
use dashmap::DashMap;
use flashblocks_primitives::access_list::FlashblockAccessList;
use rayon::prelude::*;

use revm::state::{Bytecode, bal::Bal};
use std::collections::{HashMap, HashSet};

pub(crate) type BlockAccessIndex = u16;

/// A convenience builder type for [`FlashblockAccessList`]
#[derive(Debug, Clone)]
pub struct FlashblockAccessListConstruction {
    /// Map from Address -> AccountChangesConstruction
    pub changes: DashMap<Address, AccountChangesConstruction>,
}

impl Default for FlashblockAccessListConstruction {
    fn default() -> Self {
        Self::new()
    }
}

impl FlashblockAccessListConstruction {
    /// Creates a new empty [`FlashblockAccessListConstruction`]
    pub fn new() -> Self {
        Self {
            changes: DashMap::new(),
        }
    }

    /// Convert from revm's [`Bal`] to [`FlashblockAccessListConstruction`].
    pub fn from_revm_bal(bal: Bal) -> Self {
        let changes = DashMap::new();

        for (address, account_bal) in bal.accounts {
            let acc_changes = AccountChangesConstruction {
                balance_changes: account_bal
                    .account_info
                    .balance
                    .writes
                    .into_iter()
                    .map(|(idx, val)| (idx as u16, val))
                    .collect(),

                nonce_changes: account_bal
                    .account_info
                    .nonce
                    .writes
                    .into_iter()
                    .map(|(idx, val)| (idx as u16, val))
                    .collect(),

                code_changes: account_bal
                    .account_info
                    .code
                    .writes
                    .into_iter()
                    .map(|(idx, (_, bytecode))| (idx as u16, bytecode))
                    .collect(),

                storage_changes: account_bal
                    .storage
                    .storage
                    .into_iter()
                    .filter(|(_, writes)| !writes.is_empty())
                    .map(|(slot, writes)| {
                        let slot_changes = writes
                            .writes
                            .into_iter()
                            .map(|(idx, val)| (idx as u16, val))
                            .collect();
                        (slot, slot_changes)
                    })
                    .collect(),

                storage_reads: HashSet::new(),
            };

            changes.insert(address, acc_changes);
        }

        Self { changes }
    }

    /// Merges another [`FlashblockAccessListConstruction`] into this one
    pub fn merge(&mut self, other: Self) {
        for entry in other.changes.into_iter() {
            let (address, other_account_changes) = entry;
            self.changes
                .entry(address)
                .and_modify(|existing| existing.merge(other_account_changes.clone()))
                .or_insert(other_account_changes);
        }
    }

    /// Consumes the builder and produces a [`FlashblockAccessList`]
    ///
    /// Note: All Empty, and Adjecent changes are removed (favoring the last occurrence)
    pub fn build(self, (min_tx_index, max_tx_index): (u16, u16)) -> FlashblockAccessList {
        // Sort addresses lexicographically
        let mut changes: Vec<_> = self
            .changes
            .into_par_iter()
            .map(|(k, v)| v.build(k))
            .collect();

        changes.par_sort_unstable_by_key(|a| a.address);

        let mut access_list = FlashblockAccessList {
            changes,
            min_tx_index,
            max_tx_index,
        };

        access_list.flush();

        access_list
    }

    /// Maps a mutable reference to the [`AccountChangesConstruction`] corresponding to `address` at the given closure.
    pub fn map_account_change<F>(&self, address: Address, f: F)
    where
        F: FnOnce(&mut AccountChangesConstruction),
    {
        let mut entry = self
            .changes
            .entry(address)
            .or_insert_with(AccountChangesConstruction::default);

        f(&mut entry);
    }
}

/// A convenience builder type for [`AccountChanges`]
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct AccountChangesConstruction {
    /// Map from Storage Slot -> (Map from transaction index -> Value)
    pub storage_changes: HashMap<U256, HashMap<BlockAccessIndex, U256>>,
    /// Set of storage slots read
    pub storage_reads: HashSet<U256>,
    /// Map of balance changes
    pub balance_changes: HashMap<BlockAccessIndex, U256>,
    /// Map of nonce changes
    pub nonce_changes: HashMap<BlockAccessIndex, u64>,
    /// Map of code changes
    pub code_changes: HashMap<BlockAccessIndex, Bytecode>,
}

impl AccountChangesConstruction {
    /// Merges another [`AccountChangesConstruction`] into this one
    pub fn merge(&mut self, other: Self) {
        for (slot, other_tx_map) in other.storage_changes {
            self.storage_changes
                .entry(slot)
                .and_modify(|existing_tx_map| existing_tx_map.extend(other_tx_map.clone()))
                .or_insert(other_tx_map);
        }
        self.storage_reads.extend(other.storage_reads);
        self.balance_changes.extend(other.balance_changes);
        self.nonce_changes.extend(other.nonce_changes);
        self.code_changes.extend(other.code_changes);
    }

    /// Consumes the builder and produces an [`AccountChanges`] for the given address
    ///
    /// Note: This will sort all changes by transaction index, and storage slots by their value.
    pub fn build(mut self, address: Address) -> AccountChanges {
        let sorted_storage_changes: Vec<_> = {
            let mut slots = self.storage_changes.keys().cloned().collect::<Vec<_>>();
            slots.sort_unstable();

            slots
                .into_iter()
                .map(|s| {
                    let mut storage_changes = self
                        .storage_changes
                        .remove(&s)
                        .unwrap()
                        .into_iter()
                        .collect::<Vec<_>>();

                    storage_changes.sort_unstable_by_key(|(tx_index, _)| *tx_index);

                    (s, storage_changes)
                })
                .collect()
        };

        let mut storage_reads_sorted: Vec<_> = self.storage_reads.drain().collect();
        storage_reads_sorted.sort_unstable();

        let mut balance_changes_sorted: Vec<_> = self.balance_changes.drain().collect();
        balance_changes_sorted.sort_unstable_by_key(|(tx_index, _)| *tx_index);

        let mut nonce_changes_sorted: Vec<_> = self.nonce_changes.drain().collect();
        nonce_changes_sorted.sort_unstable_by_key(|(tx_index, _)| *tx_index);

        let mut code_changes_sorted: Vec<_> = self.code_changes.drain().collect();
        code_changes_sorted.sort_unstable_by_key(|(tx_index, _)| *tx_index);

        AccountChanges {
            address,
            storage_changes: sorted_storage_changes
                .into_iter()
                .map(|(slot, tx_map)| SlotChanges {
                    slot: slot.into(),
                    changes: tx_map
                        .into_iter()
                        .map(|(tx_index, value)| StorageChange {
                            block_access_index: tx_index as u64,
                            new_value: value.into(),
                        })
                        .collect(),
                })
                .collect(),
            storage_reads: storage_reads_sorted.into_iter().map(Into::into).collect(),
            balance_changes: balance_changes_sorted
                .into_iter()
                .map(|(tx_index, value)| BalanceChange {
                    block_access_index: tx_index as u64,
                    post_balance: value,
                })
                .collect(),
            nonce_changes: nonce_changes_sorted
                .into_iter()
                .map(|(tx_index, value)| NonceChange {
                    block_access_index: tx_index as u64,
                    new_nonce: value,
                })
                .collect(),
            code_changes: code_changes_sorted
                .into_iter()
                .map(|(tx_index, bytecode)| CodeChange {
                    block_access_index: tx_index as u64,
                    new_code: bytecode.original_bytes(),
                })
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.storage_changes.is_empty()
            && self.storage_reads.is_empty()
            && self.balance_changes.is_empty()
            && self.nonce_changes.is_empty()
            && self.code_changes.is_empty()
    }
}
