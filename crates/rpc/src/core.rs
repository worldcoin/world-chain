use crate::{EthTransactionsExt, sequencer::SequencerClient};
use alloy_primitives::{B256, Bytes};
use jsonrpsee::{
    core::{RpcResult, async_trait},
    proc_macros::rpc,
};
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
use reth_transaction_pool::TransactionPool;
use world_chain_pool::tx::WorldChainPooledTransaction;

/// WorldChainEthApi Extension for `sendRawTransactionConditional` and `sendRawTransaction`
#[derive(Clone, Debug)]
pub struct WorldChainEthApiExt<Pool, Client> {
    pub(crate) pool: Pool,
    pub(crate) client: Client,
    pub(crate) sequencer_client: Option<SequencerClient>,
}

#[cfg_attr(not(test), rpc(server, namespace = "eth"))]
#[cfg_attr(test, rpc(server, client, namespace = "eth"))]
#[async_trait]
pub trait EthApiExt {
    /// Sends a raw transaction to the pool
    #[method(name = "sendRawTransaction")]
    async fn send_raw_transaction(&self, tx: Bytes) -> RpcResult<B256>;
}

#[async_trait]
impl<Pool, Client> EthApiExtServer for WorldChainEthApiExt<Pool, Client>
where
    Pool: TransactionPool<Transaction = WorldChainPooledTransaction> + Clone + 'static,
    Client: BlockReaderIdExt + StateProviderFactory + 'static,
{
    async fn send_raw_transaction(&self, tx: Bytes) -> RpcResult<B256> {
        Ok(EthTransactionsExt::send_raw_transaction(self, tx).await?)
    }
}
