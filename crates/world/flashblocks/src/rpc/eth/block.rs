//! Loads and formats OP block RPC response.

use reth_optimism_rpc::OpEthApiError;
use reth_rpc_eth_api::{
    helpers::{EthBlocks, LoadBlock},
    FromEvmError, RpcConvert, RpcNodeCore,
};

use crate::rpc::eth::FlashblocksEthApi;

impl<N, Rpc> EthBlocks for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    OpEthApiError: FromEvmError<N::Evm>,
    Rpc: RpcConvert<Primitives = N::Primitives, Error = OpEthApiError>,
{
}

impl<N, Rpc> LoadBlock for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    OpEthApiError: FromEvmError<N::Evm>,
    Rpc: RpcConvert<Primitives = N::Primitives, Error = OpEthApiError>,
{
}
