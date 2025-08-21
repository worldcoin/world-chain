// use crate::{eth::RpcNodeCore, OpEthApi, OpEthApiError};
use reth_evm::TxEnvFor;
use reth_optimism_rpc::OpEthApiError;
use reth_rpc_eth_api::{
    helpers::{estimate::EstimateCall, Call, EthCall},
    FromEvmError, RpcConvert, RpcNodeCore,
};

use crate::rpc::eth::FlashblocksEthApi;

impl<N, Rpc> EthCall for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    OpEthApiError: FromEvmError<N::Evm>,
    Rpc: RpcConvert<Primitives = N::Primitives, Error = OpEthApiError, TxEnv = TxEnvFor<N::Evm>>,
{
}

impl<N, Rpc> EstimateCall for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    OpEthApiError: FromEvmError<N::Evm>,
    Rpc: RpcConvert<Primitives = N::Primitives, Error = OpEthApiError, TxEnv = TxEnvFor<N::Evm>>,
{
}

impl<N, Rpc> Call for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    OpEthApiError: FromEvmError<N::Evm>,
    Rpc: RpcConvert<Primitives = N::Primitives, Error = OpEthApiError, TxEnv = TxEnvFor<N::Evm>>,
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
