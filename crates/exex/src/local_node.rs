//! Implementation of [`LocalChainAccess`](crate::source::local::LocalChainAccess)
//! over a reth ExEx node provider.
//!
//! Splitting this off into its own module keeps the broad reth storage-api
//! trait bounds isolated; the rest of the crate only depends on the small
//! [`LocalChainAccess`](crate::source::local::LocalChainAccess) trait.

use std::sync::Arc;

use alloy_eips::BlockId;
use alloy_primitives::{Address, B256};
use async_trait::async_trait;
use reth_exex::ExExContext;
use reth_node_api::FullNodeComponents;
use alloy_consensus::BlockHeader;
use reth_storage_api::{
    BlockHashReader, BlockIdReader, HeaderProvider, StateProofProvider, StateProviderFactory,
};

use crate::source::{
    ProposalSourceError,
    local::{BlockMeta, ChainStatus, LocalChainAccess},
};

/// Adapter that wraps an ExEx node provider and implements
/// [`LocalChainAccess`].
pub struct ExExLocalAccess<N: FullNodeComponents> {
    provider: N::Provider,
}

impl<N: FullNodeComponents> ExExLocalAccess<N> {
    pub fn new(ctx: &ExExContext<N>) -> Self
    where
        N::Provider: Clone,
    {
        Self {
            provider: ctx.components.provider().clone(),
        }
    }

    pub fn from_provider(provider: N::Provider) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl<N> LocalChainAccess for ExExLocalAccess<N>
where
    N: FullNodeComponents,
    N::Provider: reth_storage_api::BlockReader<Header = alloy_consensus::Header>
        + reth_storage_api::BlockHashReader
        + reth_storage_api::StateProviderFactory
        + reth_storage_api::BlockIdReader
        + Send
        + Sync
        + 'static,
{
    async fn block_meta(&self, block_number: u64) -> Result<BlockMeta, ProposalSourceError> {
        let provider = self.provider.clone();
        tokio::task::spawn_blocking(move || {
            let header = provider
                .header_by_number(block_number)
                .map_err(|e| ProposalSourceError::Other(e.to_string()))?
                .ok_or_else(|| {
                    ProposalSourceError::Other(format!("header for {block_number} not found"))
                })?;
            let block_hash = provider
                .block_hash(block_number)
                .map_err(|e| ProposalSourceError::Other(e.to_string()))?
                .ok_or_else(|| {
                    ProposalSourceError::Other(format!("hash for {block_number} not found"))
                })?;
            Ok(BlockMeta { state_root: header.state_root(), block_hash })
        })
        .await
        .map_err(|e| ProposalSourceError::Other(e.to_string()))?
    }

    async fn storage_root_at(
        &self,
        block_number: u64,
        address: Address,
    ) -> Result<B256, ProposalSourceError> {
        let provider = self.provider.clone();
        tokio::task::spawn_blocking(move || {
            let state = provider
                .state_by_block_id(BlockId::Number(alloy_eips::BlockNumberOrTag::Number(
                    block_number,
                )))
                .map_err(|e| ProposalSourceError::Other(e.to_string()))?;
            let proof = state
                .proof(Default::default(), address, &[])
                .map_err(|e| ProposalSourceError::Other(e.to_string()))?;
            Ok(proof.storage_root)
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

/// Builds a boxed [`Arc<dyn LocalChainAccess>`] from an `ExExContext`.
pub fn local_access_from_ctx<N>(ctx: &ExExContext<N>) -> Arc<dyn LocalChainAccess>
where
    N: FullNodeComponents,
    N::Provider: Clone
        + reth_storage_api::BlockReader<Header = alloy_consensus::Header>
        + reth_storage_api::BlockHashReader
        + reth_storage_api::StateProviderFactory
        + reth_storage_api::BlockIdReader
        + Send
        + Sync
        + 'static,
{
    Arc::new(ExExLocalAccess::<N>::new(ctx))
}
