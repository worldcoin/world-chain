use alloy_eip7928::{
    AccountChanges, BalanceChange, CodeChange, NonceChange, SlotChanges, StorageChange,
};
use alloy_primitives::{Address, U256};
use dashmap::DashMap;
use flashblocks_primitives::access_list::FlashblockAccessList;
use rayon::prelude::*;

use revm::state::Bytecode;
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
    pub fn build(self, (min_tx_index, max_tx_index): (u16, u16)) -> FlashblockAccessList {
        // Sort addresses lexicographically
        let mut changes: Vec<_> = self
            .changes
            .into_par_iter()
            .map(|(k, v)| v.build(k))
            .collect();

        changes.par_sort_unstable_by_key(|a| a.address);

        FlashblockAccessList {
            changes,
            min_tx_index,
            max_tx_index,
        }
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
            let tx_map = self.storage_changes.entry(slot).or_default();
            for (tx_index, value) in other_tx_map {
                tx_map.insert(tx_index, value);
            }
        }
        self.storage_reads.extend(other.storage_reads);
        
        for (tx_index, value) in other.balance_changes {
            self.balance_changes.insert(tx_index, value);
        }
        for (tx_index, value) in other.nonce_changes {
            self.nonce_changes.insert(tx_index, value);
        }
        for (tx_index, value) in other.code_changes {
            self.code_changes.insert(tx_index, value);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Strategy for generating random U256 values
    fn arb_u256() -> impl Strategy<Value = U256> {
        any::<[u8; 32]>().prop_map(|bytes| U256::from_be_bytes(bytes))
    }

    /// Strategy for generating random addresses
    fn arb_address() -> impl Strategy<Value = Address> {
        any::<[u8; 20]>().prop_map(Address::from)
    }

    /// Strategy for generating random block access indices (0..1000)
    fn arb_block_access_index() -> impl Strategy<Value = BlockAccessIndex> {
        0u16..1000u16
    }

    /// Strategy for generating storage changes: HashMap<U256, HashMap<BlockAccessIndex, U256>>
    fn arb_storage_changes() -> impl Strategy<Value = HashMap<U256, HashMap<BlockAccessIndex, U256>>>
    {
        prop::collection::hash_map(
            arb_u256(),
            prop::collection::hash_map(arb_block_access_index(), arb_u256(), 0..5),
            0..10,
        )
    }

    /// Strategy for generating storage reads
    fn arb_storage_reads() -> impl Strategy<Value = HashSet<U256>> {
        prop::collection::hash_set(arb_u256(), 0..10)
    }

    /// Strategy for generating balance changes
    fn arb_balance_changes() -> impl Strategy<Value = HashMap<BlockAccessIndex, U256>> {
        prop::collection::hash_map(arb_block_access_index(), arb_u256(), 0..5)
    }

    /// Strategy for generating nonce changes
    fn arb_nonce_changes() -> impl Strategy<Value = HashMap<BlockAccessIndex, u64>> {
        prop::collection::hash_map(arb_block_access_index(), any::<u64>(), 0..5)
    }

    /// Strategy for generating AccountChangesConstruction
    fn arb_account_changes_construction() -> impl Strategy<Value = AccountChangesConstruction> {
        (
            arb_storage_changes(),
            arb_storage_reads(),
            arb_balance_changes(),
            arb_nonce_changes(),
        )
            .prop_map(
                |(storage_changes, storage_reads, balance_changes, nonce_changes)| {
                    AccountChangesConstruction {
                        storage_changes,
                        storage_reads,
                        balance_changes,
                        nonce_changes,
                        code_changes: HashMap::new(), // Skip code changes for simplicity
                    }
                },
            )
    }

    /// Strategy for generating FlashblockAccessListConstruction with multiple addresses
    fn arb_access_list_construction() -> impl Strategy<Value = FlashblockAccessListConstruction> {
        prop::collection::vec((arb_address(), arb_account_changes_construction()), 0..10).prop_map(
            |entries| {
                let construction = FlashblockAccessListConstruction::new();
                for (addr, changes) in entries {
                    construction.changes.insert(addr, changes);
                }
                construction
            },
        )
    }

    /// Strategy for generating index ranges
    fn arb_index_range() -> impl Strategy<Value = (u16, u16)> {
        (0u16..100u16).prop_flat_map(|min| (Just(min), min..min.saturating_add(100)))
    }

    proptest! {
        /// Merging with an empty AccountChangesConstruction should be identity
        #[test]
        fn prop_account_merge_with_empty_is_identity(
            acc in arb_account_changes_construction()
        ) {
            let original = acc.clone();
            let mut merged = acc;
            merged.merge(AccountChangesConstruction::default());

            prop_assert_eq!(merged.storage_changes, original.storage_changes);
            prop_assert_eq!(merged.storage_reads, original.storage_reads);
            prop_assert_eq!(merged.balance_changes, original.balance_changes);
            prop_assert_eq!(merged.nonce_changes, original.nonce_changes);
        }

        /// Merging two AccountChangesConstruction should combine all storage slots
        #[test]
        fn prop_account_merge_combines_storage_slots(
            acc1 in arb_account_changes_construction(),
            acc2 in arb_account_changes_construction()
        ) {
            let mut merged = acc1.clone();
            merged.merge(acc2.clone());

            // All storage slots from both should be present
            for slot in acc1.storage_changes.keys() {
                prop_assert!(merged.storage_changes.contains_key(slot));
            }
            for slot in acc2.storage_changes.keys() {
                prop_assert!(merged.storage_changes.contains_key(slot));
            }
        }

        /// Merging should combine all storage reads
        #[test]
        fn prop_account_merge_combines_storage_reads(
            acc1 in arb_account_changes_construction(),
            acc2 in arb_account_changes_construction()
        ) {
            let mut merged = acc1.clone();
            merged.merge(acc2.clone());

            // All storage reads from both should be present
            for slot in &acc1.storage_reads {
                prop_assert!(merged.storage_reads.contains(slot));
            }
            for slot in &acc2.storage_reads {
                prop_assert!(merged.storage_reads.contains(slot));
            }
        }

        /// Building AccountChanges should sort storage slots
        #[test]
        fn prop_account_build_sorts_storage_slots(
            acc in arb_account_changes_construction(),
            addr in arb_address()
        ) {
            let built = acc.build(addr);

            // Verify storage changes are sorted by slot
            let slots: Vec<_> = built.storage_changes.iter().map(|s| s.slot).collect();
            let mut sorted_slots = slots.clone();
            sorted_slots.sort();
            prop_assert_eq!(slots, sorted_slots, "Storage slots should be sorted");
        }

        /// Building AccountChanges should sort changes within each slot by tx index
        #[test]
        fn prop_account_build_sorts_changes_by_tx_index(
            acc in arb_account_changes_construction(),
            addr in arb_address()
        ) {
            let built = acc.build(addr);

            // Verify each slot's changes are sorted by block_access_index
            for slot_changes in &built.storage_changes {
                let indices: Vec<_> = slot_changes.changes.iter()
                    .map(|c| c.block_access_index)
                    .collect();
                let mut sorted_indices = indices.clone();
                sorted_indices.sort();
                prop_assert_eq!(indices, sorted_indices,
                    "Storage changes within slot should be sorted by tx index");
            }
        }

        /// Building AccountChanges should sort balance changes by tx index
        #[test]
        fn prop_account_build_sorts_balance_changes(
            acc in arb_account_changes_construction(),
            addr in arb_address()
        ) {
            let built = acc.build(addr);

            let indices: Vec<_> = built.balance_changes.iter()
                .map(|c| c.block_access_index)
                .collect();
            let mut sorted_indices = indices.clone();
            sorted_indices.sort();
            prop_assert_eq!(indices, sorted_indices,
                "Balance changes should be sorted by tx index");
        }

        /// Building AccountChanges should sort nonce changes by tx index
        #[test]
        fn prop_account_build_sorts_nonce_changes(
            acc in arb_account_changes_construction(),
            addr in arb_address()
        ) {
            let built = acc.build(addr);

            let indices: Vec<_> = built.nonce_changes.iter()
                .map(|c| c.block_access_index)
                .collect();
            let mut sorted_indices = indices.clone();
            sorted_indices.sort();
            prop_assert_eq!(indices, sorted_indices,
                "Nonce changes should be sorted by tx index");
        }

        /// Building AccountChanges should sort storage reads
        #[test]
        fn prop_account_build_sorts_storage_reads(
            acc in arb_account_changes_construction(),
            addr in arb_address()
        ) {
            let built = acc.build(addr);

            let reads: Vec<_> = built.storage_reads.clone();
            let mut sorted_reads = reads.clone();
            sorted_reads.sort();
            prop_assert_eq!(reads, sorted_reads, "Storage reads should be sorted");
        }
    }

    proptest! {
        /// Merging with empty FlashblockAccessListConstruction should be identity
        #[test]
        fn prop_bal_merge_with_empty_is_identity(
            bal in arb_access_list_construction()
        ) {
            let original_addrs: HashSet<_> = bal.changes.iter()
                .map(|e| *e.key())
                .collect();

            let mut merged = bal;
            merged.merge(FlashblockAccessListConstruction::new());

            let merged_addrs: HashSet<_> = merged.changes.iter()
                .map(|e| *e.key())
                .collect();

            prop_assert_eq!(original_addrs, merged_addrs);
        }

        /// Merging two FlashblockAccessListConstruction should combine all addresses
        #[test]
        fn prop_bal_merge_combines_addresses(
            bal1 in arb_access_list_construction(),
            bal2 in arb_access_list_construction()
        ) {
            let addrs1: HashSet<_> = bal1.changes.iter().map(|e| *e.key()).collect();
            let addrs2: HashSet<_> = bal2.changes.iter().map(|e| *e.key()).collect();

            let mut merged = bal1;
            merged.merge(bal2);

            // All addresses from both should be present
            for addr in &addrs1 {
                prop_assert!(merged.changes.contains_key(addr));
            }
            for addr in &addrs2 {
                prop_assert!(merged.changes.contains_key(addr));
            }
        }

        /// Building FlashblockAccessList should sort addresses lexicographically
        #[test]
        fn prop_bal_build_sorts_addresses(
            bal in arb_access_list_construction(),
            range in arb_index_range()
        ) {
            let built = bal.build(range);

            let addrs: Vec<_> = built.changes.iter().map(|c| c.address).collect();
            let mut sorted_addrs = addrs.clone();
            sorted_addrs.sort();
            prop_assert_eq!(addrs, sorted_addrs, "Addresses should be sorted lexicographically");
        }

        /// Building FlashblockAccessList should preserve index range
        #[test]
        fn prop_bal_build_preserves_index_range(
            bal in arb_access_list_construction(),
            range in arb_index_range()
        ) {
            let built = bal.build(range);

            prop_assert_eq!(built.min_tx_index, range.0);
            prop_assert_eq!(built.max_tx_index, range.1);
        }

        /// Building FlashblockAccessList should preserve address count
        #[test]
        fn prop_bal_build_preserves_address_count(
            bal in arb_access_list_construction(),
            range in arb_index_range()
        ) {
            let original_count = bal.changes.len();
            let built = bal.build(range);

            prop_assert_eq!(built.changes.len(), original_count);
        }
    }


    #[test]
    fn test_empty_account_changes_is_empty() {
        let acc = AccountChangesConstruction::default();
        assert!(acc.is_empty());
    }

    #[test]
    fn test_account_changes_with_storage_not_empty() {
        let mut acc = AccountChangesConstruction::default();
        acc.storage_changes
            .insert(U256::from(1), HashMap::from([(0u16, U256::from(100))]));
        assert!(!acc.is_empty());
    }

    #[test]
    fn test_account_changes_with_balance_not_empty() {
        let mut acc = AccountChangesConstruction::default();
        acc.balance_changes.insert(0, U256::from(100));
        assert!(!acc.is_empty());
    }

    #[test]
    fn test_bal_construction_map_account_change() {
        let bal = FlashblockAccessListConstruction::new();
        let addr = Address::ZERO;

        bal.map_account_change(addr, |acc| {
            acc.balance_changes.insert(0, U256::from(100));
        });

        assert!(bal.changes.contains_key(&addr));
        let acc = bal.changes.get(&addr).unwrap();
        assert_eq!(acc.balance_changes.get(&0), Some(&U256::from(100)));
    }

    #[test]
    fn test_bal_merge_overlapping_address() {
        let mut bal1 = FlashblockAccessListConstruction::new();
        let bal2 = FlashblockAccessListConstruction::new();
        let addr = Address::ZERO;

        // bal1 has balance change at index 0
        bal1.map_account_change(addr, |acc| {
            acc.balance_changes.insert(0, U256::from(100));
        });

        // bal2 has balance change at index 1
        bal2.map_account_change(addr, |acc| {
            acc.balance_changes.insert(1, U256::from(200));
        });

        bal1.merge(bal2);

        let acc = bal1.changes.get(&addr).unwrap();
        assert_eq!(acc.balance_changes.len(), 2);
        assert_eq!(acc.balance_changes.get(&0), Some(&U256::from(100)));
        assert_eq!(acc.balance_changes.get(&1), Some(&U256::from(200)));
    }
}
