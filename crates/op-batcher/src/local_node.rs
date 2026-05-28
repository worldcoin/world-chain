//! reth-backed implementation of [`LocalBlockSource`].
//!
//! This is the local-state replacement for the canonical batcher's rollup-node
//! RPC: `l2_block` replaces `eth_getBlockByNumber` + the L1-info decode, and
//! `heads` replaces the `optimism_syncStatus` fields the batcher consumes
//! (`UnsafeL2`, `LocalSafeL2`, and the safe block's `L1Origin`).
//!
//! The L2 safe head is set by `op-node`'s derivation pipeline via the Engine
//! API, so it advances precisely as the batches this ExEx submits are derived
//! back from L1 — see `BATCHER_SPEC.md` §2.

use std::sync::Arc;

use alloy_consensus::{BlockHeader, Transaction};
use alloy_eips::Encodable2718;
use alloy_primitives::Bytes;
use async_trait::async_trait;
use kona_protocol::L1BlockInfoTx;
use reth_exex::ExExContext;
use reth_node_api::FullNodeComponents;
use reth_optimism_primitives::OpBlock;
use reth_storage_api::{BlockHashReader, BlockIdReader, BlockNumReader, BlockReader};

use crate::source::{BlockSourceError, L2BlockData, LocalBlockSource, LocalHeads};

/// Amortized trait bounds for a node provider this ExEx can read full OP blocks
/// from. A blanket impl means any provider already satisfying the constituents
/// is automatically `ProviderBounds`.
pub trait ProviderBounds:
    BlockReader<Block = OpBlock>
    + BlockIdReader
    + BlockNumReader
    + BlockHashReader
    + Clone
    + Send
    + Sync
    + 'static
{
}

impl<T> ProviderBounds for T where
    T: BlockReader<Block = OpBlock>
        + BlockIdReader
        + BlockNumReader
        + BlockHashReader
        + Clone
        + Send
        + Sync
        + 'static
{
}

/// Adapter wrapping an ExEx node provider.
pub struct ExExBatcherReader<N: FullNodeComponents> {
    provider: N::Provider,
}

impl<N: FullNodeComponents> ExExBatcherReader<N>
where
    N::Provider: ProviderBounds,
{
    pub fn new(ctx: &ExExContext<N>) -> Self {
        Self {
            provider: ctx.components.provider().clone(),
        }
    }
}

/// Convert a full OP block into normalized [`L2BlockData`].
///
/// Mirrors `BlockToSingularBatch` (`op-node/rollup/derive/channel_out.go`):
/// the first transaction must be the L1-info deposit (used to derive the epoch),
/// and deposit transactions are excluded from the batch.
fn block_to_l2data(
    number: u64,
    hash: alloy_primitives::B256,
    block: OpBlock,
) -> Result<L2BlockData, BlockSourceError> {
    let txs = block.body.transactions;
    if txs.is_empty() {
        return Err(BlockSourceError::EmptyBlock(number));
    }

    // First tx is the L1-info deposit; decode it to get the L1 origin (epoch).
    let l1_info = L1BlockInfoTx::decode_calldata(txs[0].input().as_ref())
        .map_err(|e| BlockSourceError::L1InfoDecode(number, format!("{e:?}")))?;
    let epoch = l1_info.id();

    let transactions: Vec<Bytes> = txs
        .iter()
        .filter(|tx| !tx.is_deposit())
        .map(|tx| Bytes::from(tx.encoded_2718()))
        .collect();

    Ok(L2BlockData {
        number,
        hash,
        parent_hash: block.header.parent_hash(),
        timestamp: block.header.timestamp(),
        epoch_num: epoch.number,
        epoch_hash: epoch.hash,
        transactions,
    })
}

#[async_trait]
impl<N> LocalBlockSource for ExExBatcherReader<N>
where
    N: FullNodeComponents,
    N::Provider: ProviderBounds,
{
    async fn l2_block(&self, number: u64) -> Result<L2BlockData, BlockSourceError> {
        let provider = self.provider.clone();
        tokio::task::spawn_blocking(move || {
            let block = provider
                .block_by_number(number)
                .map_err(|e| BlockSourceError::Provider(e.to_string()))?
                .ok_or(BlockSourceError::NotFound(number))?;
            let hash = provider
                .block_hash(number)
                .map_err(|e| BlockSourceError::Provider(e.to_string()))?
                .unwrap_or_default();
            block_to_l2data(number, hash, block)
        })
        .await
        .map_err(|e| BlockSourceError::Provider(e.to_string()))?
    }

    async fn heads(&self) -> Result<LocalHeads, BlockSourceError> {
        let provider = self.provider.clone();
        tokio::task::spawn_blocking(move || {
            let unsafe_l2 = provider
                .best_block_number()
                .map_err(|e| BlockSourceError::Provider(e.to_string()))?;
            let unsafe_hash = provider
                .block_hash(unsafe_l2)
                .map_err(|e| BlockSourceError::Provider(e.to_string()))?
                .unwrap_or_default();

            let safe = provider
                .safe_block_num_hash()
                .map_err(|e| BlockSourceError::Provider(e.to_string()))?
                .unwrap_or_default();

            // Derive the safe block's L1 origin from its L1-info deposit.
            let safe_l1_origin = if safe.number == 0 {
                0
            } else {
                let block = provider
                    .block_by_number(safe.number)
                    .map_err(|e| BlockSourceError::Provider(e.to_string()))?
                    .ok_or(BlockSourceError::NotFound(safe.number))?;
                let txs = block.body.transactions;
                if txs.is_empty() {
                    return Err(BlockSourceError::EmptyBlock(safe.number));
                }
                L1BlockInfoTx::decode_calldata(txs[0].input().as_ref())
                    .map_err(|e| BlockSourceError::L1InfoDecode(safe.number, format!("{e:?}")))?
                    .id()
                    .number
            };

            Ok(LocalHeads {
                unsafe_l2,
                unsafe_hash,
                safe_l2: safe.number,
                safe_hash: safe.hash,
                safe_l1_origin,
            })
        })
        .await
        .map_err(|e| BlockSourceError::Provider(e.to_string()))?
    }
}

/// Build an `Arc<dyn LocalBlockSource>` from an `ExExContext`.
pub fn local_source_from_ctx<N>(ctx: &ExExContext<N>) -> Arc<dyn LocalBlockSource>
where
    N: FullNodeComponents,
    N::Provider: ProviderBounds,
{
    Arc::new(ExExBatcherReader::<N>::new(ctx))
}
