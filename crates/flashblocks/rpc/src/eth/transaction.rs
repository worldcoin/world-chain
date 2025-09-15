//! Loads and formats OP transaction RPC response.

use alloy_consensus::BlockHeader;
use alloy_primitives::{Bytes, TxHash, B256};
use reth_node_api::BlockBody;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::OpEthApi;
use reth_optimism_rpc::OpEthApiError;
use reth_primitives::TransactionMeta;
use reth_provider::ReceiptProvider;
use reth_provider::TransactionsProvider;
use reth_provider::{ProviderReceipt, ProviderTx};
use reth_rpc_eth_api::helpers::LoadPendingBlock;
use reth_rpc_eth_api::EthApiTypes;
use reth_rpc_eth_api::FromEthApiError;
use reth_rpc_eth_api::FromEvmError;
use reth_rpc_eth_api::{
    helpers::{spec::SignersForRpc, EthTransactions, LoadTransaction, SpawnBlocking},
    RpcConvert, RpcNodeCore,
};

use std::future::Future;
use tracing::info;
use world_chain_provider::InMemoryState;

use crate::eth::FlashblocksEthApi;

impl<N, Rpc> EthTransactions for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>, Primitives = OpPrimitives>,
    Rpc: RpcConvert,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: RpcNodeCore<Provider: InMemoryState<Primitives = OpPrimitives>, Primitives = OpPrimitives>
        + LoadPendingBlock
        + EthApiTypes<Error = OpEthApiError>
        + EthTransactions
        + Clone,
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
    // <OpEthApi<N, Rpc> as EthApiTypes>::Error: From<<<<<OpEthApi<N, Rpc> as RpcNodeCore>::Evm as ConfigureEvm>::BlockExecutorFactory as BlockExecutorFactory>::EvmFactory as EvmFactory>::Error<ProviderError>>
    /// Helper method that loads a transaction and its receipt.
    #[expect(clippy::complexity)]
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
            info!("Loading tx and receipt for hash: {hash:?}");
            let pending_block = this.local_pending_block().await?;
            if let Some((block, receipts)) = pending_block.clone() {
                if let Some(pos) = block
                    .clone()
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
    Rpc: RpcConvert,
    OpEthApi<N, Rpc>: LoadTransaction + Clone,
{
}
