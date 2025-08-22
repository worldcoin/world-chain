use std::sync::Arc;

use alloy_consensus::EMPTY_OMMER_ROOT_HASH;
use alloy_consensus::{
    proofs::ordered_trie_root_with_encoder, Block, BlockBody, BlockHeader, Header,
};
use alloy_eips::eip4895::Withdrawals;
use alloy_eips::merge::BEACON_NONCE;
use alloy_eips::Decodable2718;
use alloy_eips::Encodable2718;
use alloy_primitives::{FixedBytes, B256, U256};
use eyre::eyre::eyre;
use op_alloy_consensus::OpBlock;
use op_alloy_consensus::OpTxEnvelope;
use reth::api::Block as _;
use reth::api::BlockBody as _;
use reth::payload::EthPayloadBuilderAttributes;
use reth::revm::cancelled::CancelOnDrop;
use reth::{api::BuiltPayload, payload::PayloadBuilderAttributes};
use reth_optimism_forks::OpHardforks;
use reth_provider::{ChainSpecProvider, StateProviderFactory as _};
use reth_rpc_eth_api::RpcNodeCore;

use reth_basic_payload_builder::PayloadConfig;
use reth_chain_state::ExecutedBlockWithTrieUpdates;
use reth_node_api::NodeTypesWithDB;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::{
    OpBuiltPayload, OpDAConfig, OpEvmConfig, OpPayloadAttributes, OpPayloadBuilderAttributes,
};
use reth_optimism_primitives::{OpPrimitives, OpReceipt};
use reth_primitives::{NodePrimitives, RecoveredBlock};

use parking_lot::RwLock;
use reth_provider::providers::{BlockchainProvider, ProviderNodeTypes};
use rollup_boost::{
    ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1, FlashblocksPayloadV1,
};
use serde::{Deserialize, Serialize};

use crate::builder::executor::FlashblocksBlockExecutorFactory;
use crate::{PayloadBuilderCtx, PayloadBuilderCtxBuilder};

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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Flashblocks(pub Vec<Flashblock>);

