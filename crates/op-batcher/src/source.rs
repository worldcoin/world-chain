//! Local block source: the in-process equivalent of the canonical batcher's
//! `op-node` `optimism_syncStatus` RPC + `eth_getBlockByNumber`.
//!
//! This module defines the data types and the [`LocalBlockSource`] trait. The
//! reth-backed implementation lives in [`crate::local_node`].
//!
//! See `BATCHER_SPEC.md` §2 (local-state mapping) and §5 (loading L2 blocks).

use alloy_primitives::{B256, Bytes};
use async_trait::async_trait;
use thiserror::Error;

/// Normalized L2 block data needed to build a singular batch. Produced by the
/// reth adapter so the encoder never depends on reth/op block types directly.
///
/// `transactions` excludes deposit transactions (they are derived from L1, not
/// from batches) and holds the remaining transactions as opaque EIP-2718
/// encoded bytes — exactly the `SingularBatch.transactions` wire format.
#[derive(Debug, Clone)]
pub struct L2BlockData {
    pub number: u64,
    pub hash: B256,
    pub parent_hash: B256,
    pub timestamp: u64,
    /// L1 origin block number (the batch epoch number).
    pub epoch_num: u64,
    /// L1 origin block hash (the batch epoch hash).
    pub epoch_hash: B256,
    /// Non-deposit transactions, EIP-2718 encoded.
    pub transactions: Vec<Bytes>,
}

/// Snapshot of the local L2 chain heads, assembled from reth node state. This is
/// the local equivalent of the fields of `eth.SyncStatus` the canonical batcher
/// reads from the rollup node.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalHeads {
    /// Canonical (unsafe) L2 tip — the highest block to batch up to.
    pub unsafe_l2: u64,
    pub unsafe_hash: B256,
    /// Derivation-backed L2 safe head — the resume/prune cursor.
    pub safe_l2: u64,
    pub safe_hash: B256,
    /// L1 origin (number) of the safe L2 block.
    pub safe_l1_origin: u64,
}

/// Abstraction over the local node state the batcher reads from.
#[async_trait]
pub trait LocalBlockSource: Send + Sync {
    /// Full normalized L2 block data by number.
    async fn l2_block(&self, number: u64) -> Result<L2BlockData, BlockSourceError>;

    /// Current unsafe/safe L2 heads and the safe block's L1 origin.
    async fn heads(&self) -> Result<LocalHeads, BlockSourceError>;
}

#[derive(Debug, Error)]
pub enum BlockSourceError {
    #[error("block {0} not found")]
    NotFound(u64),
    #[error("block {0} has no transactions (missing L1-info deposit)")]
    EmptyBlock(u64),
    #[error("failed to decode L1-info deposit for block {0}: {1}")]
    L1InfoDecode(u64, String),
    #[error("provider error: {0}")]
    Provider(String),
}
