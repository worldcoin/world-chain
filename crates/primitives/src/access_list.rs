use alloy_eip7928::{AccountChanges, BlockAccessList};
use alloy_primitives::{B256, keccak256};
use alloy_rlp::{RlpDecodable, RlpEncodable};
use reth_primitives_traits::Account;
use reth_storage_api::{StateProvider, errors::provider::ProviderResult};
use reth_trie_common::{HashedPostState, HashedStorage};

use serde::{Deserialize, Serialize};

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
    /// Creates a flashblock access list sidecar from an upstream block access list.
    pub fn from_block_access_list(
        changes: BlockAccessList,
        (min_tx_index, max_tx_index): (u64, u64),
    ) -> Self {
        Self {
            changes,
            min_tx_index,
            max_tx_index,
        }
    }

    /// Returns the upstream block access list contents carried by this flashblock sidecar.
    pub fn as_block_access_list(&self) -> BlockAccessList {
        self.changes.clone()
    }

    /// Consumes the flashblock sidecar and returns the upstream block access list contents.
    pub fn into_block_access_list(self) -> BlockAccessList {
        self.changes
    }

    /// Extends `hashed_post_state` (the already-committed post-state) with the changes recorded in
    /// this access list, producing the cumulative hashed post-state for the block.
    ///
    /// This derives the post-state directly from the *claimed* access list, so the block state root
    /// can be computed without re-executing transactions. The access list is a sparse diff — it
    /// only records the fields that changed — so any field it omits is read from `state_provider`
    /// (the parent state) to complete the account leaf. The latest recorded change per field wins
    /// (changes are not guaranteed pre-sorted, so selection is by block access index, not position).
    ///
    /// Accounts that only appear as storage reads are skipped, and accounts that end empty
    /// (EIP-158) are inserted as destroyed so their trie leaf and storage are removed.
    pub fn into_hashed_post_state(
        self,
        state_provider: &(impl StateProvider + ?Sized),
        mut hashed_post_state: HashedPostState,
    ) -> ProviderResult<HashedPostState> {
        for changes in &self.changes {
            // Read the parent account only when the access list omits a field we need to complete
            // the leaf. Pure storage reads carry no field changes and need no parent lookup.
            let pre = if changes.balance_changes.is_empty()
                || changes.nonce_changes.is_empty()
                || changes.code_changes.is_empty()
            {
                state_provider.basic_account(&changes.address)?
            } else {
                None
            };

            let Some(account) = account_post_state(changes, pre) else {
                continue;
            };

            let hashed_address = keccak256(changes.address);
            // An account that ends empty is pruned from the trie (EIP-158): record it as destroyed
            // so the leaf is removed (`None`) and its storage is wiped.
            let destroyed = account.is_empty();
            hashed_post_state
                .accounts
                .insert(hashed_address, (!destroyed).then_some(account));

            // `wiped` is carried on the storage itself: a destroyed account wipes any committed
            // storage when merged below.
            let mut storage = HashedStorage::new(destroyed);
            for (slot, value) in changes.storage_post_states() {
                storage.storage.insert(keccak256(B256::from(slot)), value);
            }
            // Skip live accounts with no storage writes (matches `from_bundle_state`, which omits
            // empty storages); otherwise merge onto any committed storage, with the new entries
            // winning.
            if !storage.is_empty() {
                hashed_post_state
                    .storages
                    .entry(hashed_address)
                    .and_modify(|committed| committed.extend(&storage))
                    .or_insert(storage);
            }
        }

        Ok(hashed_post_state)
    }
}

/// Reconstructs the post-state [`Account`] for a single account from its recorded access-list
/// changes, falling back to `pre` (the parent-block account) for any field the access list omits.
///
/// The latest recorded change per field wins (changes are not guaranteed pre-sorted, so selection
/// is by block access index, not position). Returns `None` for accounts that only appear as storage
/// reads, which carry no post-state change.
fn account_post_state(changes: &AccountChanges, pre: Option<Account>) -> Option<Account> {
    if changes.storage_changes.is_empty()
        && changes.balance_changes.is_empty()
        && changes.nonce_changes.is_empty()
        && changes.code_changes.is_empty()
    {
        return None;
    }

    let balance = changes
        .balance_changes()
        .iter()
        .max_by_key(|change| change.block_access_index.0)
        .map(|change| change.post_balance);
    let nonce = changes
        .nonce_changes()
        .iter()
        .max_by_key(|change| change.block_access_index.0)
        .map(|change| change.new_nonce);
    let bytecode_hash = changes
        .code_changes()
        .iter()
        .max_by_key(|change| change.block_access_index.0)
        .map(|change| keccak256(&change.new_code));

    Some(Account {
        balance: balance
            .or(pre.map(|account| account.balance))
            .unwrap_or_default(),
        nonce: nonce
            .or(pre.map(|account| account.nonce))
            .unwrap_or_default(),
        bytecode_hash: bytecode_hash.or(pre.and_then(|account| account.bytecode_hash)),
    })
}

