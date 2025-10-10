use alloy_eip7928::{
    AccountChanges, BalanceChange, CodeChange, NonceChange, SlotChanges, StorageChange,
};
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_rlp::{RlpDecodable, RlpEncodable};
use reth::revm::state::Account;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub type StateAccesses = HashMap<Address, HashMap<B256, ()>>;

pub trait StateAccessesMerge {
    fn merge(&mut self, other: &StateAccesses);
}

impl StateAccessesMerge for StateAccesses {
    fn merge(&mut self, other: &StateAccesses) {
        for (addr, accesses) in other {
            let account_accesses = self.entry(*addr).or_default();
            for slot in accesses.keys() {
                account_accesses.insert(*slot, ());
            }
        }
    }
}

pub type BlockAccessList = Vec<AccountAccess>;

pub type StateDiff = Option<HashMap<Address, Account>>;

/// AccountAccess contains post-block account state for mutations as well as
/// all storage keys that were read during execution. It is used when building block
/// access list during execution.
#[derive(Clone, Debug, PartialEq, Default, Deserialize, Serialize, Eq)]
pub struct AccountAccess {
    pub storage_writes: HashMap<B256, HashMap<u16, B256>>,
    pub storage_reads: HashSet<B256>,
    pub balance_changes: HashMap<u16, U256>,
    pub nonce_changes: HashMap<u16, u64>,
    pub code_changes: HashMap<u16, Bytes>,
}

impl AccountAccess {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashblockBlockAccessList {
    pub min_tx_index: u64,
    pub max_tx_index: u64,
    pub fbal_accumulator: B256,
    pub accounts: HashMap<Address, AccountAccess>,
}

#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, RlpEncodable, RlpDecodable,
)]
pub struct FlashblockBlockAccessListWire {
    pub min_tx_index: u64,
    pub max_tx_index: u64,
    pub fbal_accumulator: B256,
    pub accounts: Vec<AccountChanges>,
}

impl From<FlashblockBlockAccessList> for FlashblockBlockAccessListWire {
    fn from(fbal: FlashblockBlockAccessList) -> Self {
        let accounts = fbal
            .accounts
            .into_iter()
            .map(|(address, access)| {
                let storage_changes = access
                    .storage_writes
                    .into_iter()
                    .map(|(slot, changes)| SlotChanges {
                        slot,
                        changes: changes
                            .into_iter()
                            .map(|(tx_index, new_value)| StorageChange {
                                block_access_index: tx_index as u64,
                                new_value,
                            })
                            .collect(),
                    })
                    .collect();

                let storage_reads = access.storage_reads.into_iter().collect();

                let balance_changes = access
                    .balance_changes
                    .into_iter()
                    .map(|(tx_index, post_balance)| BalanceChange {
                        block_access_index: tx_index as u64,
                        post_balance,
                    })
                    .collect();

                let nonce_changes = access
                    .nonce_changes
                    .into_iter()
                    .map(|(tx_index, new_nonce)| NonceChange {
                        block_access_index: tx_index as u64,
                        new_nonce,
                    })
                    .collect();

                let code_changes = access
                    .code_changes
                    .into_iter()
                    .map(|(tx_index, new_code)| CodeChange {
                        block_access_index: tx_index as u64,
                        new_code,
                    })
                    .collect();

                AccountChanges {
                    address,
                    storage_changes,
                    storage_reads,
                    balance_changes,
                    nonce_changes,
                    code_changes,
                }
            })
            .collect();

        Self {
            min_tx_index: fbal.min_tx_index,
            max_tx_index: fbal.max_tx_index,
            fbal_accumulator: fbal.fbal_accumulator,
            accounts,
        }
    }
}

impl From<FlashblockBlockAccessListWire> for FlashblockBlockAccessList {
    fn from(val: FlashblockBlockAccessListWire) -> Self {
        let accounts = val
            .accounts
            .into_iter()
            .map(|change| {
                let storage_writes = change
                    .storage_changes
                    .into_iter()
                    .map(|slot_change| {
                        let changes = slot_change
                            .changes
                            .into_iter()
                            .map(|storage_change| {
                                (
                                    storage_change.block_access_index as u16,
                                    storage_change.new_value,
                                )
                            })
                            .collect();
                        (slot_change.slot, changes)
                    })
                    .collect();

                let storage_reads = change.storage_reads.into_iter().collect();

                let balance_changes = change
                    .balance_changes
                    .into_iter()
                    .map(|balance_change| {
                        (
                            balance_change.block_access_index as u16,
                            balance_change.post_balance,
                        )
                    })
                    .collect();

                let nonce_changes = change
                    .nonce_changes
                    .into_iter()
                    .map(|nonce_change| {
                        (
                            nonce_change.block_access_index as u16,
                            nonce_change.new_nonce,
                        )
                    })
                    .collect();

                let code_changes = change
                    .code_changes
                    .into_iter()
                    .map(|code_change| {
                        (code_change.block_access_index as u16, code_change.new_code)
                    })
                    .collect();

                (
                    change.address,
                    AccountAccess {
                        storage_writes,
                        storage_reads,
                        balance_changes,
                        nonce_changes,
                        code_changes,
                    },
                )
            })
            .collect();

        Self {
            min_tx_index: val.min_tx_index,
            max_tx_index: val.max_tx_index,
            fbal_accumulator: val.fbal_accumulator,
            accounts,
        }
    }
}

