//! Loads and formats OP transaction RPC response.

use alloy_consensus::BlockHeader;
use alloy_primitives::{B256, Bytes, TxHash};
use reth_node_api::BlockBody;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_primitives::TransactionMeta;
use reth_provider::{ProviderReceipt, ProviderTx, ReceiptProvider, TransactionsProvider};
use reth_rpc_eth_api::{
    EthApiTypes, FromEthApiError, FromEvmError, RpcConvert, RpcNodeCore,
    helpers::{
        EthTransactions, LoadPendingBlock, LoadTransaction, SpawnBlocking, spec::SignersForRpc,
    },
};
use reth_rpc_eth_types::block::BlockAndReceipts;

use std::{future::Future, time::Duration};

use crate::eth::FlashblocksEthApi;

impl<N, Rpc> EthTransactions for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert + Clone,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: RpcNodeCore<Primitives = OpPrimitives>
        + LoadPendingBlock
        + EthApiTypes<Error = OpEthApiError>
        + EthTransactions
        + Clone,
{
    fn signers(&self) -> &SignersForRpc<Self::Provider, Self::NetworkTypes> {
        self.inner.signers()
    }

    // TODO: FIXME:
    fn send_transaction(
        &self,
        tx: alloy_eips::eip2718::WithEncoded<
            reth_primitives::Recovered<reth_transaction_pool::PoolPooledTx<Self::Pool>>,
        >,
    ) -> impl Future<Output = Result<B256, Self::Error>> + Send {
        unimplemented!("TODO:")
    }

    /// Decodes and recovers the transaction and submits it to the pool.
    ///
    /// Returns the hash of the transaction.
    async fn send_raw_transaction(&self, tx: Bytes) -> Result<B256, Self::Error> {
        self.inner.send_raw_transaction(tx).await
    }

    fn send_raw_transaction_sync_timeout(&self) -> Duration {
        self.inner.send_raw_transaction_sync_timeout()
    }

    /// Helper method that loads a transaction and its receipt.
    fn load_transaction_and_receipt(
        &self,
        hash: TxHash,
    ) -> impl Future<
        Output = Result<
            Option<(
                ProviderTx<Self::Provider>,
                TransactionMeta,
                ProviderReceipt<Self::Provider>,
            )>,
            Self::Error,
        >,
    > + Send
    where
        Self: 'static,
    {
        self.spawn_blocking_io_fut(async move |this| {
            let pending_block = this.local_pending_block().await?;
            if let Some(BlockAndReceipts { block, receipts }) = pending_block.clone()
                && let Some(pos) = block
                    .body()
                    .transactions_iter()
                    .position(|t| *t.tx_hash() == hash)
            {
                let receipt = &receipts[pos];
                let tx = block
                    .clone()
                    .body()
                    .transactions_iter()
                    .nth(pos)
                    .expect("position is valid; qed")
                    .clone();

                let meta = TransactionMeta {
                    tx_hash: tx.tx_hash(),
                    block_hash: block.hash_slow(),
                    block_number: block.number(),
                    index: pos as u64,
                    base_fee: block.base_fee_per_gas(),
                    timestamp: block.header().timestamp(),
                    ..Default::default()
                };

                return Ok(Some((tx, meta, receipt.clone())));
            }

            let provider = this.provider();

            let (tx, meta) = match provider
                .transaction_by_hash_with_meta(hash)
                .map_err(Self::Error::from_eth_err)?
            {
                Some((tx, meta)) => (tx, meta),
                None => return Ok(None),
            };

            let receipt = match provider
                .receipt_by_hash(hash)
                .map_err(Self::Error::from_eth_err)?
            {
                Some(recpt) => recpt,
                None => return Ok(None),
            };

            Ok(Some((tx, meta, receipt)))
        })
    }
}

impl<N, Rpc> LoadTransaction for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore,
    Rpc: RpcConvert + Clone,
    OpEthApi<N, Rpc>: LoadTransaction + Clone,
{
}
