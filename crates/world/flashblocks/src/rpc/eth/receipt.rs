use reth_optimism_rpc::OpEthApiError;
use reth_rpc_eth_api::{helpers::LoadReceipt, RpcConvert, RpcNodeCore};

use crate::rpc::eth::OpEthApi;

impl<N, Rpc> LoadReceipt for OpEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert<Primitives = N::Primitives, Error = OpEthApiError>,
{
}
