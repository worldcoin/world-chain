//! Loads and formats OP transaction RPC response.

use crate::rpc::eth::FlashblocksEthApi;
use alloy_primitives::{Bytes, B256};
use reth_optimism_rpc::{OpEthApiError, SequencerClient};
use reth_rpc_eth_api::{
    helpers::{spec::SignersForRpc, EthTransactions, LoadTransaction},
    RpcConvert, RpcNodeCore,
};

impl<N, Rpc> EthTransactions for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert<Primitives = N::Primitives, Error = OpEthApiError>,
{
    fn signers(&self) -> &SignersForRpc<Self::Provider, Self::NetworkTypes> {
        todo!()
        // self.inner.eth_api.signers()
    }

    /// Decodes and recovers the transaction and submits it to the pool.
    ///
    /// Returns the hash of the transaction.
    async fn send_raw_transaction(&self, tx: Bytes) -> Result<B256, Self::Error> {
        todo!()
        // let recovered = recover_raw_transaction(&tx)?;
        //
        // // broadcast raw transaction to subscribers if there is any.
        // self.eth_api().broadcast_raw_transaction(tx.clone());
        //
        // let pool_transaction = <Self::Pool as TransactionPool>::Transaction::from_pooled(recovered);
        //
        // // On optimism, transactions are forwarded directly to the sequencer to be included in
        // // blocks that it builds.
        // if let Some(client) = self.raw_tx_forwarder().as_ref() {
        //     tracing::debug!(target: "rpc::eth", hash = %pool_transaction.hash(), "forwarding raw transaction to sequencer");
        //     let hash = client.forward_raw_transaction(&tx).await.inspect_err(|err| {
        //             tracing::debug!(target: "rpc::eth", %err, hash=% *pool_transaction.hash(), "failed to forward raw transaction");
        //         })?;
        //
        //     // Retain tx in local tx pool after forwarding, for local RPC usage.
        //     let _ = self.inner.eth_api.add_pool_transaction(pool_transaction).await.inspect_err(|err| {
        //         tracing::warn!(target: "rpc::eth", %err, %hash, "successfully sent tx to sequencer, but failed to persist in local tx pool");
        //     });
        //
        //     return Ok(hash);
        // }
        //
        // // submit the transaction to the pool with a `Local` origin
        // let AddedTransactionOutcome { hash, .. } = self
        //     .pool()
        //     .add_transaction(TransactionOrigin::Local, pool_transaction)
        //     .await
        //     .map_err(Self::Error::from_eth_err)?;
        //
        // Ok(hash)
    }
}

impl<N, Rpc> LoadTransaction for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert<Primitives = N::Primitives, Error = OpEthApiError>,
{
}

impl<N, Rpc> FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert<Primitives = N::Primitives>,
{
    /// Returns the [`SequencerClient`] if one is set.
    pub fn raw_tx_forwarder(&self) -> Option<SequencerClient> {
        self.inner.raw_tx_forwarder()
    }
}
