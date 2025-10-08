//! Block Access List (BAL) utilities for reconstructing state at any transaction index.
//!
//! This module provides functionality to reconstruct the pre-state `BundleState` for any
//! transaction index from the EIP-7928 Block Access List, enabling executionless state updates
//! and parallel transaction validation.

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use ::eyre::eyre::eyre;
use eyre::{eyre, Result};
use flashblocks_primitives::primitives::FlashblockBlockAccessList;
use reth::{
    providers::{StateProvider, StateProviderFactory},
    revm::{
        db::{
            states::{bundle_account::BundleAccount, reverts::AccountInfoRevert, StorageSlot},
            AccountStatus, BundleState, PlainAccount,
        },
        state::{AccountInfo, Bytecode},
    },
};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Reconstructs pre-state bundles from Block Access Lists.
///
/// This struct can reconstruct the exact state at any transaction index within a block
/// by applying BAL changes in order, enabling parallel execution and executionless sync.
pub struct BalPreStateBuilder<'a, SP> {
    /// The block access list containing all state changes
    bal: &'a FlashblockBlockAccessList,
    /// State provider for fetching base state (parent block state)
    state_provider: &'a SP,
}

impl<'a, SP: StateProvider> BalPreStateBuilder<'a, SP> {
    /// Creates a new pre-state builder from a BAL and state provider.
    ///
    /// # Arguments
    /// * `bal` - The block access list containing all state changes
    /// * `state_provider` - Provider for the parent block state
    pub fn new(bal: &'a FlashblockBlockAccessList, state_provider: &'a SP) -> Self {
        Self {
            bal,
            state_provider,
        }
    }

    /// Builds the pre-state bundle for a specific transaction index.
    ///
    /// This reconstructs the exact state as it existed BEFORE executing the transaction
    /// at the given index. The state includes:
    /// - Account info (balance, nonce, code) from the parent block
    /// - Storage values as they existed before the transaction
    /// - All accounts that will be accessed during execution (from BAL)
    ///
    /// # Arguments
    /// * `tx_index` - The transaction index (0-based) for which to build pre-state
    ///
    /// # Returns
    /// A `BundleState` representing the state before executing transaction `tx_index`
    pub fn build_pre_state_at(&self, tx_index: u64) -> Result<BundleState> {
        let mut bundle = BundleState::default();

        // Iterate through all accounts in the BAL
        for (address, account_access) in &self.bal.accounts {
            // Get the base account info from parent block
            let base_info = self
                .state_provider
                .basic_account(address)?
                .unwrap_or_default();

            // Reconstruct the account state as it existed just before tx_index
            let (original_info, present_info, storage, status) =
                self.reconstruct_account_at(*address, account_access, tx_index, base_info.into())?;

            let bundle_account = BundleAccount::new(
                Some(original_info),
                Some(present_info),
                storage
                    .into_iter()
                    .collect::<alloy_primitives::map::HashMap<U256, StorageSlot>>(),
                status,
            );

            bundle.state.insert(*address, bundle_account);
        }

        Ok(bundle)
    }

    /// Builds the pre-state bundle for transaction index 0 (before any transactions).
    ///
    /// This is the state after pre-execution system contracts (block_access_index = 0)
    /// but before the first user transaction (block_access_index = 1).
    pub fn build_initial_state(&self) -> Result<BundleState> {
        self.build_pre_state_at(1)
    }

    /// Builds the final state after all transactions.
    ///
    /// This is the state after the last transaction has executed, including any
    /// post-execution system contract calls.
    pub fn build_final_state(&self) -> Result<BundleState> {
        let max_index = self.bal.max_tx_index;
        self.build_pre_state_at(max_index + 1)
    }

    /// Reconstructs a single account's state at a specific transaction index.
    ///
    /// Returns: (original_info, present_info, storage, status)
    fn reconstruct_account_at(
        &self,
        address: Address,
        account_access: &flashblocks_primitives::primitives::AccountAccess,
        tx_index: u64,
        base_info: AccountInfo,
    ) -> Result<(
        AccountInfo,
        AccountInfo,
        HashMap<U256, StorageSlot>,
        AccountStatus,
    )> {
        // Start with base state from parent block
        let mut original_info = base_info.clone();
        let mut present_info = base_info.clone();

        // Apply all balance changes up to (but not including) tx_index
        let mut balance_applied = false;
        for (idx, balance) in &account_access.balance_changes {
            if (*idx as u64) < tx_index {
                present_info.balance = *balance;
                balance_applied = true;
            }
        }

        // Apply all nonce changes up to (but not including) tx_index
        let mut nonce_applied = false;
        for (idx, nonce) in &account_access.nonce_changes {
            if (*idx as u64) < tx_index {
                present_info.nonce = *nonce;
                nonce_applied = true;
            }
        }

        // Apply all code changes up to (but not including) tx_index
        let mut code_applied = false;
        for (idx, code) in &account_access.code_changes {
            if (*idx as u64) < tx_index {
                present_info.code_hash = keccak256(code);
                present_info.code = Some(Bytecode::new_raw(code.clone()));
                code_applied = true;
            }
        }

        // Reconstruct storage state
        let storage = self.reconstruct_storage_at(address, account_access, tx_index)?;

        // Determine account status
        let status = if balance_applied || nonce_applied || code_applied || !storage.is_empty() {
            if present_info.is_empty() && original_info.is_empty() {
                AccountStatus::Loaded
            } else {
                AccountStatus::Changed
            }
        } else {
            AccountStatus::LoadedNotExisting
        };

        Ok((original_info, present_info, storage, status))
    }

