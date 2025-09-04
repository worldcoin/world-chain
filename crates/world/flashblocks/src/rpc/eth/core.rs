use alloy_consensus::Sealable;
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_primitives::B256;
use jsonrpsee::{
    core::{async_trait, RpcResult},
    proc_macros::rpc,
};
use op_alloy_network::Optimism;
use op_alloy_rpc_types::OpTransactionReceipt;
use reth_node_api::BlockBody;
use reth_node_api::NodePrimitives;
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_rpc_eth_api::{
    helpers::{
        EthBlocks, EthTransactions, LoadBlock, LoadPendingBlock, LoadReceipt, LoadTransaction,
    },
    EthApiTypes, RpcBlock, RpcConvert, RpcTypes,
};
use reth_rpc_eth_api::{RpcNodeCore, RpcReceipt};
use tracing::trace;

use crate::rpc::eth::FlashblocksEthApi;

#[cfg_attr(not(test), rpc(server, namespace = "eth"))]
#[cfg_attr(test, rpc(server, client, namespace = "eth"))]
#[async_trait]
pub trait FlashblocksEthApiExt {
    /// Returns information about a block by hash.
    #[method(name = "getBlockByHash")]
    async fn block_by_hash(&self, hash: B256, full: bool) -> RpcResult<Option<RpcBlock<Optimism>>>;

    // /// Returns information about a block by number.
    // #[method(name = "getBlockByNumber")]
    // async fn block_by_number(
    //     &self,
    //     number: BlockNumberOrTag,
    //     full: bool,
    // ) -> RpcResult<Option<RpcBlock<Optimism>>>;

    // /// Returns all transaction receipts for a given block.
    // #[method(name = "getBlockReceipts")]
    // async fn block_receipts(
    //     &self,
    //     block_id: BlockId,
    // ) -> RpcResult<Option<Vec<RpcReceipt<Optimism>>>>;

    /// Returns the receipt of a transaction by transaction hash.
    #[method(name = "getTransactionReceipt")]
    async fn transaction_receipt(
        &self,
        hash: B256,
        block: Option<BlockId>,
    ) -> RpcResult<Option<RpcReceipt<Optimism>>>;
}

#[async_trait]
impl<N: RpcNodeCore, Rpc: RpcConvert<Error = OpEthApiError>> FlashblocksEthApiExtServer
    for FlashblocksEthApi<N, Rpc>
where
    FlashblocksEthApi<N, Rpc>: LoadPendingBlock
        + LoadReceipt
        + LoadBlock<Error: Into<jsonrpsee_types::error::ErrorObject<'static>>>
        + EthApiTypes<
            NetworkTypes: RpcTypes<
                TransactionResponse = op_alloy_rpc_types::Transaction,
                Header = reth::rpc::types::Header,
                Receipt = OpTransactionReceipt,
            >,
            Error: Into<jsonrpsee_types::error::ErrorObject<'static>>,
        > + RpcNodeCore<Primitives: NodePrimitives<Receipt = OpTransactionReceipt>>
        + LoadTransaction,
    OpEthApi<N, Rpc>: EthBlocks + EthTransactions,
{
    /// Returns information about a block by hash.
    async fn block_by_hash(&self, hash: B256, full: bool) -> RpcResult<Option<RpcBlock<Optimism>>> {
        trace!(target: "flashblocks::rpc", "Fetching block by hash: {:?}", hash);
        if let Some(pending) = self
            .recovered_block(BlockId::pending())
            .await
            .map_err(Into::into)?
        {
            let pending_hash = pending.hash_slow();
            if pending_hash == hash {
                let block = pending
                    .clone_into_rpc_block(
                        full.into(),
                        |tx, tx_info| self.tx_resp_builder().fill(tx, tx_info),
                        |header, size| self.tx_resp_builder().convert_header(header, size),
                    )
                    .map_err(Into::into)?;

                return Ok(Some(block));
            }
        }

        Ok(EthBlocks::rpc_block(self, hash.into(), full)
            .await
            .map_err(Into::into)?)
    }

    // /// Returns information about a block by number.
    // async fn block_by_number(
    //     &self,
    //     number: BlockNumberOrTag,
    //     full: bool,
    // ) -> RpcResult<Option<RpcBlock<Optimism>>> {
    //     todo!();
    //     // if matches!(number, BlockNumberOrTag::Pending) {
    //     //     let pending_block = self.pending_block().lock().await;
    //     //     if let Some(pending) = pending_block.clone() {
    //     //         let sealed = pending
    //     //             .executed_block
    //     //             .recovered_block
    //     //             .sealed_block()
    //     //             .clone();
    //     //         let block = sealed.unseal();
    //     //         return Ok(Some(block));
    //     //     }
    //     // }

    //     // TODO: Fetch block from inner
    //     Ok(None)
    // }

    // /// Returns all transaction receipts for a given block.
    // async fn block_receipts(
    //     &self,
    //     block_id: BlockId,
    // ) -> RpcResult<Option<Vec<RpcReceipt<Optimism>>>> {
    //     todo!();
    // }

    /// Returns the receipt of a transaction by transaction hash.
    async fn transaction_receipt(
        &self,
        hash: B256,
        block: Option<BlockId>,
    ) -> RpcResult<Option<RpcReceipt<Optimism>>> {
        if let Some(id) = block {
            if matches!(id, BlockId::Number(BlockNumberOrTag::Pending)) {
                let pending_block = self.pending_block().lock().await;
                if let Some(pending) = pending_block.clone() {
                    let pending_receipts = pending.receipts;
                    let recovered = pending.executed_block.recovered_block.clone();
                    if let Some(pos) = recovered
                        .clone()
                        .body()
                        .transaction_hashes_iter()
                        .position(|h| *h == hash)
                    {
                        return Ok(Some(pending_receipts[pos].clone()));
                    }
                }
            }
        }

        Ok(EthTransactions::transaction_receipt(self, hash)
            .await
            .map_err(Into::into)?)
    }
}
