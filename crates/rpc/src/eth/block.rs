//! Loads and formats OP block RPC response.

use alloy_eips::BlockId;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_primitives_traits::RecoveredBlock;
use reth_provider::{BlockIdReader, BlockReader};
use reth_rpc_eth_api::{
    EthApiTypes, FromEthApiError, FromEvmError, RpcConvert, RpcNodeCore, RpcNodeCoreExt,
    helpers::{EthBlocks, LoadBlock, LoadPendingBlock, block::BlockAndReceiptsResult},
};
use reth_rpc_eth_types::block::BlockAndReceipts;
use std::sync::Arc;

use crate::eth::FlashblocksEthApi;

impl<N, Rpc, const PINNED: bool> EthBlocks for FlashblocksEthApi<N, Rpc, PINNED>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert + Clone,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: EthBlocks
        + RpcNodeCore<Primitives = OpPrimitives>
        + EthApiTypes<Error = OpEthApiError>
        + Clone,
{
    async fn load_block_and_receipts(&self, block_id: BlockId) -> BlockAndReceiptsResult<Self> {
        let pending_block = self.flashblock_pending_block(block_id)?;
        if block_id.is_pending() {
            if self.pending_block_kind().is_none() {
                return Ok(None);
            }

            // Select the Flashblock before the provider's independent pending candidate.
            if let Some(pending_block) = pending_block {
                return Ok(Some((
                    pending_block.recovered_block,
                    pending_block.execution_output.into(),
                )));
            }

            if let Some(pending) = self
                .provider()
                .pending_block_and_receipts()
                .map_err(Self::Error::from_eth_err)?
            {
                let (block, output) = pending.into_parts();
                return Ok(Some((block, output.into())));
            }

            if let Some(pending) = self.local_pending_block().await? {
                return Ok(Some((pending.block, pending.receipts)));
            }
        }

        let Some(block_hash) = self
            .provider()
            .block_hash_for_id(block_id)
            .map_err(Self::Error::from_eth_err)?
        else {
            return Ok(None);
        };
        self.cache()
            .get_block_and_receipts(block_hash)
            .await
            .map(|result| result.map(|(block, receipts)| (block, receipts.into())))
            .map_err(Self::Error::from_eth_err)
    }
}

impl<N, Rpc, const PINNED: bool> LoadBlock for FlashblocksEthApi<N, Rpc, PINNED>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert + Clone,
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
        let pending_block = self.flashblock_pending_block(block_id)?;
        if block_id.is_pending() {
            if let Some(pending_block) = pending_block {
                return Ok(Some(pending_block.recovered_block));
            }
            // Preserve the local fallback when no Flashblock is available.
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

        if let Some(pending_block) = pending_block
            && pending_block.recovered_block.hash() == block_hash
        {
            return Ok(Some(pending_block.recovered_block));
        }

        self.cache()
            .get_recovered_block(block_hash)
            .await
            .map_err(Self::Error::from_eth_err)
    }
}
