//! Loads and formats OP block RPC response.

use crate::rpc::WorldChainEthApi;
use reth_chainspec::ChainSpec;
use reth_node_api::{FullNodeComponents, NodeTypes};
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_provider::{BlockReaderIdExt, HeaderProvider};
use reth_rpc_eth_api::helpers::{EthBlocks, LoadBlock, LoadPendingBlock, LoadReceipt};
use reth_rpc_eth_types::EthStateCache;
use reth_rpc_types::{AnyTransactionReceipt, BlockId};

impl<N> EthBlocks for WorldChainEthApi<N>
where
    Self: LoadBlock<Error = OpEthApiError>,
    N: FullNodeComponents<Types: NodeTypes<ChainSpec = ChainSpec>>,
{
    #[inline]
    fn provider(&self) -> impl HeaderProvider {
        EthBlocks::provider(&self.inner)
    }

    async fn block_receipts(
        &self,
        block_id: BlockId,
    ) -> Result<Option<Vec<AnyTransactionReceipt>>, Self::Error>
    where
        Self: LoadReceipt,
    {
        self.inner.block_receipts(block_id).await
    }
}

impl<N> LoadBlock for WorldChainEthApi<N>
where
    Self: LoadPendingBlock,
    OpEthApi<N>: LoadPendingBlock,
    N: FullNodeComponents,
{
    #[inline]
    fn provider(&self) -> impl BlockReaderIdExt {
        LoadBlock::provider(&self.inner)
    }

    #[inline]
    fn cache(&self) -> &EthStateCache {
        LoadBlock::cache(&self.inner)
    }
}
