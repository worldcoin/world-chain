use std::collections::HashMap;

use alloy_eip7928::{
    AccountChanges, BalanceChange, CodeChange, NonceChange, SlotChanges, StorageChange,
};
use alloy_primitives::{map::HashSet, Address, Bloom, Bytes, FixedBytes, B256, B64, U256};
use alloy_rlp::{
    Decodable, Encodable, Header, RlpDecodable, RlpDecodableWrapper, RlpEncodable,
    RlpEncodableWrapper,
};
use alloy_rpc_types_engine::PayloadId;
use alloy_rpc_types_eth::{AccountInfo, Withdrawal};
use reth::revm::{
    db::{states::bundle_state::BundleRetention, BundleAccount, BundleState},
    state::Account,
    Database, State,
};
use reth_trie::{
    iter::{IntoParallelIterator, ParallelIterator},
    HashedStorage, KeyHasher, TrieAccount,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::flashblocks::FlashblockMetadata;

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
        }).collect();

        Self {
            min_tx_index: fbal.min_tx_index,
            max_tx_index: fbal.max_tx_index,
            fbal_accumulator: fbal.fbal_accumulator,
            accounts,
        }
    }
}

impl From<FlashblockBlockAccessListWire> for FlashblockBlockAccessList {
    fn from(fbal: FlashblockBlockAccessListWire) -> Self {
        let accounts = fbal
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
                        (nonce_change.block_access_index as u16, nonce_change.new_nonce)
                    })
                    .collect();

                let code_changes = change
                    .code_changes
                    .into_iter()
                    .map(|code_change| {
                        (code_change.block_access_index as u16, code_change.new_code)
                    })
                    .collect();

                (change.address, AccountAccess {
                    storage_writes,
                    storage_reads,
                    balance_changes,
                    nonce_changes,
                    code_changes,
                })
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

impl FlashblockBlockAccessList {
    /// NewConstructionBlockAccessList instantiates an empty access list.
    pub fn new() -> Self {
        Self::default()
    }

    /// AccountRead records the address of an account that has been read during execution.
    pub fn account_read(&mut self, addr: Address) {
        self.accounts.entry(addr).or_insert_with(AccountAccess::new);
    }

    /// StorageRead records a storage key read during execution.
    pub fn storage_read(&mut self, address: Address, key: B256) {
        let account = self
            .accounts
            .entry(address)
            .or_insert_with(AccountAccess::new);
        if account.storage_writes.contains_key(&key) {
            return;
        }
        account.storage_reads.insert(key);
    }

    /// StorageWrite records the post-transaction value of a mutated storage slot.
    /// The storage slot is removed from the list of read slots.
    pub fn storage_write(&mut self, tx_idx: u16, address: Address, key: B256, value: B256) {
        let account = self
            .accounts
            .entry(address)
            .or_insert_with(AccountAccess::new);

        account
            .storage_writes
            .entry(key)
            .or_insert_with(HashMap::new)
            .insert(tx_idx, value);

        account.storage_reads.remove(&key);
    }

    /// CodeChange records the code of a newly-created contract.
    pub fn code_change(&mut self, address: Address, tx_index: u16, code: Bytes) {
        let account = self
            .accounts
            .entry(address)
            .or_insert_with(AccountAccess::new);
        account.code_changes.insert(tx_index, code);
    }

    /// NonceChange records tx post-state nonce of any contract-like accounts whose
    /// nonce was incremented.
    pub fn nonce_change(&mut self, address: Address, tx_idx: u16, post_nonce: u64) {
        let account = self
            .accounts
            .entry(address)
            .or_insert_with(AccountAccess::new);
        account.nonce_changes.insert(tx_idx, post_nonce);
    }

    /// BalanceChange records the post-transaction balance of an account whose
    /// balance changed.
    pub fn balance_change(&mut self, tx_idx: u16, address: Address, balance: U256) {
        let account = self
            .accounts
            .entry(address)
            .or_insert_with(AccountAccess::new);
        account.balance_changes.insert(tx_idx, balance);
    }

