use reth_optimism_rpc::OpEthApi;
use reth_rpc_eth_api::{
    helpers::{LoadPendingBlock, LoadReceipt},
    RpcConvert, RpcNodeCore,
};
use reth_rpc_eth_api::{EthApiTypes, FromEthApiError};

use crate::rpc::eth::FlashblocksEthApi;

impl<N, Rpc> LoadReceipt for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>: LoadReceipt + Clone,
    Self: LoadPendingBlock
        + EthApiTypes<
            RpcConvert: RpcConvert<
                Primitives = Self::Primitives,
                Error = Self::Error,
                Network = Self::NetworkTypes,
            >,
            Error: FromEthApiError,
        >,
{
}
