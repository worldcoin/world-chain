//! Flashblocks-aware `eth_getLogs` implementation.
//!
//! Wraps the standard `EthFilter` to override `eth_getLogs` behavior for
//! `fromBlock: "pending"` queries. The default reth filter implementation
//! only returns pending block logs when `block.number() > best_number`,
//! which can fail during flashblocks operation (e.g., when the watch channel
//! is empty and the fallback returns the latest committed block).
//!
//! This wrapper bypasses that check by directly consulting the flashblocks
//! watch channel for pending block data.

use alloy_consensus::BlockHeader;
use alloy_rpc_types_eth::{
    Filter, FilterBlockOption, FilterChanges, FilterId, Log, PendingTransactionFilterKind,
};
use async_trait::async_trait;
use flashblocks_builder::state::FlashblocksState;
use jsonrpsee::core::RpcResult;
use reth::rpc::eth::EthFilter;
use reth_optimism_primitives::OpPrimitives;
use reth_rpc_eth_api::{
    EthApiTypes, EthFilterApiServer, FullEthApiTypes, RpcNodeCore, RpcNodeCoreExt, RpcTransaction,
    helpers::{EthBlocks, LoadReceipt},
};
use reth_rpc_eth_types::logs_utils::{ProviderOrBlock, append_matching_block_logs};
use reth_storage_api::{BlockIdReader, BlockReader};
use tracing::trace;

/// A wrapper around [`EthFilter`] that overrides `eth_getLogs` to handle
/// pending flashblock logs correctly.
///
/// When the user queries `eth_getLogs` with `fromBlock: "pending"`, this
/// implementation reads the pending block directly from the flashblocks
/// watch channel and returns matching logs without the `> best_number` check
/// that causes issues with the standard implementation.
pub struct FlashblocksEthFilter<Eth: EthApiTypes> {
    inner: EthFilter<Eth>,
    pending_block: Option<FlashblocksState<OpPrimitives>>,
}

impl<Eth: EthApiTypes> FlashblocksEthFilter<Eth> {
    /// Creates a new `FlashblocksEthFilter` wrapping the given `EthFilter`.
    pub fn new(
        inner: EthFilter<Eth>,
        pending_block: Option<FlashblocksState<OpPrimitives>>,
    ) -> Self {
        Self {
            inner,
            pending_block,
        }
    }

    /// Attempts to get pending block logs from the flashblocks watch channel.
    ///
    /// Returns `Some(logs)` if a pending flashblock is available and the filter
    /// matches, or `None` if no pending block is available (caller should fall
    /// back to default behavior).
    fn pending_flashblock_logs(&self, filter: &Filter) -> Option<Vec<Log>>
    where
        Eth: RpcNodeCore<Primitives = OpPrimitives>,
        Eth::Provider: BlockReader,
    {
        let state = self.pending_block.as_ref()?;
        let pending = state.pending_block()?;

        let block = &pending.recovered_block;
        let receipts: &[_] = &pending.receipts;

        let block_num_hash = block.num_hash();
        let timestamp = block.timestamp();

        let mut all_logs = Vec::new();
        append_matching_block_logs(
            &mut all_logs,
            ProviderOrBlock::<Eth::Provider>::Block(block.clone()),
            filter,
            block_num_hash,
            &receipts,
            false,
            timestamp,
        )
        .ok()?;

        Some(all_logs)
    }
}

impl<Eth: EthApiTypes> Clone for FlashblocksEthFilter<Eth> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            pending_block: self.pending_block.clone(),
        }
    }
}

impl<Eth: EthApiTypes> std::fmt::Debug for FlashblocksEthFilter<Eth> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlashblocksEthFilter")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl<Eth> EthFilterApiServer<RpcTransaction<Eth::NetworkTypes>> for FlashblocksEthFilter<Eth>
where
    Eth: FullEthApiTypes<Provider: BlockReader + BlockIdReader>
        + RpcNodeCore<Primitives = OpPrimitives>
        + RpcNodeCoreExt
        + LoadReceipt
        + EthBlocks
        + 'static,
{
    async fn new_filter(&self, filter: Filter) -> RpcResult<FilterId> {
        EthFilterApiServer::new_filter(&self.inner, filter).await
    }

    async fn new_block_filter(&self) -> RpcResult<FilterId> {
        EthFilterApiServer::new_block_filter(&self.inner).await
    }

    async fn new_pending_transaction_filter(
        &self,
        kind: Option<PendingTransactionFilterKind>,
    ) -> RpcResult<FilterId> {
        EthFilterApiServer::new_pending_transaction_filter(&self.inner, kind).await
    }

    async fn filter_changes(
        &self,
        id: FilterId,
    ) -> RpcResult<FilterChanges<RpcTransaction<Eth::NetworkTypes>>> {
        EthFilterApiServer::filter_changes(&self.inner, id).await
    }

    async fn filter_logs(&self, id: FilterId) -> RpcResult<Vec<Log>> {
        EthFilterApiServer::filter_logs(&self.inner, id).await
    }

    async fn uninstall_filter(&self, id: FilterId) -> RpcResult<bool> {
        EthFilterApiServer::uninstall_filter(&self.inner, id).await
    }

    /// Returns logs matching given filter object.
    ///
    /// Overrides the default `eth_getLogs` to handle `fromBlock: "pending"` by
    /// reading directly from the flashblocks watch channel instead of relying on
    /// reth's default pending block resolution which requires `block.number() > best_number`.
    async fn logs(&self, filter: Filter) -> RpcResult<Vec<Log>> {
        trace!(target: "flashblocks", "Serving eth_getLogs");

        // Check if this is a pending block range query
        if let FilterBlockOption::Range {
            from_block,
            to_block,
        } = &filter.block_option
        {
            let from_pending = from_block.is_some_and(|b| b.is_pending());
            let to_pending = to_block.map_or(false, |b| b.is_pending());

            if from_pending || to_pending {
                // Try to get logs from the flashblocks pending block
                if let Some(logs) = self.pending_flashblock_logs(&filter) {
                    return Ok(logs);
                }
                // No pending flashblock available — fall through to default behavior
            }
        }

        // Delegate to the inner EthFilter for all other cases
        EthFilterApiServer::logs(&self.inner, filter).await
    }
}