    /// ApplyAccesses records the given account/storage accesses in the BAL.
    pub fn apply_accesses(&mut self, accesses: &StateAccesses) {
        for (addr, acct_accesses) in accesses {
            let account = self
                .accounts
                .entry(*addr)
                .or_insert_with(AccountAccess::new);

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
            let account = self
                .accounts
                .entry(change.address)
                .or_insert_with(AccountAccess::new);

            // Merge storage writes
            for slot_change in change.storage_changes {
                for storage_change in slot_change.changes {
                    let tx_idx = storage_change.block_access_index as u16;
                    account
                        .storage_writes
                        .entry(slot_change.slot)
                        .or_insert_with(HashMap::new)
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

pub type StateAccesses = HashMap<Address, HashMap<B256, ()>>;

pub trait StateAccessesMerge {
    fn merge(&mut self, other: &StateAccesses);
}

impl StateAccessesMerge for StateAccesses {
    fn merge(&mut self, other: &StateAccesses) {
        for (addr, accesses) in other {
            let account_accesses = self.entry(*addr).or_insert_with(HashMap::new);
            for slot in accesses.keys() {
                account_accesses.insert(*slot, ());
            }
        }
    }
}

pub type BlockAccessList = Vec<AccountAccess>;

/// Represents the modified portions of an execution payload within a flashblock.
/// This structure contains only the fields that can be updated during block construction,
/// such as state root, receipts, logs, and new transactions. Other immutable block fields
/// like parent hash and block number are excluded since they remain constant throughout
/// the block's construction.
#[derive(
    Clone, Debug, PartialEq, Default, Deserialize, Serialize, Eq, RlpDecodable, RlpEncodable,
)]
pub struct ExecutionPayloadFlashblockDeltaV1 {
    /// The state root of the block.
    pub state_root: B256,
    /// The receipts root of the block.
    pub receipts_root: B256,
    /// The logs bloom of the block.
    pub logs_bloom: Bloom,
    /// The gas used of the block.
    #[serde(with = "alloy_serde::quantity")]
    pub gas_used: u64,
    /// The block hash of the block.
    pub block_hash: B256,
    /// The transactions of the block.
    pub transactions: Vec<Bytes>,
    /// Array of [`Withdrawal`] enabled with V2
    pub withdrawals: Vec<Withdrawal>,
    /// The withdrawals root of the block.
    pub withdrawals_root: B256,
    /// The access list of the diff.
    pub flash_bal: FlashblockBlockAccessListWire,
    /// The hash of the access list of the diff.
    pub flash_bal_hash: B256,
}

/// Represents the base configuration of an execution payload that remains constant
/// throughout block construction. This includes fundamental block properties like
/// parent hash, block number, and other header fields that are determined at
/// block creation and cannot be modified.
#[derive(
    Clone, Debug, PartialEq, Default, Deserialize, Serialize, Eq, RlpEncodable, RlpDecodable,
)]
pub struct ExecutionPayloadBaseV1 {
    /// Ecotone parent beacon block root
    pub parent_beacon_block_root: B256,
    /// The parent hash of the block.
    pub parent_hash: B256,
    /// The fee recipient of the block.
    pub fee_recipient: Address,
    /// The previous randao of the block.
    pub prev_randao: B256,
    /// The block number.
    #[serde(with = "alloy_serde::quantity")]
    pub block_number: u64,
    /// The gas limit of the block.
    #[serde(with = "alloy_serde::quantity")]
    pub gas_limit: u64,
    /// The timestamp of the block.
    #[serde(with = "alloy_serde::quantity")]
    pub timestamp: u64,
    /// The extra data of the block.
    pub extra_data: Bytes,
    /// The base fee per gas of the block.
    pub base_fee_per_gas: U256,
}

#[derive(Clone, Debug, PartialEq, Default, Deserialize, Serialize, Eq)]
pub struct FlashblocksPayloadV1<M = FlashblockMetadata> {
    /// The payload id of the flashblock
    pub payload_id: PayloadId,
    /// The index of the flashblock in the block
    pub index: u64,
    /// The delta/diff containing modified portions of the execution payload
    pub diff: ExecutionPayloadFlashblockDeltaV1,
    /// Additional metadata associated with the flashblock
    pub metadata: M,
    /// The base execution payload configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<ExecutionPayloadBaseV1>,
}

/// Manual RLP implementation because `PayloadId` and `serde_json::Value` are
/// outside of alloy-rlp’s blanket impls.
impl<M> Encodable for FlashblocksPayloadV1<M>
where
    M: Serialize,
{
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        // ---- compute payload length -------------------------------------------------
        let json_bytes = Bytes::from(
            serde_json::to_vec(&self.metadata).expect("serialising `metadata` to JSON never fails"),
        );

        // encoded-len helper — empty string is one byte (`0x80`)
        let empty_len = 1usize;

        let base_len = self.base.as_ref().map(|b| b.length()).unwrap_or(empty_len);

        let payload_len = self.payload_id.0.length()
            + self.index.length()
            + self.diff.length()
            + json_bytes.length()
            + base_len;

        Header {
            list: true,
            payload_length: payload_len,
        }
        .encode(out);

        // 1. `payload_id` – the inner `B64` already impls `Encodable`
        self.payload_id.0.encode(out);

        // 2. `index`
        self.index.encode(out);

        // 3. `diff`
        self.diff.encode(out);

        // 4. `metadata` (as raw JSON bytes)
        json_bytes.encode(out);

        // 5. `base` (`Option` as “value | empty string”)
        if let Some(base) = &self.base {
            base.encode(out);
        } else {
            // RLP encoding for empty value
            out.put_u8(0x80);
        }
    }

