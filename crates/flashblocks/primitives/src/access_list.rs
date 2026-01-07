use alloy_eip7928::{
    AccountChanges, BalanceChange, CodeChange, NonceChange, SlotChanges, StorageChange,
};
use alloy_primitives::{Address, B256, Bytes, FixedBytes, U256};
use alloy_rlp::{RlpDecodable, RlpEncodable};
use reth::revm::{
    DatabaseRef,
    db::{AccountStatus, BundleAccount, BundleState, states::StorageSlot},
    state::{AccountInfo, Bytecode},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(
    Clone, Debug, PartialEq, Default, Deserialize, Serialize, Eq, RlpEncodable, RlpDecodable,
)]
pub struct FlashblockAccessListData {
    /// The [`FlashblockAccessList`] containing all [`AccountChanges`] that occured throughout execution of a
    /// single Flashblock `diff`
    pub access_list: FlashblockAccessList,
    /// The associated Keccak256 hash of the RLP-Encoding of [`FlashblockAccessList`]
    pub access_list_hash: B256,
}

#[derive(
    Clone, Debug, PartialEq, Default, Deserialize, Serialize, Eq, RlpEncodable, RlpDecodable,
)]
pub struct FlashblockAccessList {
    /// All [`AccountChanges`] constructed during the execution of transactions from `min_tx_index` to `max_tx_index`
    pub changes: Vec<AccountChanges>,
    /// The Minimum transaction index in the global indexing of the block.
    pub min_tx_index: u64,
    /// The Maximum transaction index in the global indexing of the block.
    /// Note: This will always correspond to the System Transaction e.g. Balance Increments
    pub max_tx_index: u64,
}

impl FlashblockAccessList {
    /// Removes duplicate entries by key, keeping the last occurrence (highest block_access_index).
    /// Results are re-ordered ascending by block_access_index.
    pub fn flush(&mut self) {
        let ret_non_empty = |c: &mut Vec<AccountChanges>| {
            c.retain_mut(|change: &mut AccountChanges| {
                let balance_changes = change
                    .balance_changes
                    .iter()
                    .filter(|b| b.post_balance() != U256::ZERO)
                    .count();
                let nonce_changes = change
                    .nonce_changes
                    .iter()
                    .filter(|n| n.new_nonce() != 0)
                    .count();
                let code_changes = change
                    .code_changes
                    .iter()
                    .filter(|c| *c.new_code() != Bytes::default())
                    .count();
                let storage_changes = change
                    .storage_changes
                    .iter()
                    .filter(|slot_changes| {
                        slot_changes
                            .changes
                            .iter()
                            .any(|s| s.new_value != FixedBytes::<32>::ZERO)
                    })
                    .count();

                !(balance_changes == 0
                    && nonce_changes == 0
                    && code_changes == 0
                    && storage_changes == 0)
            })
        };

        ret_non_empty(&mut self.changes);

        for change in &mut self.changes {
            // De-duplicate by ascending block_access_index, keeping the last occurrence
            change.balance_changes.reverse();
            change.balance_changes.dedup_by_key(|n| n.post_balance());
            change.balance_changes.sort_by_key(|n| n.block_access_index);

            change.nonce_changes.reverse();
            change.nonce_changes.dedup_by_key(|n| n.new_nonce());
            change.nonce_changes.sort_by_key(|n| n.block_access_index);

            change.code_changes.reverse();
            change.code_changes.dedup_by_key(|c| c.new_code().clone());
            change.code_changes.sort_by_key(|c| c.block_access_index);

            for slot_changes in &mut change.storage_changes {
                slot_changes.changes.reverse();
                slot_changes.changes.dedup_by_key(|s| s.new_value);
                slot_changes.changes.sort_by_key(|s| s.block_access_index);
            }
        }
    }

    /// Extends this [`FlashblockAccessList`] with another, merging account changes appropriately.
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

