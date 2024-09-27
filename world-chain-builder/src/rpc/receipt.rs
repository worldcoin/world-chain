//! Loads and formats OP receipt RPC response.

use crate::rpc::WorldChainEthApi;
use reth_chainspec::ChainSpec;
use reth_node_api::{FullNodeComponents, NodeTypes};
use reth_optimism_rpc::OpEthApiError;
use reth_primitives::{Receipt, TransactionMeta, TransactionSigned};
use reth_rpc_eth_api::helpers::{EthApiSpec, LoadReceipt, LoadTransaction};
use reth_rpc_eth_types::EthStateCache;
use reth_rpc_types::AnyTransactionReceipt;

impl<N> LoadReceipt for WorldChainEthApi<N>
where
    Self: EthApiSpec + LoadTransaction<Error = OpEthApiError>,
    N: FullNodeComponents<Types: NodeTypes<ChainSpec = ChainSpec>>,
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
    ) -> Result<AnyTransactionReceipt, Self::Error> {
        self.inner
            .build_transaction_receipt(tx, meta, receipt)
            .await
    }
}
