use alloy_consensus::{transaction::SignerRecoverable, TxReceipt};
use op_alloy_rpc_types::OpTransactionReceipt;
use reth::rpc::{compat::RpcReceipt, server_types::eth::block::BlockAndReceipts};
use reth_chainspec::ChainSpecProvider;
use reth_optimism_forks::OpHardforks;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::{OpEthApi, OpEthApiError, OpReceiptBuilder};
use reth_primitives::TransactionMeta;
use reth_provider::{ProviderReceipt, ProviderTx};
use reth_rpc_eth_api::{
    helpers::{LoadPendingBlock, LoadReceipt},
    transaction::ConvertReceiptInput,
    EthApiTypes, FromEthApiError, RpcConvert, RpcNodeCore, RpcNodeCoreExt, RpcTypes,
};
use reth_rpc_eth_types::EthApiError;
use tracing::trace;

use crate::eth::FlashblocksEthApi;

impl<N, Rpc> LoadReceipt for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives, Provider: ChainSpecProvider<ChainSpec: OpHardforks>>,
    Rpc: RpcConvert<Primitives = N::Primitives> + Clone,
    OpEthApi<N, Rpc>: LoadReceipt + Clone,
    Self: LoadPendingBlock
        + EthApiTypes<
            RpcConvert: RpcConvert<
                Primitives = Self::Primitives,
                Error = Self::Error,
                Network = Self::NetworkTypes,
            >,
            NetworkTypes: RpcTypes<Receipt = OpTransactionReceipt>,
            Error = OpEthApiError,
        > + RpcNodeCore<Primitives = OpPrimitives, Provider: ChainSpecProvider<ChainSpec: OpHardforks>>,
{
    /// Helper method for `eth_getBlockReceipts` and `eth_getTransactionReceipt`.
    async fn build_transaction_receipt(
        &self,
        tx: ProviderTx<Self::Provider>,
        meta: TransactionMeta,
        receipt: ProviderReceipt<Self::Provider>,
    ) -> Result<RpcReceipt<Self::NetworkTypes>, Self::Error> {
        let hash = meta.block_hash;

        // get all receipts for the block
        let all_receipts = if let Ok(Some(receipts)) = self.cache().get_receipts(hash).await {
            Some((receipts, None))
        } else {
            // try the pending block
            let pending_block = self.local_pending_block().await?;
            if let Some(BlockAndReceipts { block, receipts }) = pending_block {
                if block.hash_slow() == hash {
                    Some((receipts, Some(block)))
                } else {
                    None
                }
            } else {
                None
            }
        };

        let all_receipts = all_receipts.ok_or(EthApiError::HeaderNotFound(hash.into()))?;
        let (all_receipts, block) = all_receipts;

        let mut gas_used = 0;
        let mut next_log_index = 0;

        if meta.index > 0 {
            for receipt in all_receipts.iter().take(meta.index as usize) {
                gas_used = receipt.cumulative_gas_used();
                next_log_index += receipt.logs().len();
            }
        }
        let tx = tx
            .try_into_recovered_unchecked()
            .map_err(Self::Error::from_eth_err)?;
        let hash = tx.hash();
        trace!(
            target: "flashblocks",
            tx_hash = ?hash,
            tx_index = meta.index,
            gas_used = ?gas_used,
            receipt_gas_used = ?receipt.cumulative_gas_used(),
            "Building receipt for tx",
        );

        let input = ConvertReceiptInput {
            tx: tx.as_recovered_ref(),
            gas_used: receipt.cumulative_gas_used().saturating_sub(gas_used),
            receipt,
            next_log_index,
            meta,
        };

        match block {
            None => Ok(self
                .tx_resp_builder()
                .convert_receipts(vec![input])?
                .pop()
                .unwrap()),
            Some(block) => {
                let mut l1_block_info = match reth_optimism_evm::extract_l1_info(block.body()) {
                    Ok(l1_block_info) => l1_block_info,
                    Err(err) => Err(err)?,
                };

                // We must clear this cache as different L2 transactions can have different
                // L1 costs. A potential improvement here is to only clear the cache if the
                // new transaction input has changed, since otherwise the L1 cost wouldn't.
                l1_block_info.clear_tx_l1_cost();

                

                Ok(OpReceiptBuilder::new(
                    &self.provider().chain_spec(),
                    input,
                    &mut l1_block_info,
                )?
                .build())
            }
        }
    }
}
