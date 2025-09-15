//! Loads OP pending block for a RPC response.

use alloy_eips::BlockNumberOrTag;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::OpEthApi;
use reth_optimism_rpc::OpEthApiError;
use reth_primitives::RecoveredBlock;
use reth_provider::BlockReader;
use reth_provider::BlockReaderIdExt;
use reth_provider::ReceiptProvider;
use reth_rpc_eth_api::EthApiTypes;
use reth_rpc_eth_api::FromEvmError;
use reth_rpc_eth_api::{
    helpers::{pending_block::PendingEnvBuilder, LoadPendingBlock, SpawnBlocking},
    RpcConvert, RpcNodeCore,
};
use reth_rpc_eth_types::{EthApiError, PendingBlock};
use reth_storage_api::{ProviderBlock, ProviderReceipt};
use std::sync::Arc;
use world_chain_provider::InMemoryState;

use crate::eth::FlashblocksEthApi;

impl<N, Rpc> LoadPendingBlock for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>, Primitives = OpPrimitives>,
    Rpc: RpcConvert,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>, Primitives = OpPrimitives>
        + LoadPendingBlock
        + Clone
        + SpawnBlocking
        + EthApiTypes<Error = OpEthApiError>,
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
        let pending_block = self
            .provider()
            .in_memory_state()
            .pending_block_and_receipts();

        if let Some((block, receipts)) = pending_block {
            return Ok(Some((Arc::new(block), Arc::new(receipts))));
        }

        // See: <https://github.com/ethereum-optimism/op-geth/blob/f2e69450c6eec9c35d56af91389a1c47737206ca/miner/worker.go#L367-L375>
        let latest = self
            .provider()
            .latest_header()?
            .ok_or(EthApiError::HeaderNotFound(BlockNumberOrTag::Latest.into()))?;
        let block_id = latest.hash().into();
        let block = self
            .provider()
            .recovered_block(block_id, Default::default())?
            .ok_or(EthApiError::HeaderNotFound(block_id.into()))?;

        let receipts = self
            .provider()
            .receipts_by_block(block_id)?
            .ok_or(EthApiError::ReceiptsNotFound(block_id.into()))?;

        Ok(Some((Arc::new(block), Arc::new(receipts))))
    }

    fn pending_block_kind(&self) -> reth_rpc_eth_types::builder::config::PendingBlockKind {
        self.inner.pending_block_kind()
    }
}
