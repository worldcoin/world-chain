use crate::rpc::WorldChainEthApi;
use reth_chainspec::ChainSpec;
use reth_evm::ConfigureEvm;
use reth_node_api::{EthApiTypes, FullNodeComponents, NodeTypes};
use reth_optimism_rpc::OpEthApi;
use reth_primitives::{
    revm_primitives::{BlockEnv, TxEnv},
    Header,
};
use reth_rpc_eth_api::helpers::{Call, EthCall, LoadState, SpawnBlocking};
use reth_rpc_types::TransactionRequest;

impl<N> EthCall for WorldChainEthApi<N>
where
    Self: Call,
    N: FullNodeComponents<Types: NodeTypes<ChainSpec = ChainSpec>>,
    Self::Error: From<<OpEthApi<N> as EthApiTypes>::Error>,
{
}

impl<N> Call for WorldChainEthApi<N>
where
    Self: LoadState + SpawnBlocking,
    N: FullNodeComponents,
    OpEthApi<N>: Call,
    Self::Error: From<<OpEthApi<N> as EthApiTypes>::Error>,
{
    #[inline]
    fn call_gas_limit(&self) -> u64 {
        self.inner.call_gas_limit()
    }

    #[inline]
    fn max_simulate_blocks(&self) -> u64 {
        self.inner.max_simulate_blocks()
    }

    #[inline]
    fn evm_config(&self) -> &impl ConfigureEvm<Header = Header> {
        self.inner.evm_config()
    }

    fn create_txn_env(
        &self,
        block_env: &BlockEnv,
        request: TransactionRequest,
    ) -> Result<TxEnv, Self::Error> {
        Ok(self.inner.create_txn_env(block_env, request)?)
    }
}
