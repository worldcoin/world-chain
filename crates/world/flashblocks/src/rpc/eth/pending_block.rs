//! Loads OP pending block for a RPC response.

use std::sync::Arc;

use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::OpEthApi;
use reth_primitives::RecoveredBlock;
use reth_rpc_eth_api::{
    helpers::{pending_block::PendingEnvBuilder, LoadPendingBlock, SpawnBlocking},
    RpcConvert, RpcNodeCore,
};
use reth_rpc_eth_types::PendingBlock;
use reth_storage_api::{ProviderBlock, ProviderReceipt};
use world_chain_provider::InMemoryState;

use crate::rpc::eth::FlashblocksEthApi;

impl<N, Rpc> LoadPendingBlock for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>>,
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>: RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>>
        + LoadPendingBlock
        + Clone
        + SpawnBlocking,
{
    #[inline]
    fn pending_block(
        &self,
    ) -> &tokio::sync::Mutex<Option<PendingBlock<<OpEthApi<N, Rpc> as RpcNodeCore>::Primitives>>>
    {
        self.inner.pending_block()
    }

    #[inline]
    fn pending_env_builder(&self) -> &dyn PendingEnvBuilder<Self::Evm> {
        self.inner.pending_env_builder()
    }

    /// Returns the locally built pending block
    async fn local_pending_block(
        &self,
    ) -> Result<
        Option<(
            Arc<RecoveredBlock<ProviderBlock<Self::Provider>>>,
            Arc<Vec<ProviderReceipt<Self::Provider>>>,
        )>,
        Self::Error,
    > {
        self.inner.local_pending_block().await
    }

    fn pending_block_kind(&self) -> reth_rpc_eth_types::builder::config::PendingBlockKind {
        self.inner.pending_block_kind()
    }
}
