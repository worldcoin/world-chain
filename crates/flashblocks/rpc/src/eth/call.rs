use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_rpc_eth_api::{
    helpers::{estimate::EstimateCall, Call, EthCall},
    EthApiTypes, FromEvmError, RpcConvert, RpcNodeCore,
};
use world_chain_provider::InMemoryState;

use crate::eth::FlashblocksEthApi;

impl<N, Rpc> EthCall for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>, Primitives = OpPrimitives>,
    Rpc: RpcConvert,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: EthCall
        + RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>, Primitives = OpPrimitives>
        + EthApiTypes<Error = OpEthApiError>
        + Clone,
{
}

impl<N, Rpc> EstimateCall for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>, Primitives = OpPrimitives>,
    Rpc: RpcConvert,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: EstimateCall
        + EthApiTypes<Error = OpEthApiError>
        + RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>, Primitives = OpPrimitives>
        + Clone,
{
}

impl<N, Rpc> Call for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>, Primitives = OpPrimitives>,
    Rpc: RpcConvert,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: Call
        + RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>, Primitives = OpPrimitives>
        + EthApiTypes<Error = OpEthApiError>
        + Clone,
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
