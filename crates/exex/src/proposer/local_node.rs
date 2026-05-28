//! Implementation of [`LocalStorageReader`](crate::proposer::source::local::LocalStorageReader)
//! over a reth ExEx node provider.
//!
//! Splitting this off into its own module keeps the broad reth storage-api
//! trait bounds isolated; the rest of the crate only depends on the small
//! [`LocalStorageReader`](crate::proposer::source::local::LocalStorageReader) trait.
//!
//! Post-Isthmus, the L2ToL1MessagePasser storage root is carried in the
//! header's `withdrawalsRoot` field — see the spec & rationale in
//! [`crate::proposer::source::local`]. This adapter therefore only does header
//! lookups; no state-trie reads are involved.

use std::sync::Arc;

use alloy_consensus::{BlockHeader, Header};
use async_trait::async_trait;
use reth_exex::ExExContext;
use reth_node_api::FullNodeComponents;
use reth_storage_api::{BlockIdReader, BlockReader, HeaderProvider};

use crate::proposer::source::{
    ProposalSourceError,
    local::{BlockMeta, ChainStatus, LocalStorageReader},
};

/// Amortized trait bounds for a node provider that this ExEx can read from.
///
/// `N::Provider` must satisfy this anywhere we need to construct or wire up
/// the local proposal source. The trait collapses what used to be a
/// 6-line `where` clause repeated across `ExExChainReader`'s impl,
/// `op_proposer_exex`, and `install_op_proposer_exex` down to a single
/// bound `N::Provider: ProviderBounds`.
///
/// The blanket impl below means callers don't need to implement anything —
/// any provider that already satisfies the constituent traits is
/// automatically `ProviderBounds`.
pub trait ProviderBounds:
    BlockReader<Header = Header> + BlockIdReader + Clone + Send + Sync + 'static
{
}

impl<T> ProviderBounds for T where
    T: BlockReader<Header = Header> + BlockIdReader + Clone + Send + Sync + 'static
{
}

/// Adapter that wraps an ExEx node provider and implements
/// [`LocalStorageReader`].
pub struct ExExChainReader<N: FullNodeComponents> {
    provider: N::Provider,
}

impl<N: FullNodeComponents> ExExChainReader<N>
where
    N::Provider: ProviderBounds,
{
    pub fn new(ctx: &ExExContext<N>) -> Self {
        Self {
            provider: ctx.components.provider().clone(),
        }
    }
}

#[async_trait]
impl<N> LocalStorageReader for ExExChainReader<N>
where
    N: FullNodeComponents,
    N::Provider: ProviderBounds,
{
    async fn block_meta(&self, block_number: u64) -> Result<BlockMeta, ProposalSourceError> {
        let provider = self.provider.clone();
        tokio::task::spawn_blocking(move || {
            // Single fetch: `sealed_header` returns the header bound to its
            // hash, so we don't need a separate `block_hash` lookup.
            let sealed = provider
                .sealed_header(block_number)
                .map_err(|e| ProposalSourceError::Other(e.to_string()))?
                .ok_or_else(|| {
                    ProposalSourceError::Other(format!("header for {block_number} not found"))
                })?;
            // Post-Isthmus: `withdrawalsRoot` on the L2 header *is* the
            // L2ToL1MessagePasser account storage root. If the header was
            // produced pre-Isthmus (or pre-Shanghai) the field will be
            // `None`/empty; this proposer is post-Isthmus and treats that
            // as a hard error so we don't silently submit a wrong root.
            let withdrawals_root = sealed.withdrawals_root().ok_or_else(|| {
                ProposalSourceError::Other(format!(
                    "header for block {block_number} has no withdrawals_root \
                     (pre-Isthmus block — this proposer requires post-Isthmus)"
                ))
            })?;
            Ok(BlockMeta {
                state_root: sealed.state_root(),
                withdrawals_root,
                block_hash: sealed.hash(),
            })
        })
        .await
        .map_err(|e| ProposalSourceError::Other(e.to_string()))?
    }

    async fn chain_status(&self) -> Result<ChainStatus, ProposalSourceError> {
        let provider = self.provider.clone();
        tokio::task::spawn_blocking(move || {
            let safe = provider
                .safe_block_num_hash()
                .map_err(|e| ProposalSourceError::Other(e.to_string()))?
                .unwrap_or_default();
            let finalized = provider
                .finalized_block_num_hash()
                .map_err(|e| ProposalSourceError::Other(e.to_string()))?
                .unwrap_or_default();
            Ok(ChainStatus {
                safe_l2: safe.number,
                finalized_l2: finalized.number,
            })
        })
        .await
        .map_err(|e| ProposalSourceError::Other(e.to_string()))?
    }
}

/// Builds an `Arc<dyn LocalStorageReader>` from an `ExExContext`.
pub fn local_reader_from_ctx<N>(ctx: &ExExContext<N>) -> Arc<dyn LocalStorageReader>
where
    N: FullNodeComponents,
    N::Provider: ProviderBounds,
{
    Arc::new(ExExChainReader::<N>::new(ctx))
}
