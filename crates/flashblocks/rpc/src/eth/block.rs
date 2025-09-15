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
use std::{future::Future, sync::Arc};
use world_chain_provider::InMemoryState;

use crate::eth::FlashblocksEthApi;

impl<N, Rpc> EthBlocks for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>, Primitives = OpPrimitives>,
    Rpc: RpcConvert,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: EthBlocks
        + RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>, Primitives = OpPrimitives>
        + EthApiTypes<Error = OpEthApiError>
        + Clone,
{
}

impl<N, Rpc> LoadBlock for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>, Primitives = OpPrimitives>,
    Rpc: RpcConvert,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: LoadBlock
        + RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>, Primitives = OpPrimitives>
        + EthApiTypes<Error = OpEthApiError>
        + Clone,
{
    /// Returns the block object for the given block id.
    #[expect(clippy::type_complexity)]
    fn recovered_block(
        &self,
        block_id: BlockId,
    ) -> impl Future<
        Output = Result<
            Option<Arc<RecoveredBlock<<Self::Provider as BlockReader>::Block>>>,
            Self::Error,
        >,
    > + Send {
        async move {
            if block_id.is_pending() {
                // Pending block can be fetched directly without need for caching
                if let Some(pending_block) = self
                    .provider()
                    .pending_block()
                    .map_err(Self::Error::from_eth_err)?
                {
                    return Ok(Some(Arc::new(pending_block)));
                }

                // If no pending block from provider, try to get local pending block
                return match self.local_pending_block().await? {
                    Some((block, _)) => Ok(Some(block)),
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

            let pending_block = self
                .provider()
                .pending_block()
                .map_err(Self::Error::from_eth_err)?;

            if let Some(pending_block) = pending_block {
                // If the requested block hash matches the pending block, return it
                if pending_block.hash() == block_hash {
                    return Ok(Some(Arc::new(pending_block)));
                }
            }

            self.cache()
                .get_recovered_block(block_hash)
                .await
                .map_err(Self::Error::from_eth_err)
        }
    }
}
