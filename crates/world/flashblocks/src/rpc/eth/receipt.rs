use reth_optimism_rpc::OpEthApi;
use reth_rpc_eth_api::{helpers::LoadReceipt, RpcConvert, RpcNodeCore};

use crate::rpc::eth::FlashblocksEthApi;

impl<N, Rpc> LoadReceipt for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>: LoadReceipt + Clone,
{
    // TODO:
    // fn build_transaction_receipt(
    //         &self,
    //         tx: reth_provider::ProviderTx<Self::Provider>,
    //         meta: reth_primitives::TransactionMeta,
    //         receipt: reth_provider::ProviderReceipt<Self::Provider>,
    //     ) -> impl Future<Output = Result<reth_rpc_eth_api::RpcReceipt<Self::NetworkTypes>, Self::Error>> + Send {
    //         async {
    //             let provier = self.inner.provider();
    //             Ok(RpcReceipt::default())
    //         }
    // }
}