    fn length(&self) -> usize {
        let json_bytes = Bytes::from(
            serde_json::to_vec(&self.metadata).expect("serialising `metadata` to JSON never fails"),
        );

        let empty_len = 1usize;

        let base_len = self.base.as_ref().map(|b| b.length()).unwrap_or(empty_len);

        // list header length + payload length
        let payload_length = self.payload_id.0.length()
            + self.index.length()
            + self.diff.length()
            + json_bytes.length()
            + base_len;

        Header {
            list: true,
            payload_length,
        }
        .length()
            + payload_length
    }
}

impl<M> Decodable for FlashblocksPayloadV1<M>
where
    M: DeserializeOwned,
{
    fn decode(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let header = Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }

        // Limit the decoding window to the list payload only.
        let mut body = &buf[..header.payload_length];

        let payload_id = B64::decode(&mut body)?.into();
        let index = u64::decode(&mut body)?;
        let diff = ExecutionPayloadFlashblockDeltaV1::decode(&mut body)?;

        // metadata – stored as raw JSON bytes
        let meta_bytes = Bytes::decode(&mut body)?;
        let metadata = serde_json::from_slice(&meta_bytes)
            .map_err(|_| alloy_rlp::Error::Custom("bad JSON"))?;

        // base (`Option`)
        let base = if body.first() == Some(&0x80) {
            None
        } else {
            Some(ExecutionPayloadBaseV1::decode(&mut body)?)
        };

        // advance the original buffer cursor
        *buf = &buf[header.payload_length..];

        Ok(Self {
            payload_id,
            index,
            diff,
            metadata,
            base,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::keccak256;
    use alloy_rlp::{encode, Decodable};

    fn sample_diff() -> ExecutionPayloadFlashblockDeltaV1 {
        let mut buff = vec![];
        ExecutionPayloadFlashblockDeltaV1::default().encode(&mut buff);

        ExecutionPayloadFlashblockDeltaV1 {
            state_root: B256::from([1u8; 32]),
            receipts_root: B256::from([2u8; 32]),
            logs_bloom: Bloom::default(),
            gas_used: 21_000,
            block_hash: B256::from([3u8; 32]),
            transactions: vec![Bytes::from(vec![0xde, 0xad, 0xbe, 0xef])],
            withdrawals: vec![Withdrawal::default()],
            withdrawals_root: B256::from([4u8; 32]),
            flash_bal: FlashblockBlockAccessListWire::default(),
            flash_bal_hash: keccak256(&buff).into(),
        }
    }

    fn sample_base() -> ExecutionPayloadBaseV1 {
        ExecutionPayloadBaseV1 {
            parent_beacon_block_root: B256::from([5u8; 32]),
            parent_hash: B256::from([6u8; 32]),
            fee_recipient: Address::from([0u8; 20]),
            prev_randao: B256::from([7u8; 32]),
            block_number: 123,
            gas_limit: 30_000_000,
            timestamp: 1_700_000_000,
            extra_data: Bytes::from(b"hello".to_vec()),
            base_fee_per_gas: U256::from(1_000_000_000u64),
        }
    }

    #[test]
    fn roundtrip_without_base() {
        let original = FlashblocksPayloadV1 {
            payload_id: PayloadId::default(),
            index: 0,
            diff: sample_diff(),
            metadata: serde_json::json!({ "key": "value" }),
            base: None,
        };

        let encoded = encode(&original);
        assert_eq!(
            encoded.len(),
            original.length(),
            "length() must match actually-encoded size"
        );

        let mut slice = encoded.as_ref();
        let decoded = FlashblocksPayloadV1::decode(&mut slice).expect("decode succeeds");
        assert_eq!(original, decoded, "round-trip must be loss-less");
        assert!(
            slice.is_empty(),
            "decoder should consume the entire input buffer"
        );
    }

    #[test]
    fn roundtrip_with_base() {
        let original = FlashblocksPayloadV1 {
            payload_id: PayloadId::default(),
            index: 42,
            diff: sample_diff(),
            metadata: serde_json::json!({ "foo": 1, "bar": [1, 2, 3] }),
            base: Some(sample_base()),
        };

        let encoded = encode(&original);
        assert_eq!(encoded.len(), original.length());

        let mut slice = encoded.as_ref();
        let decoded = FlashblocksPayloadV1::decode(&mut slice).expect("decode succeeds");
        assert_eq!(original, decoded);
        assert!(slice.is_empty());
    }

    #[test]
    fn invalid_rlp_is_rejected() {
        let valid = FlashblocksPayloadV1 {
            payload_id: PayloadId::default(),
            index: 1,
            diff: sample_diff(),
            metadata: serde_json::json!({}),
            base: None,
        };

        // Encode, then truncate the last byte to corrupt the payload.
        let mut corrupted = encode(&valid);
        corrupted.pop();

        let mut slice = corrupted.as_ref();
        let result = FlashblocksPayloadV1::<String>::decode(&mut slice);
        assert!(
            result.is_err(),
            "decoder must flag malformed / truncated input"
        );
    }
}
