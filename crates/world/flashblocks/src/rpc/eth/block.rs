//! Loads and formats OP block RPC response.

use reth_rpc_eth_api::helpers::{EthBlocks, LoadBlock};

use crate::rpc::eth::FlashblocksEthApi;

impl<N, Rpc> EthBlocks for FlashblocksEthApi<N, Rpc>
where
    N: reth_rpc_eth_api::RpcNodeCore,
    Rpc: reth_rpc_eth_api::RpcConvert,
    crate::rpc::eth::OpEthApi<N, Rpc>: EthBlocks + Clone,
{
}

impl<N, Rpc> LoadBlock for FlashblocksEthApi<N, Rpc>
where
    N: reth_rpc_eth_api::RpcNodeCore,
    Rpc: reth_rpc_eth_api::RpcConvert,
    crate::rpc::eth::OpEthApi<N, Rpc>: LoadBlock + Clone,
{
}
