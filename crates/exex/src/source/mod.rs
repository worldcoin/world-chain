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

use alloy_eips::BlockNumHash;
use alloy_primitives::B256;
use async_trait::async_trait;
use thiserror::Error;

/// Sync status reported by a proposal source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncStatus {
    /// L1 block the source is anchored to (optional for the local source).
    pub current_l1: BlockNumHash,
    /// Latest safe L2 block number.
    pub safe_l2: u64,
    /// Latest finalized L2 block number.
    pub finalized_l2: u64,
}

/// A proposal returned from a [`ProposalSource`].
#[derive(Debug, Clone)]
pub struct Proposal {
    /// The L2 output root being proposed.
    pub root: B256,
    /// L2 block number this proposal corresponds to.
    pub block_number: u64,
    /// L2 block hash (for logs only).
    pub block_hash: B256,
    /// The L1 block this proposal is anchored to (zero for the local source).
    pub current_l1: BlockNumHash,
}

impl Proposal {
    /// `ExtraData()` from upstream — a 32-byte big-endian L2 block number,
    /// passed to `DisputeGameFactory.create` as the `_extraData` parameter.
    pub fn extra_data(&self) -> [u8; 32] {
        let mut buf = [0u8; 32];
        buf[24..].copy_from_slice(&self.block_number.to_be_bytes());
        buf
    }
}

/// A pluggable proposal source.
#[async_trait]
pub trait ProposalSource: Send + Sync {
    async fn proposal_at_block(&self, block_number: u64) -> Result<Proposal, ProposalSourceError>;

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
