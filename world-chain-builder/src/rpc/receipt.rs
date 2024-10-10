//! Loads and formats OP receipt RPC response.

use crate::rpc::WorldChainEthApi;
use reth::api::{FullNodeComponents, NodeTypes};
use reth::core::rpc::eth::helpers::LoadReceipt;
use reth::{core::rpc::eth::RpcReceipt, rpc::server_types::eth::EthStateCache};
use reth_optimism_chainspec::OpChainSpec;
use reth_primitives::{Receipt, TransactionMeta, TransactionSigned};

impl<N> LoadReceipt for WorldChainEthApi<N>
where
    Self: Send + Sync,
    N: FullNodeComponents<Types: NodeTypes<ChainSpec = OpChainSpec>>,
{
    #[inline]
    fn cache(&self) -> &EthStateCache {
        LoadReceipt::cache(&self.inner)
    }

    async fn build_transaction_receipt(
        &self,
        tx: TransactionSigned,
        meta: TransactionMeta,
        receipt: Receipt,
    ) -> Result<RpcReceipt<Self::NetworkTypes>, Self::Error> {
        self.inner
            .build_transaction_receipt(tx, meta, receipt)
            .await
    }
}
