use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_rpc_eth_api::{
    helpers::{estimate::EstimateCall, Call, EthCall},
    EthApiTypes, FromEvmError, RpcConvert, RpcNodeCore,
};

use crate::eth::FlashblocksEthApi;

impl<N, Rpc> EthCall for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert + Clone,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: EthCall
        + RpcNodeCore<Primitives = OpPrimitives>
        + EthApiTypes<Error = OpEthApiError>
        + Clone,
{
}

impl<N, Rpc> EstimateCall for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert + Clone,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: EstimateCall
        + EthApiTypes<Error = OpEthApiError>
        + RpcNodeCore<Primitives = OpPrimitives>
        + Clone,
{
}

impl<N, Rpc> Call for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert + Clone,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>:
        Call + RpcNodeCore<Primitives = OpPrimitives> + EthApiTypes<Error = OpEthApiError> + Clone,
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
