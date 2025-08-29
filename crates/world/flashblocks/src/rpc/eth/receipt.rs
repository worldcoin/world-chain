use reth_rpc_eth_api::helpers::LoadReceipt;

use crate::rpc::eth::FlashblocksEthApi;

impl<T> LoadReceipt for FlashblocksEthApi<T>
where
    T: LoadReceipt + Clone,
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
