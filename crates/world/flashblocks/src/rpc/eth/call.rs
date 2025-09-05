use reth_optimism_rpc::OpEthApi;
use reth_rpc_eth_api::helpers::{estimate::EstimateCall, Call, EthCall};

use crate::rpc::eth::FlashblocksEthApi;

impl<N, Rpc> EthCall for FlashblocksEthApi<N, Rpc>
where
    N: reth_rpc_eth_api::RpcNodeCore,
    Rpc: reth_rpc_eth_api::RpcConvert,
    OpEthApi<N, Rpc>: EthCall + Clone,
{
}

impl<N, Rpc> EstimateCall for FlashblocksEthApi<N, Rpc>
where
    N: reth_rpc_eth_api::RpcNodeCore,
    Rpc: reth_rpc_eth_api::RpcConvert,
    OpEthApi<N, Rpc>: EstimateCall + Clone,
{
}

impl<N, Rpc> Call for FlashblocksEthApi<N, Rpc>
where
    N: reth_rpc_eth_api::RpcNodeCore,
    Rpc: reth_rpc_eth_api::RpcConvert,
    OpEthApi<N, Rpc>: Call + Clone,
{
    #[inline]
    fn call_gas_limit(&self) -> u64 {
        self.inner.call_gas_limit()
    }

    #[inline]
    fn max_simulate_blocks(&self) -> u64 {
        self.inner.max_simulate_blocks()
    }
}
