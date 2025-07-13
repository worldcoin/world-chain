use alloy_consensus::{BlockHeader, Sealable};
use alloy_primitives::{map::foldhash::HashMap, Address, Bytes, U256};
use reth::api::{BlockBody, PayloadBuilderError};
use reth::payload::PayloadId;
use reth::{
    revm::primitives::{alloy_primitives::Bloom, B256},
    rpc::types::Withdrawal,
};
use reth_evm::execute::BlockBuilderOutcome;
use reth_optimism_node::OpPayloadBuilderAttributes;
use reth_primitives::NodePrimitives;
use revm::database::BundleState;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Debug;

use crate::payload_builder_ctx::ExecutionInfo;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct FlashblocksPayloadV1 {
    /// The payload id of the flashblock
    pub payload_id: PayloadId,
    /// The index of the flashblock in the block
    pub index: u64,
    /// The base execution payload configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<ExecutionPayloadBaseV1>,
    /// The delta/diff containing modified portions of the execution payload
    pub diff: ExecutionPayloadFlashblockDeltaV1,
    /// Additional metadata associated with the flashblock
    pub metadata: Value,
}

/// Represents the base configuration of an execution payload that remains constant
/// throughout block construction. This includes fundamental block properties like
/// parent hash, block number, and other header fields that are determined at
/// block creation and cannot be modified.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
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

/// Represents the modified portions of an execution payload within a flashblock.
/// This structure contains only the fields that can be updated during block construction,
/// such as state root, receipts, logs, and new transactions. Other immutable block fields
/// like parent hash and block number are excluded since they remain constant throughout
/// the block's construction.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FlashblocksMetadata<N: NodePrimitives> {
    pub receipts: HashMap<B256, N::Receipt>,
    pub new_account_balances: HashMap<Address, U256>,
    pub block_number: u64,
}

pub struct ExecutionData<N: NodePrimitives, E: Debug + Default> {
    pub bundle_state: BundleState,
    pub outcome: BlockBuilderOutcome<N>,
    pub info: ExecutionInfo<N, E>,
    pub attributes: OpPayloadBuilderAttributes<N::SignedTx>,
    pub total_flashblocks: u64,
    pub current_flashblock_offset: u64,
}

impl<N: NodePrimitives, E: Debug + Default> ExecutionData<N, E> {
    pub fn new(
        bundle_state: BundleState,
        outcome: BlockBuilderOutcome<N>,
        info: ExecutionInfo<N, E>,
        total_flashblocks: u64,
        current_flashblock_offset: u64,
        attributes: OpPayloadBuilderAttributes<N::SignedTx>,
    ) -> Self {
        Self {
            bundle_state,
            outcome,
            info,
            total_flashblocks,
            current_flashblock_offset,
            attributes,
        }
    }
}

impl<N: NodePrimitives, E: Debug + Default> TryFrom<&ExecutionData<N, E>> for FlashblocksPayloadV1 {
    type Error = PayloadBuilderError;
    fn try_from(data: &ExecutionData<N, E>) -> Result<Self, Self::Error> {
        let base = if data.total_flashblocks == 0 {
            Some(ExecutionPayloadBaseV1 {
                parent_beacon_block_root: data.outcome.block.header().parent_hash(),
                parent_hash: data.outcome.block.header().parent_hash(),
                fee_recipient: data.attributes.payload_attributes.suggested_fee_recipient,
                prev_randao: data.attributes.payload_attributes.prev_randao,
                block_number: data.outcome.block.header().number(),
                gas_limit: data.outcome.block.header().gas_limit(),
                timestamp: data.outcome.block.header().timestamp(),
                extra_data: data.outcome.block.header().extra_data().clone(),
                base_fee_per_gas: data
                    .outcome
                    .block
                    .header()
                    .base_fee_per_gas()
                    .map(U256::from)
                    .unwrap_or_default(), // TODO: Should we default to parent?
            })
        } else {
            None
        };

        let account_balances = data
            .outcome
            .hashed_state
            .accounts
            .iter()
            .filter_map(|(account_hash, account)| {
                if let Some(acc) = account {
                    Some((Address::from_word(*account_hash), acc.balance))
                } else {
                    None
                }
            })
            .collect::<HashMap<_, _>>();

        let transactions = data
            .outcome
            .block
            .body()
            .transaction_hashes_iter()
            .skip(data.current_flashblock_offset as usize)
            .collect::<Vec<_>>();

        let receipts = data
            .info
            .receipts
            .iter()
            .skip(data.current_flashblock_offset as usize)
            .zip(
                transactions
                    .into_iter()
                    .skip(data.current_flashblock_offset as usize),
            )
            .map(|(receipt, tx_hash)| (*tx_hash, receipt.clone()))
            .collect::<HashMap<_, _>>();

        Ok(FlashblocksPayloadV1 {
            payload_id: data.attributes.payload_attributes.payload_id(),
            index: data.total_flashblocks,
            base,
            diff: ExecutionPayloadFlashblockDeltaV1 {
                state_root: data.outcome.block.header().state_root(),
                receipts_root: data.outcome.block.header().receipts_root(),
                logs_bloom: data.outcome.block.header().logs_bloom(),
                gas_used: data.outcome.block.header().gas_used(),
                block_hash: data.outcome.block.header().hash_slow(),
                transactions: data
                    .outcome
                    .block
                    .body()
                    .encoded_2718_transactions_iter()
                    .map(Bytes::from)
                    .collect(),
                withdrawals: data
                    .outcome
                    .block
                    .body()
                    .withdrawals()
                    .as_ref()
                    .map_or_else(|| Vec::new(), |w| w.iter().cloned().collect()),
            },
            metadata: serde_json::to_value(FlashblocksMetadata::<N> {
                receipts: receipts,
                new_account_balances: account_balances,
                block_number: data.outcome.block.header().number(),
            })
            .map_err(|e| PayloadBuilderError::Other(e.to_string().into()))?,
        })
    }
}
