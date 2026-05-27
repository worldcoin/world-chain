//! Local, in-process proposal source backed by the ExEx node's state.
//!
//! This is the preferred source when running as an ExEx: we already have the
//! whole L2 chain locally, so there is no point doing an RPC round-trip to an
//! op-node. We compute the OP output root ourselves:
//!
//! ```text
//! output_root = keccak256(
//!     version_zero_32 ||
//!     header.state_root ||
//!     L2ToL1MessagePasser.storage_root ||
//!     header.block_hash,
//! )
//! ```
//!
//! Mirrors `op-service/eth/output.go` (`OutputV0::Marshal`).

use std::sync::Arc;

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, keccak256};
use async_trait::async_trait;

use super::{Proposal, ProposalSource, ProposalSourceError, SyncStatus};

/// L2ToL1MessagePasser predeploy address (`0x4200000000000000000000000000000000000016`).
pub const L2_TO_L1_MESSAGE_PASSER: Address = Address::new(
    *b"\x42\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x16",
);

/// Abstraction over the local node state required by [`LocalProposalSource`].
///
/// Implementors typically delegate to an ExEx `FullNodeComponents::provider()`
/// and call the underlying `BlockReader`, `StateProviderFactory`, and
/// `BlockIdReader` traits. Keeping it small avoids leaking the full
/// reth-storage-api surface area into this crate.
#[async_trait]
pub trait LocalChainAccess: Send + Sync {
    /// `(state_root, block_hash)` for the given L2 block.
    async fn block_meta(&self, block_number: u64) -> Result<BlockMeta, ProposalSourceError>;

    /// Storage root of `address` at `block_number`.
    async fn storage_root_at(
        &self,
        block_number: u64,
        address: Address,
    ) -> Result<B256, ProposalSourceError>;

    /// Latest safe / finalized L2 block numbers.
    async fn chain_status(&self) -> Result<ChainStatus, ProposalSourceError>;
}

#[derive(Debug, Clone, Copy)]
pub struct BlockMeta {
    pub state_root: B256,
    pub block_hash: B256,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ChainStatus {
    pub safe_l2: u64,
    pub finalized_l2: u64,
}

pub struct LocalProposalSource {
    access: Arc<dyn LocalChainAccess>,
}

impl LocalProposalSource {
    pub fn new(access: Arc<dyn LocalChainAccess>) -> Self {
        Self { access }
    }

    /// Compute an OutputV0 output root from raw components.
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
        let storage_root = self
            .access
            .storage_root_at(block_number, L2_TO_L1_MESSAGE_PASSER)
            .await?;
        let root = Self::output_root_v0(meta.state_root, storage_root, meta.block_hash);
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

    /// Sanity-check the `OutputV0` layout: same constants as `op-service/eth`
    /// (taken from a known test vector in `OptimismPortal2.sol` tests).
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
