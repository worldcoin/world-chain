//! Local, in-process proposal source backed by the ExEx node's state.
//!
//! Computes the OP `OutputV0` root directly from the L2 block header. Post-
//! Isthmus, the L2ToL1MessagePasser storage root lives in the header's
//! `withdrawalsRoot` field, so the output root reduces to three header
//! reads — no state-proof RPC and no state-trie walk required.
//!
//! ```text
//! output_root = keccak256(
//!     version_zero_32 ||
//!     header.state_root ||
//!     header.withdrawals_root ||  (== L2ToL1MessagePasser storage root, post-Isthmus)
//!     header.block_hash,
//! )
//! ```
//!
//! ## Spec
//!
//! The substitution is mandated by the [Isthmus L2 block header
//! spec][isthmus-spec]:
//!
//! > After Isthmus activation, the `withdrawalsRoot` header field is
//! > re-purposed to hold the storage root of the
//! > `L2ToL1MessagePasser` predeploy account (rather than being unused, as
//! > on canonical Ethereum L1 post-Shanghai).
//!
//! The corresponding upstream implementation lives in
//! [`op-service/sources/l2_client.go`][l2-client] L192–L227 (function
//! `outputV0`): if `IsIsthmus(block.Time())` the header field is read
//! directly, otherwise it falls back to an `eth_getProof` against
//! `L2ToL1MessagePasser`. **This ExEx is post-Isthmus**, so we only need
//! the header branch.
//!
//! The OutputV0 marshal layout (128 B buffer: version || state_root ||
//! message_passer_storage_root || block_hash) is defined in
//! [`op-service/eth/output.go`][output-go] L49–L63.
//!
//! [isthmus-spec]:
//!     https://github.com/ethereum-optimism/specs/blob/main/specs/protocol/isthmus/exec-engine.md
//! [l2-client]:
//!     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-service/sources/l2_client.go#L192-L227
//! [output-go]:
//!     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-service/eth/output.go#L49-L63

use std::sync::Arc;

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, keccak256};
use async_trait::async_trait;

use super::{Proposal, ProposalSource, ProposalSourceError, SyncStatus};

/// `L2ToL1MessagePasser` predeploy address
/// (`0x4200000000000000000000000000000000000016`).
///
/// Kept as a public constant for callers that still want to do the
/// pre-Isthmus state-proof path; the post-Isthmus header-field path does
/// not use it.
pub const L2_TO_L1_MESSAGE_PASSER: Address = Address::new(
    *b"\x42\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x16",
);

/// Abstraction over the local node state required by [`LocalProposalSource`].
///
/// Implementors typically delegate to an ExEx `FullNodeComponents::provider()`
/// and call the underlying `BlockReader` and `BlockIdReader` traits. Keeping
/// the surface small avoids leaking reth-storage-api into the rest of this
/// crate.
#[async_trait]
pub trait LocalStorageReader: Send + Sync {
    /// `(state_root, withdrawals_root, block_hash)` for the given L2 block.
    /// Post-Isthmus, `withdrawals_root` *is* the L2ToL1MessagePasser
    /// storage root.
    async fn block_meta(&self, block_number: u64) -> Result<BlockMeta, ProposalSourceError>;

    /// Latest safe / finalized L2 block numbers.
    async fn chain_status(&self) -> Result<ChainStatus, ProposalSourceError>;
}

/// L2 block header fields needed to compute an `OutputV0`.
#[derive(Debug, Clone, Copy)]
pub struct BlockMeta {
    pub state_root: B256,
    /// Header `withdrawalsRoot` field. Post-Isthmus this is the storage
    /// root of the `L2ToL1MessagePasser` predeploy. See the module-level
    /// spec reference.
    pub withdrawals_root: B256,
    pub block_hash: B256,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ChainStatus {
    pub safe_l2: u64,
    pub finalized_l2: u64,
}

pub struct LocalProposalSource {
    access: Arc<dyn LocalStorageReader>,
}

impl LocalProposalSource {
    pub fn new(access: Arc<dyn LocalStorageReader>) -> Self {
        Self { access }
    }

    /// Compute an `OutputV0` output root from raw components.
    ///
    /// Mirrors: `(*OutputV0).Marshal` + `OutputRoot` in
    /// [output.go L49–L63][src]. The 128-byte buffer layout (32 B version ||
    /// 32 B state_root || 32 B message_passer_storage_root || 32 B
    /// block_hash) is identical.
    ///
    /// [src]:
    ///     https://github.com/ethereum-optimism/optimism/blob/op-proposer/v1.16.3-rc.1/op-service/eth/output.go#L49-L63
    pub fn output_root_v0(
        state_root: B256,
        message_passer_storage_root: B256,
        block_hash: B256,
    ) -> B256 {
        let mut buf = [0u8; 128];
        // buf[0..32] is the zero version
        buf[32..64].copy_from_slice(state_root.as_slice());
        buf[64..96].copy_from_slice(message_passer_storage_root.as_slice());
        buf[96..128].copy_from_slice(block_hash.as_slice());
        keccak256(buf)
    }
}

#[async_trait]
impl ProposalSource for LocalProposalSource {
    async fn proposal_at_block(&self, block_number: u64) -> Result<Proposal, ProposalSourceError> {
        let meta = self.access.block_meta(block_number).await?;
        let root = Self::output_root_v0(meta.state_root, meta.withdrawals_root, meta.block_hash);
        Ok(Proposal {
            root,
            block_number,
            block_hash: meta.block_hash,
            current_l1: BlockNumHash::default(),
        })
    }

    async fn sync_status(&self) -> Result<SyncStatus, ProposalSourceError> {
        let s = self.access.chain_status().await?;
        Ok(SyncStatus {
            current_l1: BlockNumHash::default(),
            safe_l2: s.safe_l2,
            finalized_l2: s.finalized_l2,
        })
    }

    async fn close(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity-check the `OutputV0` layout matches the upstream Go
    /// `(*OutputV0).Marshal` byte ordering.
    #[test]
    fn output_root_layout_matches_op_v0() {
        let state_root = B256::repeat_byte(0xaa);
        let storage_root = B256::repeat_byte(0xbb);
        let block_hash = B256::repeat_byte(0xcc);

        let mut expected = [0u8; 128];
        // version (zeroed)
        expected[32..64].copy_from_slice(&[0xaa; 32]);
        expected[64..96].copy_from_slice(&[0xbb; 32]);
        expected[96..128].copy_from_slice(&[0xcc; 32]);
        let want = alloy_primitives::keccak256(expected);

        assert_eq!(
            LocalProposalSource::output_root_v0(state_root, storage_root, block_hash),
            want,
        );
    }
}