impl Flashblocks {
    /// Creates a new instance of [`Flashblocks`] from a vector of [`FlashblocksPayloadV1`].
    pub fn from_payloads(payloads: Vec<Flashblock>) -> Self {
        Flashblocks(payloads)
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

/// The current state of all known pre confirmations received over the P2P layer
/// or generated from the payload building job of this node.
///
/// The state is flushed when FCU is received with a parent hash that matches the block hash
/// of the latest pre confirmation _or_ when an FCU is received that does not match the latest pre confirmation,
/// in which case the pre confirmations were not included as part of the canonical chain.
#[derive(Debug, Clone)]
pub struct FlashblocksState<N, P, T>
where
    N: NodeTypesWithDB,
    P: PayloadBuilderCtxBuilder<OpEvmConfig, OpChainSpec, T>,
{
    components: N,
    da_config: OpDAConfig,
    flashblocks: Arc<RwLock<Flashblocks>>,
    state_provider: BlockchainProvider<N>,
    executor_factory: FlashblocksBlockExecutorFactory,
    payload_builder_ctx_builder: P,
    latest_payload: Option<OpBuiltPayload>,

    _marker: std::marker::PhantomData<T>,
}

// #[derive(Debug, Clone)]
// pub struct FlashblocksStateInner {
//     flashblocks: Flashblocks,
//     payload_ctx: Option<OpPayloadAttributes>,
// }

impl<N, P, T> FlashblocksState<N, P, T>
where
    N: NodeTypesWithDB
        + ProviderNodeTypes
        + RpcNodeCore<Evm = OpEvmConfig>
        + ChainSpecProvider<ChainSpec = OpChainSpec>,
    P: PayloadBuilderCtxBuilder<OpEvmConfig, OpChainSpec, T>,
{
    /// Creates a new instance of [`FlashblocksState`].
    pub fn new(
        components: N,
        da_config: OpDAConfig,
        state_provider: BlockchainProvider<N>,
        executor_factory: FlashblocksBlockExecutorFactory,
        payload_builder_ctx_builder: P,
    ) -> Self {
        Self {
            components,
            da_config,
            flashblocks: Arc::new(RwLock::new(Flashblocks::default())),
            state_provider,
            executor_factory,
            payload_builder_ctx_builder,
            latest_payload: None,
            _marker: std::marker::PhantomData,
        }
    }

    /// Returns a reference to the latest flashblock.
    pub fn last(&self) -> Option<Flashblock> {
        self.flashblocks.read().0.last().cloned()
    }

    /// Returns a reference to the latest flashblock.
    pub fn flashblocks(&self) -> Flashblocks {
        self.flashblocks.read().clone()
    }

    /// Appends a new flashblock to the state.
    pub fn push(&self, payload: Flashblock) {
        let mut state = self.flashblocks.write();
        if payload.flashblock.index == 0 {
            // Since flahblocks are pushed strictly in order, we can always clear
            // self.latest_payload = None;
            state.0.clear();
        }
        state.0.push(payload);
        let cancel = CancelOnDrop::default();

        let first = state.0.first().unwrap().flashblock;
        let base = first.base.as_ref().unwrap();

        let eth_attrs = EthPayloadBuilderAttributes {
            id: todo!(),
            parent: base.parent_hash,
            timestamp: base.timestamp,
            suggested_fee_recipient: base.fee_recipient,
            prev_randao: base.prev_randao,
            withdrawals: Withdrawals(first.diff.withdrawals.clone()),
            parent_beacon_block_root: todo!(),
        };
        let attributes = OpPayloadBuilderAttributes {
            payload_attributes: eth_attrs,
            no_tx_pool: false,
            transactions: vec![],
            gas_limit: None,
            eip_1559_params: None,
        };

        let head = self
            .state_provider
            .state_by_block_hash(base.parent_hash)
            .ok();

        let config = PayloadConfig::new(todo!(), attributes);
        let payload_builder_ctx = self.payload_builder_ctx_builder.build::<T>(
            self.components.evm_config().clone(),
            self.da_config.clone(),
            self.components.chain_spec(),
            config,
            &cancel,
            self.latest_payload,
        );

        // let Self { best } = self;
        // let span = span!(
        //     tracing::Level::INFO,
        //     "flashblock_builder",
        //     id = %ctx.payload_id(),
        // );
        //
        // let _enter = span.enter();
        //
        // debug!(target: "payload_builder", "building new payload");
        //
        // let state = StateProviderDatabase::new(&state_provider);
        //
        // // 1. Prepare the db
        // let (bundle, receipts, transactions, gas_used) = if let Some(payload) = &best_payload {
        //     // if we have a best payload we will always have a bundle
        //     let execution_result = &payload
        //         .executed_block()
        //         .ok_or(PayloadBuilderError::MissingPayload)?
        //         .execution_output;
        //
        //     let receipts = execution_result
        //         .receipts
        //         .iter()
        //         .flatten()
        //         .cloned()
        //         .collect();
        //
        //     let transactions = payload
        //         .block()
        //         .body()
        //         .transactions_iter()
        //         .cloned()
        //         .map(|tx| {
        //             tx.try_into_recovered()
        //                 .map_err(|_| PayloadBuilderError::Other(eyre!("tx recovery failed").into()))
        //         })
        //         .collect::<Result<Vec<_>, _>>()?;
        //
        //     (
        //         execution_result.bundle.clone(),
        //         receipts,
        //         transactions,
        //         Some(payload.block().gas_used()),
        //     )
        // } else {
        //     (BundleState::default(), Vec::new(), Vec::new(), None)
        // };
        //
        // let gas_limit = ctx
        //     .attributes()
        //     .gas_limit
        //     .unwrap_or(ctx.parent().gas_limit)
        //     .saturating_sub(gas_used.unwrap_or(0));
        //
        // let bundle_is_empty = bundle.is_empty();
        //
        // let mut state = State::builder()
        //     .with_database(state)
        //     .with_bundle_prestate(bundle)
        //     .with_bundle_update()
        //     .build();
        //
        // // 2. Create the block builder
        // let mut builder =
        //     Self::block_builder(&mut state, transactions.clone(), receipts, gas_used, ctx)?;
        //
        // // Only execute the sequencer transactions on the first payload. The sequencer transactions
        // // will already be in the [`BundleState`] at this point if the `bundle` non-empty.
        // let mut info = if bundle_is_empty {
        //     // 3. apply pre-execution changes
        //     builder.apply_pre_execution_changes()?;
        //
        //     // 4. Execute Deposit transactions
        //     ctx.execute_sequencer_transactions(&mut builder)
        //         .map_err(PayloadBuilderError::other)?
        // } else {
        //     ExecutionInfo::default()
        // };
        //
        // // 5. Execute transactions from the tx-pool, draining any transactions seen in previous
        // // flashblocks
        // if !ctx.attributes().no_tx_pool {
        //     let best_txs = best(ctx.best_transaction_attributes(builder.evm_mut().block()));
        //     let mut best_txns = BestPayloadTxns::new(best_txs)
        //         .with_prev(transactions.iter().map(|tx| *tx.hash()).collect::<Vec<_>>());
        //
        //     if ctx
        //         .execute_best_transactions(&mut info, &mut builder, best_txns.guard(), gas_limit)?
        //         .is_none()
        //     {
        //         warn!(target: "payload_builder", "payload build cancelled");
        //         if let Some(best_payload) = best_payload {
        //             // we can return the previous best payload since we didn't include any new txs
        //             return Ok(BuildOutcomeKind::Freeze(best_payload));
        //         } else {
        //             return Err(PayloadBuilderError::MissingPayload);
        //         }
        //     }
        // }
        //
        // // 6. Build the block
        // let build_outcome = builder.finish(&state_provider)?;
        //
        // // 7. Seal the block
        // let BlockBuilderOutcome {
        //     execution_result,
        //     block,
        //     hashed_state,
        //     trie_updates,
        // } = build_outcome;
        //
        // let sealed_block = Arc::new(block.sealed_block().clone());
        //
        // let execution_outcome = ExecutionOutcome::new(
        //     state.take_bundle(),
        //     vec![execution_result.receipts.clone()],
        //     block.number(),
        //     Vec::new(),
        // );
        //
        // // create the executed block data
        // let executed: ExecutedBlockWithTrieUpdates<OpPrimitives> = ExecutedBlockWithTrieUpdates {
        //     block: ExecutedBlock {
        //         recovered_block: Arc::new(block),
        //         execution_output: Arc::new(execution_outcome),
        //         hashed_state: Arc::new(hashed_state),
        //     },
        //     trie: ExecutedTrieUpdates::Present(Arc::new(trie_updates)),
        // };
        //
        // let payload = OpBuiltPayload::new(
        //     ctx.payload_id(),
        //     sealed_block,
        //     info.total_fees,
        //     Some(executed),
        // );
    }

    /// Clears the current state of flashblocks.
    pub fn clear(&self) {
        // TODO:verify that pending state is automatically flushed here
        self.flashblocks.write().0.clear();
    }
}
