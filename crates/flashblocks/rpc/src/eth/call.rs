use reth_rpc_eth_api::helpers::{estimate::EstimateCall, Call, EthCall};

use crate::eth::FlashblocksEthApi;

impl<T> EthCall for FlashblocksEthApi<T> where T: EthCall + Clone {}

impl<T> EstimateCall for FlashblocksEthApi<T> where T: EstimateCall + Clone {}

impl<T> Call for FlashblocksEthApi<T>
where
    T: Call + Clone,
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
