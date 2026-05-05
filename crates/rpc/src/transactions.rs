use std::error::Error;

use jsonrpsee::core::async_trait;
use reth_optimism_node::txpool::OpPooledTransaction;
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use reth_rpc_eth_api::{AsEthApiError, FromEthApiError};
use reth_rpc_eth_types::{EthApiError, utils::recover_raw_transaction};
use reth_transaction_pool::{PoolTransaction, TransactionOrigin, TransactionPool};
use revm_primitives::{B256, Bytes};
use world_chain_pool::tx::WorldChainPooledTransaction;

use crate::{core::WorldChainEthApiExt, sequencer::SequencerClient};

#[async_trait]
pub trait EthTransactionsExt {
    /// Extension of [`FromEthApiError`], with network specific errors.
    type Error: Into<jsonrpsee_types::error::ErrorObject<'static>>
        + FromEthApiError
        + AsEthApiError
        + Error
        + Send
        + Sync;

    async fn send_raw_transaction(&self, tx: Bytes) -> Result<B256, Self::Error>;
}

#[async_trait]
impl<Pool, Client> EthTransactionsExt for WorldChainEthApiExt<Pool, Client>
where
    Pool: TransactionPool<Transaction = WorldChainPooledTransaction> + Clone + 'static,
    Client: BlockReaderIdExt + StateProviderFactory + 'static,
{
    type Error = EthApiError;

    async fn send_raw_transaction(&self, tx: Bytes) -> Result<B256, Self::Error> {
        let recovered = recover_raw_transaction(&tx)?;
        let pool_transaction: WorldChainPooledTransaction =
            OpPooledTransaction::from_pooled(recovered).into();

        // submit the transaction to the pool with a `Local` origin
        let outcome = self
            .pool()
            .add_transaction(TransactionOrigin::Local, pool_transaction)
            .await
            .map_err(Self::Error::from_eth_err)?;

        if let Some(client) = self.raw_tx_forwarder().as_ref() {
            tracing::debug!( target: "rpc::eth",  "forwarding raw transaction to sequencer");
            let _ = client.forward_raw_transaction(&tx).await.inspect_err(|err| {
                        tracing::debug!(target: "rpc::eth", %err, hash=?*outcome.hash, "failed to forward raw transaction");
                    });
        }
        Ok(outcome.hash)
    }
}

impl<Pool, Client> WorldChainEthApiExt<Pool, Client>
where
    Pool: TransactionPool<Transaction = WorldChainPooledTransaction> + Clone + 'static,
    Client: BlockReaderIdExt + StateProviderFactory + 'static,
{
    pub fn new(pool: Pool, client: Client, sequencer_client: Option<SequencerClient>) -> Self {
        Self {
            pool,
            client,
            sequencer_client,
        }
    }

    pub fn provider(&self) -> &Client {
        &self.client
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    pub fn raw_tx_forwarder(&self) -> Option<&SequencerClient> {
        self.sequencer_client.as_ref()
    }
}
