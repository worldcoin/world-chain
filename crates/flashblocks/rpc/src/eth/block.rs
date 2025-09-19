//! Loads and formats OP block RPC response.

use alloy_eips::BlockId;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_primitives::RecoveredBlock;
use reth_provider::{BlockIdReader, BlockReader};
use reth_rpc_eth_api::{helpers::LoadPendingBlock, FromEthApiError, RpcNodeCoreExt};
use reth_rpc_eth_api::{
    helpers::{EthBlocks, LoadBlock},
    RpcConvert, RpcNodeCore,
};
use reth_rpc_eth_api::{EthApiTypes, FromEvmError};
use reth_rpc_eth_types::block::BlockAndReceipts;
use std::sync::Arc;

use crate::eth::FlashblocksEthApi;

impl<N, Rpc> EthBlocks for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: EthBlocks
        + RpcNodeCore<Primitives = OpPrimitives>
        + EthApiTypes<Error = OpEthApiError>
        + Clone,
{
}

impl<N, Rpc> LoadBlock for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: LoadBlock
        + RpcNodeCore<Primitives = OpPrimitives>
        + EthApiTypes<Error = OpEthApiError>
        + Clone,
{
    /// Returns the block object for the given block id.
    async fn recovered_block(
        &self,
        block_id: BlockId,
    ) -> Result<Option<Arc<RecoveredBlock<<Self::Provider as BlockReader>::Block>>>, Self::Error>
    {
        if block_id.is_pending() {
            // If no pending block from provider, try to get local pending block
            return match self.local_pending_block().await? {
                Some(BlockAndReceipts { block, receipts: _ }) => Ok(Some(block)),
                None => Ok(None),
            };
        }

        let block_hash = match self
            .provider()
            .block_hash_for_id(block_id)
            .map_err(Self::Error::from_eth_err)?
        {
            Some(block_hash) => block_hash,
            None => return Ok(None),
        };

        let pending_block = self.local_pending_block().await?;

        if let Some(BlockAndReceipts { block, receipts: _ }) = pending_block {
            // If the requested block hash matches the pending block, return it
            if block.hash() == block_hash {
                return Ok(Some(block));
            }
        }

        self.cache()
            .get_recovered_block(block_hash)
            .await
            .map_err(Self::Error::from_eth_err)
    }
}
