use alloy_consensus::EMPTY_OMMER_ROOT_HASH;
use alloy_consensus::{
    proofs::ordered_trie_root_with_encoder, Block, BlockBody, BlockHeader, Header,
};
use alloy_eips::merge::BEACON_NONCE;
use alloy_eips::Decodable2718;
use alloy_eips::Encodable2718;
use alloy_primitives::{FixedBytes, B256, U256};
use eyre::eyre::{bail, eyre};
use op_alloy_consensus::OpBlock;
use op_alloy_consensus::OpTxEnvelope;
use reth::api::Block as _;
use reth::api::BlockBody as _;
use reth::{api::BuiltPayload, payload::PayloadBuilderAttributes};
use reth_basic_payload_builder::PayloadConfig;
use reth_chain_state::ExecutedBlockWithTrieUpdates;
use reth_optimism_node::{OpBuiltPayload, OpPayloadBuilderAttributes};
use reth_optimism_primitives::{OpPrimitives, OpReceipt};
use reth_primitives::{NodePrimitives, RecoveredBlock};

use rollup_boost::{
    ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1, FlashblocksPayloadV1,
};
use serde::{Deserialize, Serialize};

/// A type wrapper around a single flashblock payload.
#[derive(Clone, Debug, PartialEq, Default, Deserialize, Serialize, Eq)]
pub struct Flashblock {
    pub flashblock: FlashblocksPayloadV1,
}

