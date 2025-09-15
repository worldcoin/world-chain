use crate::{sequencer::SequencerClient, EthTransactionsExt};
use alloy_primitives::{Bytes, B256};
use alloy_rpc_types::erc4337::TransactionConditional;
use jsonrpsee::{core::async_trait, core::RpcResult, proc_macros::rpc};
use reth::transaction_pool::TransactionPool;
use reth_provider::{BlockReaderIdExt, StateProviderFactory};
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

    /// Sends a raw conditional transaction to the pool
    #[method(name = "sendRawTransactionConditional")]
    async fn send_raw_transaction_conditional(
        &self,
        tx: Bytes,
        options: TransactionConditional,
    ) -> RpcResult<B256>;
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

    async fn send_raw_transaction_conditional(
        &self,
        tx: Bytes,
        options: TransactionConditional,
    ) -> RpcResult<B256> {
        Ok(EthTransactionsExt::send_raw_transaction_conditional(self, tx, options).await?)
    }
}