impl FlashblockBlockAccessList {
    /// NewConstructionBlockAccessList instantiates an empty access list.
    pub fn new() -> Self {
        Self::default()
    }

    /// AccountRead records the address of an account that has been read during execution.
    pub fn account_read(&mut self, addr: Address) {
        self.accounts.entry(addr).or_default();
    }

    /// StorageRead records a storage key read during execution.
    pub fn storage_read(&mut self, address: Address, key: B256) {
        let account = self.accounts.entry(address).or_default();
        if account.storage_writes.contains_key(&key) {
            return;
        }
        account.storage_reads.insert(key);
    }

    /// StorageWrite records the post-transaction value of a mutated storage slot.
    /// The storage slot is removed from the list of read slots.
    pub fn storage_write(&mut self, tx_idx: u16, address: Address, key: B256, value: B256) {
        let account = self.accounts.entry(address).or_default();

        account
            .storage_writes
            .entry(key)
            .or_default()
            .insert(tx_idx, value);

        account.storage_reads.remove(&key);
    }

    /// CodeChange records the code of a newly-created contract.
    pub fn code_change(&mut self, address: Address, tx_index: u16, code: Bytes) {
        let account = self.accounts.entry(address).or_default();
        account.code_changes.insert(tx_index, code);
    }

    /// NonceChange records tx post-state nonce of any contract-like accounts whose
    /// nonce was incremented.
    pub fn nonce_change(&mut self, address: Address, tx_idx: u16, post_nonce: u64) {
        let account = self.accounts.entry(address).or_default();
        account.nonce_changes.insert(tx_idx, post_nonce);
    }

    /// BalanceChange records the post-transaction balance of an account whose
    /// balance changed.
    pub fn balance_change(&mut self, tx_idx: u16, address: Address, balance: U256) {
        let account = self.accounts.entry(address).or_default();
        account.balance_changes.insert(tx_idx, balance);
    }

    /// ApplyAccesses records the given account/storage accesses in the BAL.
    pub fn apply_accesses(&mut self, accesses: &StateAccesses) {
        for (addr, acct_accesses) in accesses {
            let account = self.accounts.entry(*addr).or_default();

            if !acct_accesses.is_empty() {
                for key in acct_accesses.keys() {
                    // if any of the accessed keys were previously written, they
                    // appear in the written set only and not also in accesses.
                    if account.storage_writes.contains_key(key) {
                        continue;
                    }
                    account.storage_reads.insert(*key);
                }
            }
        }
    }

    /// Merges AccountChanges from the EIP-7928 inspector into this FlashblockBlockAccessList.
    pub fn merge_changes(&mut self, changes: impl Iterator<Item = AccountChanges>) {
        for change in changes {
            let account = self.accounts.entry(change.address).or_default();

            // Merge storage writes
            for slot_change in change.storage_changes {
                for storage_change in slot_change.changes {
                    let tx_idx = storage_change.block_access_index as u16;
                    account
                        .storage_writes
                        .entry(slot_change.slot)
                        .or_default()
                        .insert(tx_idx, storage_change.new_value);
                }
            }

            // Merge storage reads
            for slot in change.storage_reads {
                // Only add if not already written
                if !account.storage_writes.contains_key(&slot) {
                    account.storage_reads.insert(slot);
                }
            }

            // Merge balance changes
            for balance_change in change.balance_changes {
                let tx_idx = balance_change.block_access_index as u16;
                account
                    .balance_changes
                    .insert(tx_idx, balance_change.post_balance);
            }

            // Merge nonce changes
            for nonce_change in change.nonce_changes {
                let tx_idx = nonce_change.block_access_index as u16;
                account.nonce_changes.insert(tx_idx, nonce_change.new_nonce);
            }

            // Merge code changes
            for code_change in change.code_changes {
                let tx_idx = code_change.block_access_index as u16;
                account.code_changes.insert(tx_idx, code_change.new_code);
            }
        }
    }
}