impl Flashblock {
    pub fn new(
        payload: &OpBuiltPayload,
        config: PayloadConfig<OpPayloadBuilderAttributes<OpTxEnvelope>, Header>,
        index: u64,
        transactions_offset: usize,
    ) -> Self {
        let block = payload.block();
        let fees = payload.fees();

        // todo cache trie updated
        let payload_base = if index == 0 {
            Some(ExecutionPayloadBaseV1 {
                parent_beacon_block_root: config
                    .attributes
                    .payload_attributes
                    .parent_beacon_block_root
                    .unwrap_or_default(),
                parent_hash: config.attributes.parent(),
                fee_recipient: config
                    .attributes
                    .payload_attributes
                    .suggested_fee_recipient(),
                prev_randao: config.attributes.payload_attributes.prev_randao,
                block_number: block.number(),
                gas_limit: block.gas_limit(),
                timestamp: config.attributes.payload_attributes.timestamp,
                extra_data: block.extra_data().clone(),
                base_fee_per_gas: block.base_fee_per_gas().map(U256::from).unwrap_or_default(),
            })
        } else {
            None
        };

        let transactions = block
            .body()
            .transactions_iter()
            .skip(transactions_offset)
            .map(|tx| tx.encoded_2718().into())
            .collect::<Vec<_>>();

        let metadata = BlockMetaData {
            fees,
            receipts: payload
                .executed_block()
                .map(|block| {
                    block
                        .execution_output
                        .receipts
                        .iter()
                        .skip(transactions_offset)
                        .flatten()
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            transactions_root: block.transactions_root(),
            executed: payload.executed_block(),
        };
        Flashblock {
            flashblock: FlashblocksPayloadV1 {
                payload_id: config.attributes.payload_id(),
                index,
                base: payload_base,
                diff: ExecutionPayloadFlashblockDeltaV1 {
                    state_root: block.state_root(),
                    receipts_root: block.receipts_root(),
                    logs_bloom: block.logs_bloom(),
                    gas_used: block.gas_used(),
                    block_hash: block.hash(),
                    transactions,
                    withdrawals: block
                        .body()
                        .withdrawals()
                        .cloned()
                        .unwrap_or_default()
                        .to_vec(),
                    withdrawals_root: block.withdrawals_root().unwrap_or_default(),
                },
                metadata: serde_json::to_value(metadata)
                    .expect("BlockMetaData should always serialize to JSON"),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Eq)]
pub struct BlockMetaData<N: NodePrimitives<Block = OpBlock, Receipt = OpReceipt>> {
    /// Total fees collected by the proposer for this block.
    pub fees: U256,
    /// The receipts for the transactions included in this flashblock.
    pub receipts: Vec<N::Receipt>,
    /// The transactions root of the block.
    pub transactions_root: B256,
    /// The executed block with trie updates, if available.
    #[serde(skip)]
    pub executed: Option<ExecutedBlockWithTrieUpdates<N>>,
}

impl Flashblock {
    pub fn flashblock(&self) -> &FlashblocksPayloadV1 {
        &self.flashblock
    }

    pub fn into_flashblock(self) -> FlashblocksPayloadV1 {
        self.flashblock
    }

    pub fn payload_id(&self) -> &FixedBytes<8> {
        &self.flashblock.payload_id.0
    }

    pub fn base(&self) -> Option<&ExecutionPayloadBaseV1> {
        self.flashblock.base.as_ref()
    }

    pub fn diff(&self) -> &ExecutionPayloadFlashblockDeltaV1 {
        &self.flashblock.diff
    }
}

impl Flashblock {
    pub fn reduce(flashblocks: Flashblocks) -> Option<Flashblock> {
        let mut iter = flashblocks.0.into_iter();
        let mut acc = iter.next()?.flashblock;

        for next in iter {
            debug_assert_eq!(
                acc.payload_id, next.flashblock.payload_id,
                "all flashblocks should have the same payload_id"
            );

            if acc.base.is_none() && next.flashblock.base.is_some() {
                acc.base = next.flashblock.base;
            }

            acc.diff.gas_used = next.flashblock.diff.gas_used;

            acc.diff
                .transactions
                .extend(next.flashblock.diff.transactions);
            acc.diff
                .withdrawals
                .extend(next.flashblock.diff.withdrawals);

            acc.diff.state_root = next.flashblock.diff.state_root;
            acc.diff.receipts_root = next.flashblock.diff.receipts_root;
            acc.diff.logs_bloom = next.flashblock.diff.logs_bloom;
            acc.diff.withdrawals_root = next.flashblock.diff.withdrawals_root;
            acc.diff.block_hash = next.flashblock.diff.block_hash;
        }

        Some(Flashblock { flashblock: acc })
    }
}

impl TryFrom<Flashblock> for RecoveredBlock<Block<OpTxEnvelope>> {
    type Error = eyre::Report;

    /// Do _not_ use this method unless all flashblocks have been properly reduced
    fn try_from(value: Flashblock) -> Result<RecoveredBlock<Block<OpTxEnvelope>>, Self::Error> {
        let base = value.flashblock.base.clone().ok_or(eyre!(
            "Cannot convert Flashblock to RecoveredBlock without base"
        ))?;
        let diff = value.flashblock.diff.clone();
        let header = Header {
            parent_beacon_block_root: None,
            state_root: diff.state_root,
            receipts_root: diff.receipts_root,
            logs_bloom: diff.logs_bloom,
            withdrawals_root: Some(diff.withdrawals_root),
            parent_hash: base.parent_hash,
            base_fee_per_gas: Some(base.base_fee_per_gas.to()),
            beneficiary: base.fee_recipient,
            transactions_root: ordered_trie_root_with_encoder(&diff.transactions, |tx, e| {
                *e = tx.as_ref().to_vec()
            }),
            ommers_hash: EMPTY_OMMER_ROOT_HASH,
            blob_gas_used: Some(0),
            difficulty: U256::ZERO,
            number: base.block_number,
            gas_limit: base.gas_limit,
            gas_used: diff.gas_used,
            timestamp: base.timestamp,
            extra_data: base.extra_data.clone(),
            mix_hash: base.prev_randao,
            nonce: BEACON_NONCE.into(),
            requests_hash: None, // TODO: Isthmus
            excess_blob_gas: Some(0),
        };

        let transactions_encoded = diff
            .transactions
            .iter()
            .cloned()
            .map(|t| <OpPrimitives as NodePrimitives>::SignedTx::decode_2718(&mut t.as_ref()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| eyre!("Failed to decode transaction: {:?}", e))?;

        let body = BlockBody {
            transactions: transactions_encoded,
            withdrawals: Some(alloy_eips::eip4895::Withdrawals(diff.withdrawals.to_vec())),
            ommers: vec![],
        };

        Block::new(header, body)
            .try_into_recovered()
            .map_err(|e| eyre!("Failed to recover block: {:?}", e))
    }
}

/// A collection of flashblocks mapped to the same payload ID.
///
/// Guaranteed to be
/// - Non-empty
/// - All flashblocks have the same payload ID
/// - Flashblocks have contiguous indices starting from 0
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Flashblocks(Vec<Flashblock>);

impl Flashblocks {
    /// Creates a new `Flashblocks` collection from the given vector of flashblocks.
    ///
    /// Validates that the collection is non-empty, all flashblocks have the same payload ID,
    /// and that the indices are contiguous starting from 0.
    /// Returns an error if any of these conditions are not met.
    pub fn new(flashblocks: Vec<Flashblock>) -> eyre::Result<Self> {
        if flashblocks.is_empty() {
            bail!("Flashblocks cannot be empty")
        }

        let mut iter = flashblocks.iter();

        let first = iter.next().unwrap();
        if first.flashblock.base.is_none() {
            bail!("The first flashblock must contain the base payload");
        }

        let payload_id = first.payload_id();
        if iter.any(|fb| fb.payload_id() != payload_id) {
            bail!("All flashblocks must have the same payload_id")
        }

        for (i, fb) in flashblocks.iter().enumerate() {
            if fb.flashblock.index != i as u64 {
                bail!("Flashblocks must have contiguous indices starting from 0");
            }
        }

        Ok(Self(flashblocks))
    }

    /// Pushes a new flashblock into the collection.
    ///
    /// If the new flashblock has a different payload ID, the collection is cleared
    /// and the new flashblock is added as the first element (index must be 0).
    /// If the new flashblock has the same payload ID, it is added to the end
    /// of the collection (index must be contiguous).
    /// Returns `Ok(true)` if the collection was reset, `Ok(false)` if the flashblock
    /// was added to the existing collection, or an error if the conditions are not met.
    pub fn push(&mut self, flashblock: Flashblock) -> eyre::Result<bool> {
        if flashblock.payload_id() != self.payload_id() {
            if flashblock.flashblock.index != 0 {
                bail!("New flashblock has different payload_id and index is not 0");
            }
            let Some(base) = &flashblock.flashblock.base else {
                bail!("New flashblock has different payload_id and must contain the base payload");
            };
            if base.timestamp <= self.base().timestamp {
                bail!("New flashblock has different payload_id and must have a later timestamp than the current base");
            }

            self.0.clear();
            self.0.push(flashblock);
            return Ok(true);
        }

        if flashblock.flashblock.index != (self.0.len() as u64) {
            bail!("New flashblock index is not contiguous");
        }

        self.0.push(flashblock);
        Ok(false)
    }

    pub fn last(&self) -> &FlashblocksPayloadV1 {
        &self.0.last().unwrap().flashblock
    }

    pub fn flashblocks(&self) -> &[Flashblock] {
        &self.0
    }

    pub fn payload_id(&self) -> &FixedBytes<8> {
        self.0.first().unwrap().payload_id()
    }

    pub fn base(&self) -> &ExecutionPayloadBaseV1 {
        &self.0.first().unwrap().flashblock.base.as_ref().unwrap()
    }
}

impl TryFrom<Flashblocks> for RecoveredBlock<Block<OpTxEnvelope>> {
    type Error = eyre::Report;

    /// Converts a collection of flashblocks into a single `RecoveredBlock`.
    fn try_from(value: Flashblocks) -> Result<RecoveredBlock<Block<OpTxEnvelope>>, Self::Error> {
        let reduced = Flashblock::reduce(value).ok_or(eyre!("No flashblocks to reduce"))?;
        reduced.try_into()
    }
}