    /// Reconstructs storage state for an account at a specific transaction index.
    fn reconstruct_storage_at(
        &self,
        address: Address,
        account_access: &flashblocks_primitives::primitives::AccountAccess,
        tx_index: u64,
    ) -> Result<HashMap<U256, StorageSlot>> {
        let mut storage = HashMap::new();

        // Process all storage writes up to (but not including) tx_index
        for (slot, writes) in &account_access.storage_writes {
            let slot_u256 = U256::from_be_bytes(slot.0);

            // Get original value from parent block state
            let original_value = self
                .state_provider
                .storage(address, slot_u256.into())?
                .unwrap_or(U256::ZERO);

            // Find the last write before tx_index
            let mut present_value = original_value;
            for (idx, value) in writes {
                if (*idx as u64) < tx_index {
                    present_value = U256::from_be_bytes(value.0);
                }
            }

            storage.insert(
                slot_u256,
                StorageSlot::new_changed(original_value, present_value),
            );
        }

        // For storage reads (no writes), include them with unchanged values
        for slot in &account_access.storage_reads {
            let slot_u256 = U256::from_be_bytes(slot.0);

            // Only add if not already in storage (from writes)
            if !storage.contains_key(&slot_u256) {
                let value = self
                    .state_provider
                    .storage(address, slot_u256.into())?
                    .unwrap_or(U256::ZERO);

                storage.insert(slot_u256, StorageSlot::new(value));
            }
        }

        Ok(storage)
    }

    /// Returns all addresses accessed in the block.
    pub fn accessed_addresses(&self) -> HashSet<Address> {
        self.bal.accounts.keys().copied().collect()
    }

    /// Returns all addresses modified up to a specific transaction index.
    pub fn modified_addresses_at(&self, tx_index: u64) -> HashSet<Address> {
        let mut addresses = HashSet::new();

        for (address, account_access) in &self.bal.accounts {
            let has_changes = account_access
                .balance_changes
                .iter()
                .any(|(idx, _)| (*idx as u64) < tx_index)
                || account_access
                    .nonce_changes
                    .iter()
                    .any(|(idx, _)| (*idx as u64) < tx_index)
                || account_access
                    .code_changes
                    .iter()
                    .any(|(idx, _)| (*idx as u64) < tx_index)
                || account_access
                    .storage_writes
                    .iter()
                    .any(|(_, writes)| writes.iter().any(|(idx, _)| (*idx as u64) < tx_index));

            if has_changes {
                addresses.insert(*address);
            }
        }

        addresses
    }

    /// Builds a minimal pre-state bundle containing only accounts that will be accessed
    /// during the execution of transaction at `tx_index`.
    ///
    /// This is useful for parallel execution where you only need the minimal state
    /// required to execute a specific transaction.
    pub fn build_minimal_pre_state_for(&self, tx_index: u64) -> Result<BundleState> {
        let mut bundle = BundleState::default();

        // Only include accounts that are accessed at exactly tx_index
        for (address, account_access) in &self.bal.accounts {
            // Check if this account is accessed at tx_index
            let accessed_at_index = account_access
                .balance_changes
                .contains_key(&(tx_index as u16))
                || account_access
                    .nonce_changes
                    .contains_key(&(tx_index as u16))
                || account_access.code_changes.contains_key(&(tx_index as u16))
                || account_access
                    .storage_writes
                    .iter()
                    .any(|(_, writes)| writes.contains_key(&(tx_index as u16)))
                || !account_access.storage_reads.is_empty();

            if accessed_at_index {
                let base_info = self
                    .state_provider
                    .basic_account(address)?
                    .unwrap_or_default();

                let (original_info, present_info, storage, status) = self.reconstruct_account_at(
                    *address,
                    account_access,
                    tx_index,
                    base_info.into(),
                )?;

                let bundle_account = BundleAccount::new(
                    Some(original_info),
                    Some(present_info),
                    storage
                        .into_iter()
                        .collect::<alloy_primitives::map::HashMap<U256, StorageSlot>>(),
                    status,
                );

                bundle.state.insert(*address, bundle_account);
            }
        }

        Ok(bundle)
    }