    /// Extends a [`BundleState`] with the account changes from this [`FlashblockAccessList`]
    ///
    /// Pulls relevant account `info` from the latest bundle or constructs from the database if missing.
    pub fn extend_bundle<DB: DatabaseRef>(
        &self,
        bundle: &mut BundleState,
        db: &DB,
    ) -> Result<(), DB::Error> {
        let changes = &self.changes;

        for account_changes in changes {
            let account = account_changes.address;

            let bundle_account = bundle.state.entry(account).or_insert_with(|| {
                let db_account = db.basic_ref(account).unwrap_or(None);
                BundleAccount {
                    info: db_account.clone(),
                    original_info: db_account,
                    storage: alloy_primitives::map::HashMap::default(),
                    status: AccountStatus::Loaded,
                }
            });

            // Apply storage changes
            for slot_changes in &account_changes.storage_changes {
                let slot: U256 = slot_changes.slot.into();

                for change in &slot_changes.changes {
                    let original_value = db
                        .storage_ref(account_changes.address, slot)
                        .ok()
                        .unwrap_or(U256::ZERO);

                    bundle_account.storage.insert(
                        slot,
                        StorageSlot {
                            previous_or_original_value: original_value, // TODO: Revisit this logic
                            present_value: change.new_value.into(),
                        },
                    );

                    bundle_account.status = AccountStatus::Changed;
                }
            }

            // Apply balance changes
            let balance_changes = account_changes.balance_changes();
            let latest_change = balance_changes.iter().max_by_key(|b| b.block_access_index);

            if let Some(change) = latest_change {
                if let Some(info) = &mut bundle_account.info {
                    info.balance = change.post_balance();
                } else {
                    bundle_account.info = Some(AccountInfo {
                        balance: change.post_balance(),
                        ..Default::default()
                    });
                }

                bundle_account.status = AccountStatus::Changed;
            }

            // Apply nonce changes
            let nonce_changes = account_changes.nonce_changes();
            let latest_nonce_change = nonce_changes.iter().max_by_key(|n| n.block_access_index);

            if let Some(change) = latest_nonce_change {
                if let Some(info) = &mut bundle_account.info {
                    info.nonce = change.new_nonce();
                } else {
                    bundle_account.info = Some(AccountInfo {
                        nonce: change.new_nonce(),
                        ..Default::default()
                    });
                }

                bundle_account.status = AccountStatus::Changed;
            }

            // Apply code changes
            let code_changes = account_changes.code_changes();
            let latest_code_change = code_changes.iter().max_by_key(|c| c.block_access_index);

            if let Some(change) = latest_code_change {
                let bytecode = Bytecode::new_raw(change.new_code().clone());
                let code_hash = bytecode.hash_slow();

                if let Some(info) = &mut bundle_account.info {
                    info.code_hash = code_hash;
                    info.code = Some(bytecode);
                } else {
                    bundle_account.info = Some(AccountInfo {
                        code_hash,
                        code: Some(bytecode),
                        ..Default::default()
                    });
                }

                bundle_account.status = AccountStatus::Changed;
            }

            // Update the bundle with the modified account
            if bundle_account.status.is_not_modified()
                || (bundle_account.original_info.is_none()
                    && bundle_account.info.as_ref().is_some_and(|i| i.is_empty()))
            {
                bundle.state.remove(&account);
            }
        }

        Ok(())
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

/// Computes the Keccak256 hash of the RLP-Encoding of a [`FlashblockAccessList`]
pub fn access_list_hash(access_list: &FlashblockAccessList) -> B256 {
    let rlp_encoded = alloy_rlp::encode(access_list);
    alloy_primitives::keccak256(&rlp_encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, map::HashMap};
    use reth::revm::{db::AccountStatus, primitives::KECCAK_EMPTY};
    use std::convert::Infallible;

    /// Mock database for testing that can be configured with initial account state
    #[derive(Debug, Clone, Default)]
    struct MockDb {
        accounts: HashMap<Address, AccountInfo>,
    }

    impl MockDb {
        fn new() -> Self {
            Self::default()
        }

        fn with_account(mut self, address: Address, info: AccountInfo) -> Self {
            self.accounts.insert(address, info);
            self
        }
    }

    impl DatabaseRef for MockDb {
        type Error = Infallible;

        fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(self.accounts.get(&address).cloned())
        }

        fn code_by_hash_ref(&self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        fn storage_ref(&self, _address: Address, _index: U256) -> Result<U256, Self::Error> {
            Ok(U256::ZERO)
        }

        fn block_hash_ref(&self, _number: u64) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    #[test]
    fn test_extend_populated_bundle() {
        let account = address!("0x1000000000000000000000000000000000000000");

        let info = AccountInfo {
            balance: U256::from(1000),
            nonce: 1,
            code_hash: KECCAK_EMPTY,
            code: None,
            account_id: None,
        };

        let bundle_account = BundleAccount {
            info: Some(AccountInfo {
                balance: U256::from(1000),
                nonce: 2,
                code_hash: KECCAK_EMPTY,
                code: None,
                account_id: None,
            }),
            original_info: Some(info.clone()),
            storage: alloy_primitives::map::HashMap::default(),
            status: AccountStatus::Loaded,
        };

        let mut bundle = BundleState {
            state: HashMap::from_iter([(account, bundle_account)]),
            ..Default::default()
        };

        let mut access_list = FlashblockAccessList::default();

        access_list.changes.push(AccountChanges {
            address: account,
            balance_changes: vec![BalanceChange {
                block_access_index: 1,
                post_balance: U256::from(1500),
            }],
            ..Default::default()
        });

        let db = MockDb::new().with_account(account, info.clone());

        access_list
            .extend_bundle(&mut bundle, &db)
            .expect("Failed to extend bundle");

        let new_bundle_account = bundle
            .state
            .get(&account)
            .expect("Account missing in bundle")
            .clone();

        assert_eq!(
            new_bundle_account.info.unwrap(),
            AccountInfo {
                balance: U256::from(1500),
                nonce: 2,
                code_hash: KECCAK_EMPTY,
                code: None,
                account_id: None
            }
        );

        assert_eq!(new_bundle_account.original_info.unwrap(), info);
        assert_eq!(new_bundle_account.status, AccountStatus::Changed);
    }

    /// Tests that when an account is NOT in the bundle but IS in the database,
    /// the database info is used as the base for both `info` and `original_info`.
    #[test]
    fn test_extend_bundle_falls_back_to_database_when_account_missing() {
        let account = address!("0x2000000000000000000000000000000000000000");

        // Account info in database (simulating existing on-chain state)
        let db_info = AccountInfo {
            balance: U256::from(5000),
            nonce: 10,
            code_hash: KECCAK_EMPTY,
            code: None,
            account_id: None,
        };

        let db = MockDb::new().with_account(account, db_info.clone());

        // Empty bundle - account does NOT exist here
        let mut bundle = BundleState::default();
        assert!(!bundle.state.contains_key(&account));

        // Create access list with a balance change for the account
        let mut access_list = FlashblockAccessList::default();
        access_list.changes.push(AccountChanges {
            address: account,
            balance_changes: vec![BalanceChange {
                block_access_index: 1,
                post_balance: U256::from(6000),
            }],
            ..Default::default()
        });

        access_list
            .extend_bundle(&mut bundle, &db)
            .expect("Failed to extend bundle");

        // Account should now exist in bundle
        let bundle_account = bundle
            .state
            .get(&account)
            .expect("Account should exist in bundle after extend");

        // Info should have the updated balance but preserve nonce from DB
        let info = bundle_account.info.as_ref().expect("info should exist");
        assert_eq!(info.balance, U256::from(6000), "balance should be updated");
        assert_eq!(info.nonce, 10, "nonce should be preserved from database");
        assert_eq!(info.code_hash, KECCAK_EMPTY);

        // Original info should match what was in the database
        let original_info = bundle_account
            .original_info
            .as_ref()
            .expect("original_info should exist");

        assert_eq!(
            *original_info, db_info,
            "original_info should match database state"
        );

        assert_eq!(bundle_account.status, AccountStatus::Changed);
    }
}
