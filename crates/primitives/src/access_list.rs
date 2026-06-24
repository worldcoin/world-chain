use alloy_eip7928::{AccountChanges, BlockAccessList};
use alloy_primitives::B256;
use alloy_rlp::{RlpDecodable, RlpEncodable};

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
}

/// Computes the Keccak256 hash of the RLP-Encoding of a [`FlashblockAccessList`]
pub fn access_list_hash(access_list: &FlashblockAccessList) -> B256 {
    let rlp_encoded = alloy_rlp::encode(access_list);
    alloy_primitives::keccak256(&rlp_encoded)
}
