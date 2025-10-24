use alloy_eip7928::{
    AccountChanges, BalanceChange, CodeChange, NonceChange, SlotChanges, StorageChange,
};
use alloy_primitives::map::foldhash::HashMap as AlloyHashMap;
use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_rlp::{RlpDecodable, RlpEncodable};
use reth::revm::{
    db::{states::StorageSlot, AccountStatus, BundleAccount},
    state::{AccountInfo, Bytecode},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(
    Clone, Debug, PartialEq, Default, Deserialize, Serialize, Eq, RlpEncodable, RlpDecodable,
)]
pub struct FlashblockAccessList {
    pub changes: Vec<AccountChanges>,
}

impl FlashblockAccessList {
    pub fn extend(&mut self, other: &FlashblockAccessList) {
        // Create a map to merge AccountChanges by address
        let mut merged: BTreeMap<Address, AccountChanges> = BTreeMap::new();

        // Insert all existing changes into the map
        for account in self.changes.drain(..) {
            merged.insert(account.address, account);
        }

        // Merge with other's changes
        for other_account in &other.changes {
            merged
                .entry(other_account.address)
                .and_modify(|existing| {
                    merge_account_changes(existing, other_account);
                })
                .or_insert_with(|| other_account.clone());
        }

        // Rebuild the sorted vector
        self.changes = merged.into_values().collect();
    }
}

/// Helper function to merge two [`AccountChanges`] preserving lexicographic order.
fn merge_account_changes(existing: &mut AccountChanges, other: &AccountChanges) {
    let mut storage_map: BTreeMap<B256, BTreeMap<u64, StorageChange>> = BTreeMap::new();

    for slot_changes in &existing.storage_changes {
        let mut changes_map = BTreeMap::new();
        for change in &slot_changes.changes {
            changes_map.insert(change.block_access_index, change.clone());
        }
        storage_map.insert(slot_changes.slot, changes_map);
    }

    for slot_changes in &other.storage_changes {
        storage_map.entry(slot_changes.slot).or_default().extend(
            slot_changes
                .changes
                .iter()
                .map(|c| (c.block_access_index, c.clone())),
        );
    }

    existing.storage_changes = storage_map
        .into_iter()
        .map(|(slot, changes_map)| SlotChanges {
            slot,
            changes: changes_map.into_values().collect(),
        })
        .collect();

    let mut storage_reads_set: BTreeMap<B256, ()> = BTreeMap::new();
    for read in &existing.storage_reads {
        storage_reads_set.insert(*read, ());
    }
    for read in &other.storage_reads {
        storage_reads_set.insert(*read, ());
    }
    existing.storage_reads = storage_reads_set.into_keys().collect();

    let mut balance_map: BTreeMap<u64, BalanceChange> = BTreeMap::new();
    for change in &existing.balance_changes {
        balance_map.insert(change.block_access_index, change.clone());
    }
    for change in &other.balance_changes {
        balance_map.insert(change.block_access_index, change.clone());
    }
    existing.balance_changes = balance_map.into_values().collect();

    let mut nonce_map: BTreeMap<u64, NonceChange> = BTreeMap::new();
    for change in &existing.nonce_changes {
        nonce_map.insert(change.block_access_index, change.clone());
    }
    for change in &other.nonce_changes {
        nonce_map.insert(change.block_access_index, change.clone());
    }
    existing.nonce_changes = nonce_map.into_values().collect();

    let mut code_map: BTreeMap<u64, CodeChange> = BTreeMap::new();
    for change in &existing.code_changes {
        code_map.insert(change.block_access_index, change.clone());
    }
    for change in &other.code_changes {
        code_map.insert(change.block_access_index, change.clone());
    }
    existing.code_changes = code_map.into_values().collect();
}

impl From<FlashblockAccessList> for HashMap<Address, BundleAccount> {
    fn from(value: FlashblockAccessList) -> Self {
        let mut result = HashMap::new();
        for account in value.changes.iter() {
            let address = account.address;

            let mut account_storage = AlloyHashMap::default();

            // Aggregate the storage changes. Keep the latest value stored for each storage key.
            // This assumes that the changes are ordered by transaction index.
            for change in &account.storage_changes {
                let slot: U256 = change.slot.into();

                let latest_value = match change.changes.last() {
                    Some(change) => change.new_value,
                    None => continue,
                };

                account_storage.insert(
                    slot,
                    StorageSlot {
                        previous_or_original_value: U256::ZERO,
                        present_value: latest_value.into(),
                    },
                );
            }

            // Accumulate the latest account info changes.
            let latest_balance_change = account
                .balance_changes()
                .last()
                .map(|change| change.post_balance())
                .unwrap_or_default();

            let latest_nonce_change = account
                .nonce_changes()
                .last()
                .map(|change| change.new_nonce())
                .unwrap_or_default();

            let latest_code_changes = account
                .code_changes()
                .last()
                .map(|change| change.new_code())
                .unwrap_or_default();

            let code_hash = keccak256(latest_code_changes.as_ref());

            let account_info = AccountInfo {
                balance: latest_balance_change,
                nonce: latest_nonce_change,
                code_hash,
                code: Some(Bytecode::new_raw(latest_code_changes.clone())),
            };

            let bundle_account = BundleAccount {
                info: Some(account_info),
                original_info: None,
                storage: account_storage,
                status: AccountStatus::Changed,
            };

            // Insert or update the account in the resulting map.
            result.insert(address, bundle_account);
        }

        result
    }
}
