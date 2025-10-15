use alloy_eip7928::AccountChanges;
use alloy_primitives::map::foldhash::HashMap as AlloyHashMap;
use alloy_primitives::{keccak256, Address, U256};
use alloy_rlp::{RlpDecodable, RlpEncodable};
use reth::revm::{
    db::{states::StorageSlot, AccountStatus, BundleAccount},
    state::{AccountInfo, Bytecode},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(
    Clone, Debug, PartialEq, Default, Deserialize, Serialize, Eq, RlpEncodable, RlpDecodable,
)]
pub struct FlashblockAccessList {
    pub changes: Vec<AccountChanges>,
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
