use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::OpEthApi;
use reth_rpc_eth_api::{
    helpers::{estimate::EstimateCall, Call, EthCall},
    RpcConvert, RpcNodeCore,
};
use world_chain_provider::InMemoryState;

use crate::rpc::eth::FlashblocksEthApi;

impl<N, Rpc> EthCall for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>>,
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>:
        EthCall + Clone + RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>>,
{
}

impl<N, Rpc> EstimateCall for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>>,
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>:
        EstimateCall + RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>> + Clone,
{
}

impl<N, Rpc> Call for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>>,
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>:
        Call + RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>> + Clone,
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
