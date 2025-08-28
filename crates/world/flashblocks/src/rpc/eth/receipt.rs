use std::future::Future;

use reth::rpc::compat::RpcReceipt;
use reth_optimism_rpc::OpEthApiError;
use reth_provider::StateProviderFactory;
use reth_rpc_eth_api::{helpers::LoadReceipt, RpcConvert, RpcNodeCore};
use world_chain_provider::InMemoryState;

use crate::rpc::eth::FlashblocksEthApi;

impl<N, Rpc> LoadReceipt for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Provider: StateProviderFactory>,
    Rpc: RpcConvert<Primitives = N::Primitives, Error = OpEthApiError>,
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
