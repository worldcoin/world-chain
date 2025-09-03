use alloy_consensus::{Block, Sealable};
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_primitives::B256;
use jsonrpsee::{core::RpcResult, proc_macros::rpc};
use jsonrpsee_core::async_trait;
use reth_node_api::BlockBody;
use reth_node_api::NodePrimitives;
use reth_optimism_primitives::OpTransactionSigned;
use reth_rpc_eth_api::helpers::{LoadPendingBlock, SpawnBlocking};
use reth_rpc_eth_api::RpcNodeCore;
use tracing::info;

use crate::rpc::eth::FlashblocksEthApi;

#[rpc(server, client, namespace = "eth")]
pub trait FlashblocksEthApiExt<T: RpcNodeCore> {
    /// Returns information about a block by hash.
    #[method(name = "getBlockByHash")]
    async fn block_by_hash(
        &self,
        hash: B256,
        full: bool,
    ) -> RpcResult<Option<<<T as RpcNodeCore>::Primitives as NodePrimitives>::Block>>;

    /// Returns information about a block by number.
    #[method(name = "getBlockByNumber")]
    async fn block_by_number(
        &self,
        number: BlockNumberOrTag,
        full: bool,
    ) -> RpcResult<Option<Block<OpTransactionSigned>>>;

    /// Returns all transaction receipts for a given block.
    #[method(name = "getBlockReceipts")]
    async fn block_receipts(
        &self,
        block_id: BlockId,
    ) -> RpcResult<Option<Vec<<<T as RpcNodeCore>::Primitives as NodePrimitives>::Receipt>>>;

    /// Returns the receipt of a transaction by transaction hash.
    #[method(name = "getTransactionReceipt")]
    async fn transaction_receipt(
        &self,
        hash: B256,
        block: Option<BlockId>,
    ) -> RpcResult<Option<<<T as RpcNodeCore>::Primitives as NodePrimitives>::Receipt>>;
}

#[async_trait]
impl<
        T: RpcNodeCore<Primitives: NodePrimitives<Block = Block<OpTransactionSigned>>>
            + LoadPendingBlock
            + Clone
            + SpawnBlocking,
    > FlashblocksEthApiExtServer<T> for FlashblocksEthApi<T>
{
    /// Returns information about a block by hash.
    async fn block_by_hash(
        &self,
        hash: B256,
        full: bool,
    ) -> RpcResult<Option<<<T as RpcNodeCore>::Primitives as NodePrimitives>::Block>> {
        info!(target: "reth::rpc", "Fetching block by hash: {:?}", hash);
        let pending_block = self.pending_block().lock().await;
        if let Some(pending) = pending_block.clone() {
            let pending_hash = pending.executed_block.recovered_block.clone().hash_slow();
            if pending_hash == hash {
                let sealed = pending
                    .executed_block
                    .recovered_block
                    .sealed_block()
                    .clone();
                // TODO: Strip transactions
                let block = sealed.unseal();
                return Ok(Some(block));
            }
        }

        // TODO: Fetch block from inner
        Ok(None)
    }

    /// Returns information about a block by number.
    async fn block_by_number(
        &self,
        number: BlockNumberOrTag,
        full: bool,
    ) -> RpcResult<Option<Block<OpTransactionSigned>>> {
        if matches!(number, BlockNumberOrTag::Pending) {
            let pending_block = self.pending_block().lock().await;
            if let Some(pending) = pending_block.clone() {
                let sealed = pending
                    .executed_block
                    .recovered_block
                    .sealed_block()
                    .clone();
                let block = sealed.unseal();
                return Ok(Some(block));
            }
        }

        // TODO: Fetch block from inner
        Ok(None)
    }

    /// Returns all transaction receipts for a given block.
    async fn block_receipts(
        &self,
        block_id: BlockId,
    ) -> RpcResult<Option<Vec<<<T as RpcNodeCore>::Primitives as NodePrimitives>::Receipt>>> {
        todo!()
    }

    /// Returns the receipt of a transaction by transaction hash.
    async fn transaction_receipt(
        &self,
        hash: B256,
        block: Option<BlockId>,
    ) -> RpcResult<Option<<<T as RpcNodeCore>::Primitives as NodePrimitives>::Receipt>> {
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

        // TODO: Fetch receipt from inner
        Ok(None)
    }
}
