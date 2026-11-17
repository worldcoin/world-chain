use alloy_eips::{BlockId, eip2930::AccessListResult};
use alloy_primitives::{Bytes, U256};
use alloy_rpc_types::{
    Bundle, EthCallResponse, StateContext,
    simulate::SimulatePayload,
    state::{EvmOverrides, StateOverride},
};
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_rpc_eth_api::{
    EthApiTypes, FromEvmError, RpcConvert, RpcNodeCore, RpcTxReq,
    helpers::{Call, EthCall, Trace, call::SimulatedBlocksResult, estimate::EstimateCall},
};

use crate::eth::FlashblocksEthApi;

impl<N, Rpc> EthCall for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert + Clone,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: EthCall
        + Trace
        + RpcNodeCore<Primitives = OpPrimitives>
        + EthApiTypes<Error = OpEthApiError>
        + Clone,
{
    async fn call(
        &self,
        request: RpcTxReq<<Self::RpcConvert as RpcConvert>::Network>,
        block: Option<BlockId>,
        overrides: EvmOverrides,
    ) -> Result<Bytes, Self::Error> {
        self.pin_pending(block.unwrap_or_default())?
            .call(request, block, overrides)
            .await
    }

    async fn estimate_gas_at(
        &self,
        request: RpcTxReq<<Self::RpcConvert as RpcConvert>::Network>,
        at: BlockId,
        overrides: EvmOverrides,
    ) -> Result<U256, Self::Error> {
        EthCall::estimate_gas_at(&self.pin_pending(at)?, request, at, overrides).await
    }

    async fn simulate_v1(
        &self,
        payload: SimulatePayload<RpcTxReq<<Self::RpcConvert as RpcConvert>::Network>>,
        block: Option<BlockId>,
    ) -> SimulatedBlocksResult<Self::NetworkTypes, Self::Error> {
        self.pin_pending(block.unwrap_or_default())?
            .simulate_v1(payload, block)
            .await
    }

    async fn call_many(
        &self,
        bundles: Vec<Bundle<RpcTxReq<<Self::RpcConvert as RpcConvert>::Network>>>,
        state_context: Option<StateContext>,
        state_override: Option<StateOverride>,
    ) -> Result<Vec<Vec<EthCallResponse>>, Self::Error> {
        self.pin_pending(
            state_context
                .as_ref()
                .and_then(|context| context.block_number)
                .unwrap_or_default(),
        )?
        .call_many(bundles, state_context, state_override)
        .await
    }

    async fn create_access_list_at(
        &self,
        request: RpcTxReq<<Self::RpcConvert as RpcConvert>::Network>,
        block: Option<BlockId>,
        state_override: Option<StateOverride>,
    ) -> Result<AccessListResult, Self::Error> {
        self.pin_pending(block.unwrap_or_default())?
            .create_access_list_at(request, block, state_override)
            .await
    }
}

impl<N, Rpc> EthCall for FlashblocksEthApi<N, Rpc, true>
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

impl<N, Rpc, const PINNED: bool> EstimateCall for FlashblocksEthApi<N, Rpc, PINNED>
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

impl<N, Rpc, const PINNED: bool> Call for FlashblocksEthApi<N, Rpc, PINNED>
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

    fn evm_memory_limit(&self) -> u64 {
        self.inner.evm_memory_limit()
    }

    #[inline]
    fn compute_state_root_for_eth_simulate(&self) -> bool {
        self.inner.compute_state_root_for_eth_simulate()
    }
}