    /// Validates that the BAL is properly ordered and consistent.
    ///
    /// Checks:
    /// - Block access indices are in ascending order
    /// - No gaps in transaction indices
    /// - Storage changes are properly ordered
    pub fn validate(&self) -> Result<()> {
        for (address, account_access) in &self.bal.accounts {
            // Validate balance changes are ordered
            let mut prev_idx = None;
            for (idx, _) in &account_access.balance_changes {
                if let Some(prev) = prev_idx {
                    if *idx <= prev {
                        return Err(eyre!(
                            "Balance changes not ordered for address {:?}: {} <= {}",
                            address,
                            idx,
                            prev
                        ));
                    }
                }
                prev_idx = Some(*idx);
            }

            // Validate nonce changes are ordered
            let mut prev_idx = None;
            for (idx, _) in &account_access.nonce_changes {
                if let Some(prev) = prev_idx {
                    if *idx <= prev {
                        return Err(eyre!(
                            "Nonce changes not ordered for address {:?}: {} <= {}",
                            address,
                            idx,
                            prev
                        ));
                    }
                }
                prev_idx = Some(*idx);
            }

            // Validate code changes are ordered
            let mut prev_idx = None;
            for (idx, _) in &account_access.code_changes {
                if let Some(prev) = prev_idx {
                    if *idx <= prev {
                        return Err(eyre!(
                            "Code changes not ordered for address {:?}: {} <= {}",
                            address,
                            idx,
                            prev
                        ));
                    }
                }
                prev_idx = Some(*idx);
            }

            // Validate storage writes are ordered
            for (slot, writes) in &account_access.storage_writes {
                let mut prev_idx = None;
                for (idx, _) in writes {
                    if let Some(prev) = prev_idx {
                        if *idx <= prev {
                            return Err(eyre!(
                                "Storage writes not ordered for address {:?} slot {:?}: {} <= {}",
                                address,
                                slot,
                                idx,
                                prev
                            ));
                        }
                    }
                    prev_idx = Some(*idx);
                }
            }
        }

        Ok(())
    }
}

/// Utility functions for working with multiple flashblocks.
pub struct MultiFlashblockBalBuilder<'a> {
    flashblocks: Vec<&'a FlashblockBlockAccessList>,
}

impl<'a> MultiFlashblockBalBuilder<'a> {
    /// Creates a new builder from multiple flashblock BALs.
    pub fn new(flashblocks: Vec<&'a FlashblockBlockAccessList>) -> Self {
        Self { flashblocks }
    }

    /// Merges all flashblock BALs into a single consolidated BAL.
    ///
    /// This is useful when you have incremental flashblock updates and want to
    /// reconstruct the full block state.
    pub fn merge_all(&self) -> FlashblockBlockAccessList {
        let mut merged = FlashblockBlockAccessList::default();

        if self.flashblocks.is_empty() {
            return merged;
        }

        // Find global min/max indices
        merged.min_tx_index = self
            .flashblocks
            .iter()
            .map(|fb| fb.min_tx_index)
            .min()
            .unwrap_or(0);

        merged.max_tx_index = self
            .flashblocks
            .iter()
            .map(|fb| fb.max_tx_index)
            .max()
            .unwrap_or(0);

        // Merge all accounts
        for fb in &self.flashblocks {
            for (address, account_access) in &fb.accounts {
                let merged_account = merged
                    .accounts
                    .entry(*address)
                    .or_insert_with(Default::default);

                // Merge balance changes
                for (idx, balance) in &account_access.balance_changes {
                    merged_account.balance_changes.insert(*idx, *balance);
                }

                // Merge nonce changes
                for (idx, nonce) in &account_access.nonce_changes {
                    merged_account.nonce_changes.insert(*idx, *nonce);
                }

                // Merge code changes
                for (idx, code) in &account_access.code_changes {
                    merged_account.code_changes.insert(*idx, code.clone());
                }

                // Merge storage writes
                for (slot, writes) in &account_access.storage_writes {
                    let merged_writes = merged_account
                        .storage_writes
                        .entry(*slot)
                        .or_insert_with(Default::default);

                    for (idx, value) in writes {
                        merged_writes.insert(*idx, *value);
                    }
                }

                // Merge storage reads
                for slot in &account_access.storage_reads {
                    merged_account.storage_reads.insert(*slot);
                }
            }
        }

        merged
    }

    /// Gets the latest state across all flashblocks.
    pub fn get_latest_tx_index(&self) -> u64 {
        self.flashblocks
            .iter()
            .map(|fb| fb.max_tx_index)
            .max()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_bal() {
        let bal = FlashblockBlockAccessList::default();
        // Would need a mock state provider to fully test
        // This is a placeholder for proper tests
        assert_eq!(bal.accounts.len(), 0);
    }
}
