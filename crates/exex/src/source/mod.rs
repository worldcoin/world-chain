//! Proposal sources.
//!
//! Mirrors `op-proposer/proposer/source/`:
//!
//! * [`rollup::RollupProposalSource`] — backed by a single (or active list of)
//!   `op-node` rollup RPC endpoints.
//! * [`supervisor::SupervisorProposalSource`] — backed by `op-supervisor`
//!   instances (interop).
//! * [`supernode::SuperNodeProposalSource`] — backed by `op-supernode`
//!   instances (interop).
//!
//! Higher levels of the proposer treat all sources uniformly through the
//! [`ProposalSource`] trait.

pub mod local;
pub mod rollup;
pub mod supernode;
pub mod supervisor;

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
    /// The output root being proposed.
    pub root: B256,
    /// Block number (pre-interop) or timestamp (interop super-root).
    pub sequence_num: u64,
    /// Set for super-root proposals (interop).
    pub super_root_marshalled: Option<Vec<u8>>,
    /// The L1 block this proposal is anchored to.
    pub current_l1: L1BlockRef,
    /// Single-source extra data; only useful for logs/metrics.
    pub legacy: LegacyProposalData,
}

impl Proposal {
    pub fn is_super_root(&self) -> bool {
        self.super_root_marshalled.is_some()
    }

    /// `ExtraData()` from upstream.
    ///
    /// For super-root proposals this is the marshalled super root. For
    /// pre-interop proposals it's a 32-byte big-endian sequence number.
    pub fn extra_data(&self) -> Vec<u8> {
        if let Some(super_root) = &self.super_root_marshalled {
            super_root.clone()
        } else {
            let mut buf = [0u8; 32];
            buf[24..].copy_from_slice(&self.sequence_num.to_be_bytes());
            buf.to_vec()
        }
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
    #[error("no available sync status sources")]
    NoSyncStatusSources,
    #[error("other: {0}")]
    Other(String),
}
