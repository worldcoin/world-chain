use alloy_eip7928::{
    AccountChanges, BalanceChange, BlockAccessIndex, CodeChange, NonceChange, SlotChanges,
    StorageChange,
};
use alloy_primitives::{Address, Bytes, U256};

use crate::access_list::{FlashblockAccessList, FlashblockAccessListData, access_list_hash};

pub(crate) fn decode_hex(s: &str) -> Vec<u8> {
    alloy_primitives::hex::decode(s).expect("valid hex fixture")
}

pub(crate) fn sample_access_list_data() -> FlashblockAccessListData {
    let access_list = sample_access_list();

    FlashblockAccessListData {
        access_list_hash: access_list_hash(&access_list),
        access_list,
    }
}

pub(crate) fn sample_access_list() -> FlashblockAccessList {
    FlashblockAccessList {
        changes: vec![AccountChanges {
            address: Address::with_last_byte(0x42),
            storage_changes: vec![SlotChanges {
                slot: U256::from(0x2),
                changes: vec![StorageChange {
                    block_access_index: BlockAccessIndex::new(1),
                    new_value: U256::from(0x3),
                }],
            }],
            storage_reads: vec![U256::from(0x4)],
            balance_changes: vec![BalanceChange {
                block_access_index: BlockAccessIndex::new(3),
                post_balance: U256::from(1_000_000u64),
            }],
            nonce_changes: vec![NonceChange {
                block_access_index: BlockAccessIndex::new(4),
                new_nonce: 42,
            }],
            code_changes: vec![CodeChange {
                block_access_index: BlockAccessIndex::new(2),
                new_code: Bytes::from_static(b"\xCA\xFE"),
            }],
        }],
        min_tx_index: 0,
        max_tx_index: 5,
    }
}
