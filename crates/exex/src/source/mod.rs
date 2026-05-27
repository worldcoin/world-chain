//! Proposal sources.
//!
//! Mirrors the single-chain (pre-interop) slice of
//! `op-proposer/proposer/source/`:
//!
//! * [`local::LocalProposalSource`] — backed by the in-process ExEx node
//!   state. The default for this ExEx; no external RPC required.
//! * [`rollup::RollupProposalSource`] — backed by an `op-node` rollup RPC.
//!   Kept for parity with upstream and for use outside of the ExEx.
//!
//! Interop (supervisor / supernode / super-root) is intentionally not
//! supported here.

pub mod local;
pub mod rollup;

use alloy_primitives::{B256, BlockHash};
use async_trait::async_trait;
use thiserror::Error;

/// L1 block reference (number + hash) embedded in proposals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct L1BlockRef {
    pub number: u64,
    pub hash: BlockHash,
}

/// L2 block reference (used by legacy logs / metrics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct L2BlockRef {
    pub number: u64,
    pub hash: BlockHash,
    pub timestamp: u64,
}

/// Sync status reported by a proposal source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncStatus {
    pub current_l1: L1BlockRef,
    pub safe_l2: u64,
    pub finalized_l2: u64,
}

/// Optional metrics-only fields available from a single rollup source.
#[derive(Debug, Clone, Copy, Default)]
pub struct LegacyProposalData {
    pub head_l1: L1BlockRef,
    pub safe_l2: L2BlockRef,
    pub finalized_l2: L2BlockRef,
    pub block_ref: L2BlockRef,
}

/// A proposal returned from a [`ProposalSource`].
#[derive(Debug, Clone)]
pub struct Proposal {
    /// The L2 output root being proposed.
    pub root: B256,
    /// L2 block number this proposal corresponds to.
    pub sequence_num: u64,
    /// The L1 block this proposal is anchored to.
    pub current_l1: L1BlockRef,
    /// Single-source extra data; only useful for logs/metrics.
    pub legacy: LegacyProposalData,
}

impl Proposal {
    /// `ExtraData()` from upstream — a 32-byte big-endian L2 block number,
    /// passed to `DisputeGameFactory.create` as the `_extraData` parameter.
    pub fn extra_data(&self) -> [u8; 32] {
        let mut buf = [0u8; 32];
        buf[24..].copy_from_slice(&self.sequence_num.to_be_bytes());
        buf
    }
}

/// A pluggable proposal source.
#[async_trait]
pub trait ProposalSource: Send + Sync {
    async fn proposal_at_sequence_num(
        &self,
        sequence_num: u64,
    ) -> Result<Proposal, ProposalSourceError>;

    async fn sync_status(&self) -> Result<SyncStatus, ProposalSourceError>;

    /// Close underlying connections.
    async fn close(&self);
}

#[derive(Debug, Error)]
pub enum ProposalSourceError {
    #[error("rpc transport error: {0}")]
    Transport(String),
    #[error("rpc returned malformed payload: {0}")]
    Decode(String),
    #[error("unsupported output version: got {got:?}, expected {expected:?}")]
    UnsupportedOutputVersion { got: B256, expected: B256 },
    #[error("proposal block number mismatch: got {got}, expected {expected}")]
    BlockNumberMismatch { got: u64, expected: u64 },
    #[error("no available proposal sources")]
    NoSources,
    #[error("other: {0}")]
    Other(String),
}
