//! Loads OP pending block for a RPC response.

use std::sync::Arc;

use reth_primitives::RecoveredBlock;
use reth_rpc_eth_api::helpers::{
    pending_block::PendingEnvBuilder, LoadPendingBlock, SpawnBlocking,
};
use reth_rpc_eth_types::PendingBlock;
use reth_storage_api::{ProviderBlock, ProviderReceipt};

use crate::eth::FlashblocksEthApi;

impl<T> LoadPendingBlock for FlashblocksEthApi<T>
where
    T: LoadPendingBlock + Clone,
    T: SpawnBlocking,
{
    #[inline]
    fn pending_block(&self) -> &tokio::sync::Mutex<Option<PendingBlock<T::Primitives>>> {
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
