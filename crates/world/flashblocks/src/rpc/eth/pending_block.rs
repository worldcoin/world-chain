//! Loads OP pending block for a RPC response.

use std::sync::Arc;

use reth_optimism_rpc::OpEthApiError;
use reth_primitives::RecoveredBlock;
use reth_rpc_eth_api::{
    helpers::{pending_block::PendingEnvBuilder, LoadPendingBlock},
    FromEvmError, RpcConvert, RpcNodeCore,
};
use reth_rpc_eth_types::PendingBlock;
use reth_storage_api::{ProviderBlock, ProviderReceipt};

use crate::rpc::eth::FlashblocksEthApi;

impl<N, Rpc> LoadPendingBlock for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    OpEthApiError: FromEvmError<N::Evm>,
    Rpc: RpcConvert<Primitives = N::Primitives>,
{
    #[inline]
    fn pending_block(&self) -> &tokio::sync::Mutex<Option<PendingBlock<N::Primitives>>> {
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
}
