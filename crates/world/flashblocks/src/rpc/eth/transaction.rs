//! Loads and formats OP transaction RPC response.

use crate::rpc::eth::FlashblocksEthApi;
use alloy_primitives::{Bytes, B256};
use reth_optimism_rpc::OpEthApi;
use reth_rpc_eth_api::{
    helpers::{spec::SignersForRpc, EthTransactions, LoadTransaction},
    RpcConvert, RpcNodeCore,
};

impl<N, Rpc> EthTransactions for FlashblocksEthApi<N, Rpc>
where
    N: reth_rpc_eth_api::RpcNodeCore,
    Rpc: reth_rpc_eth_api::RpcConvert,
    crate::rpc::eth::OpEthApi<N, Rpc>: EthTransactions + Clone,
{
    fn signers(&self) -> &SignersForRpc<Self::Provider, Self::NetworkTypes> {
        self.inner.signers()
    }

    /// Decodes and recovers the transaction and submits it to the pool.
    ///
    /// Returns the hash of the transaction.
    async fn send_raw_transaction(&self, tx: Bytes) -> Result<B256, Self::Error> {
        self.inner.send_raw_transaction(tx).await
    }
}

impl<N, Rpc> LoadTransaction for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>: LoadTransaction + Clone,
{
}
