//! Loads and formats OP block RPC response.

use reth_optimism_rpc::OpEthApiError;
use reth_rpc_eth_api::{
    helpers::{EthBlocks, LoadBlock},
    FromEvmError, RpcConvert, RpcNodeCore,
};

use crate::rpc::eth::FlashblocksEthApi;

impl<T> EthBlocks for FlashblocksEthApi<T>
where
    T: EthBlocks + Clone,
{
}

impl<T> LoadBlock for FlashblocksEthApi<T>
where
    T: LoadBlock + Clone,
{
}
