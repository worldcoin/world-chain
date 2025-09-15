use alloy_consensus::{transaction::SignerRecoverable, TxReceipt};
use op_alloy_rpc_types::OpTransactionReceipt;
use reth::rpc::compat::RpcReceipt;
use reth::rpc::server_types::eth::block::BlockAndReceipts;
use reth_chainspec::ChainSpecProvider;
use reth_optimism_forks::OpHardforks;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::{OpEthApi, OpEthApiError, OpReceiptBuilder};
use reth_primitives::TransactionMeta;
use reth_provider::{ProviderReceipt, ProviderTx};
use reth_rpc_eth_api::{
    helpers::{LoadPendingBlock, LoadReceipt},
    RpcConvert, RpcNodeCore,
};
use reth_rpc_eth_api::{transaction::ConvertReceiptInput, RpcNodeCoreExt};
use reth_rpc_eth_api::{EthApiTypes, FromEthApiError, RpcTypes};
use reth_rpc_eth_types::EthApiError;
use std::{borrow::Cow, future::Future};
use tracing::info;

use crate::eth::FlashblocksEthApi;

// consider constraining the associated type `<<FlashblocksEthApi<N, Rpc> as RpcNodeCore>::Primitives as NodePrimitives>::Receipt` to `reth_optimism_primitives::OpReceipt`

impl<N, Rpc> LoadReceipt for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives, Provider: ChainSpecProvider<ChainSpec: OpHardforks>>,
    Rpc: RpcConvert<Primitives = N::Primitives>,
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
    fn build_transaction_receipt(
        &self,
        tx: ProviderTx<Self::Provider>,
        meta: TransactionMeta,
        receipt: ProviderReceipt<Self::Provider>,
    ) -> impl Future<Output = Result<RpcReceipt<Self::NetworkTypes>, Self::Error>> + Send {
        async move {
            let hash = meta.block_hash;
            info!(?hash, ?meta.index, "building transaction receipt");

            let mut is_pending = false;

            // get all receipts for the block
            let all_receipts = if let Ok(Some(receipts)) = self.cache().get_receipts(hash).await {
                Some((receipts, None))
            } else {
                // try the pending block
                let pending_block = self.local_pending_block().await?;
                if let Some(BlockAndReceipts { block, receipts }) = pending_block {
                    info!(block_hash = ?block.hash_slow(), ?hash, "checking pending block");
                    if block.hash_slow() == hash {
                        info!(block_hash = ?block.hash_slow(), ?hash, "found pending block");
                        is_pending = true;
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
            info!(?hash, ?meta.index, "found all receipts for block");
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

            let input = ConvertReceiptInput {
                tx: tx.as_recovered_ref(),
                gas_used: receipt.cumulative_gas_used() - gas_used,
                receipt: Cow::Owned(receipt),
                next_log_index,
                meta,
            };

            if !is_pending {
                return Ok(self
                    .tx_resp_builder()
                    .convert_receipts(vec![input])?
                    .pop()
                    .unwrap());
            } else {
                let block = block.expect("block is Some if is_pending is true");

                let mut l1_block_info = match reth_optimism_evm::extract_l1_info(block.body()) {
                    Ok(l1_block_info) => l1_block_info,
                    Err(err) => Err(err)?,
                };

                // We must clear this cache as different L2 transactions can have different
                // L1 costs. A potential improvement here is to only clear the cache if the
                // new transaction input has changed, since otherwise the L1 cost wouldn't.
                l1_block_info.clear_tx_l1_cost();

                let res = Ok(OpReceiptBuilder::new(
                    &self.provider().chain_spec(),
                    input,
                    &mut l1_block_info,
                )?
                .build());

                res
            }
        }
    }
}
