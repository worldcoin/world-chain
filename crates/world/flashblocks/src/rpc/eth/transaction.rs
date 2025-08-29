//! Loads and formats OP transaction RPC response.

use crate::rpc::eth::FlashblocksEthApi;
use alloy_primitives::{Bytes, B256};
use reth_optimism_rpc::{OpEthApiError, SequencerClient};
use reth_rpc_eth_api::{
    helpers::{spec::SignersForRpc, EthTransactions, LoadTransaction},
    RpcConvert, RpcNodeCore,
};

impl<T> EthTransactions for FlashblocksEthApi<T>
where
    T: EthTransactions + Clone,
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

impl<T> LoadTransaction for FlashblocksEthApi<T> where T: LoadTransaction + Clone {}