/// Computes the Keccak256 hash of the RLP-Encoding of a [`FlashblockAccessList`]
pub fn access_list_hash(access_list: &FlashblockAccessList) -> B256 {
    let rlp_encoded = alloy_rlp::encode(access_list);
    alloy_primitives::keccak256(&rlp_encoded)
}

#[cfg(test)]
mod tests {
    use super::account_post_state;
    use alloy_consensus::constants::KECCAK_EMPTY;
    use alloy_eip7928::{
        AccountChanges, BalanceChange, BlockAccessIndex, CodeChange, NonceChange, SlotChanges,
        StorageChange,
    };
    use alloy_primitives::{Address, Bytes, U256, keccak256};
    use reth_primitives_traits::Account;

    fn addr() -> Address {
        Address::with_last_byte(0x42)
    }

    #[test]
    fn pure_read_returns_none() {
        // An account that only appears as a storage read produces no post-state change.
        let changes = AccountChanges::new(addr()).with_storage_read(U256::from(1));
        assert!(account_post_state(&changes, None).is_none());
    }

    #[test]
    fn storage_only_change_inherits_prestate_account_info() {
        let pre = Account {
            nonce: 7,
            balance: U256::from(1000),
            bytecode_hash: Some(keccak256([0x60, 0x00])),
        };
        let changes = AccountChanges::new(addr()).with_storage_change(SlotChanges::new(
            U256::from(3),
            vec![StorageChange::new(
                BlockAccessIndex::new(1),
                U256::from(0xbeef),
            )],
        ));

        let account = account_post_state(&changes, Some(pre)).expect("changed account");
        // Unchanged fields come from the parent account.
        assert_eq!(account.nonce, 7);
        assert_eq!(account.balance, U256::from(1000));
        assert_eq!(account.bytecode_hash, pre.bytecode_hash);
        assert!(!account.is_empty());
    }

    #[test]
    fn latest_balance_and_nonce_change_wins_regardless_of_order() {
        // Changes are not guaranteed sorted; selection is by block access index, not position.
        let changes = AccountChanges::new(addr())
            .with_balance_change(BalanceChange::new(BlockAccessIndex::new(5), U256::from(70)))
            .with_balance_change(BalanceChange::new(BlockAccessIndex::new(2), U256::from(30)))
            .with_nonce_change(NonceChange::new(BlockAccessIndex::new(1), 1))
            .with_nonce_change(NonceChange::new(BlockAccessIndex::new(9), 4));

        let account = account_post_state(&changes, None).expect("changed account");
        assert_eq!(account.balance, U256::from(70));
        assert_eq!(account.nonce, 4);
    }

    #[test]
    fn code_change_sets_keccak_of_new_code() {
        let code = Bytes::from_static(&[0x60, 0x01, 0x60, 0x02]);
        let changes = AccountChanges::new(addr())
            .with_nonce_change(NonceChange::new(BlockAccessIndex::new(1), 1))
            .with_code_change(CodeChange::new(BlockAccessIndex::new(1), code.clone()));

        let account = account_post_state(&changes, None).expect("changed account");
        assert_eq!(account.bytecode_hash, Some(keccak256(&code)));
        assert!(!account.is_empty());
    }

    #[test]
    fn account_that_ends_empty_is_reported_empty() {
        // A balance drained to zero with no nonce/code leaves an empty account (EIP-158). The
        // caller encodes such accounts as destroyed so the trie leaf is removed.
        let changes = AccountChanges::new(addr())
            .with_balance_change(BalanceChange::new(BlockAccessIndex::new(1), U256::ZERO));

        let account = account_post_state(&changes, None).expect("changed account");
        assert!(account.is_empty());
        assert_eq!(account.bytecode_hash, None);
    }

    #[test]
    fn created_contract_without_prestate_is_kept() {
        // Newly deployed contract: no parent account, code change makes it non-empty.
        let code = Bytes::from_static(&[0xfe]);
        let changes = AccountChanges::new(addr())
            .with_code_change(CodeChange::new(BlockAccessIndex::new(1), code.clone()));

        let account = account_post_state(&changes, None).expect("created contract");
        assert_eq!(account.bytecode_hash, Some(keccak256(&code)));
        assert_ne!(account.bytecode_hash, Some(KECCAK_EMPTY));
        assert!(!account.is_empty());
    }
}
