//! Loads OP pending block for a RPC response.

use alloy_eips::BlockNumberOrTag;
use reth_chain_state::{BlockState, ExecutedBlock};
use reth_errors::RethError;
use reth_evm::ConfigureEvm;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_provider::{
    BlockReader, BlockReaderIdExt, ReceiptProvider, StateProviderBox, StateProviderFactory,
};
use reth_rpc_eth_api::{
    EthApiTypes, FromEthApiError, FromEvmError, RpcConvert, RpcNodeCore,
    helpers::{LoadPendingBlock, SpawnBlocking, pending_block::PendingEnvBuilder},
};
use reth_rpc_eth_types::{
    EthApiError, PendingBlock, PendingBlockEnv, PendingBlockEnvOrigin, block::BlockAndReceipts,
};

use crate::eth::FlashblocksEthApi;

impl<N, Rpc> LoadPendingBlock for FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert + Clone,
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: RpcNodeCore<Primitives = OpPrimitives>
        + LoadPendingBlock
        + Clone
        + SpawnBlocking
        + EthApiTypes<Error = OpEthApiError>,
{
    #[inline]
    fn pending_block(
        &self,
    ) -> &tokio::sync::Mutex<Option<PendingBlock<<OpEthApi<N, Rpc> as RpcNodeCore>::Primitives>>>
    {
        self.inner.pending_block()
    }

    #[inline]
    fn pending_env_builder(&self) -> &dyn PendingEnvBuilder<Self::Evm> {
        self.inner.pending_env_builder()
    }

    fn pending_block_env_and_cfg(&self) -> Result<PendingBlockEnv<Self::Evm>, Self::Error> {
        if let Some(pending_block) = self.flashblock_pending_block() {
            let evm_env = self
                .evm_config()
                .evm_env(pending_block.recovered_block.header())
                .map_err(RethError::other)
                .map_err(Self::Error::from_eth_err)?;

            return Ok(PendingBlockEnv::new(
                evm_env,
                PendingBlockEnvOrigin::ActualPending(
                    pending_block.recovered_block,
                    pending_block.execution_output.receipts.clone().into(),
                ),
            ));
        }

        self.inner.pending_block_env_and_cfg()
    }

    async fn local_pending_state(&self) -> Result<Option<StateProviderBox>, Self::Error> {
        let Some(pending_block) = self.flashblock_pending_block() else {
            return self.inner.local_pending_state().await;
        };

        let parent_state = self
            .provider()
            .state_by_block_hash(pending_block.recovered_block.parent_hash)
            .map_err(Self::Error::from_eth_err)?;

        Ok(Some(flashblock_state_provider(pending_block, parent_state)))
    }

    /// Returns the locally built pending block
    async fn local_pending_block(
        &self,
    ) -> Result<Option<BlockAndReceipts<<N as RpcNodeCore>::Primitives>>, Self::Error> {
        // check the pending block from the executor
        if let Some(pending_block) = self.pending_block.as_ref() {
            let pending_block = pending_block.borrow().clone();

            if let Some(pending_block) = pending_block {
                let block = pending_block.recovered_block;
                let receipts = pending_block
                    .execution_output
                    .receipts
                    .clone()
                    .into_iter()
                    .collect::<Vec<_>>(); // always a single block executed through the state executor

                let block_and_receipts = BlockAndReceipts {
                    block,
                    receipts: receipts.into(),
                };
                return Ok(Some(block_and_receipts));
            }
        }

        // See: <https://github.com/ethereum-optimism/op-geth/blob/f2e69450c6eec9c35d56af91389a1c47737206ca/miner/worker.go#L367-L375>
        let latest = self
            .provider()
            .latest_header()?
            .ok_or(EthApiError::HeaderNotFound(BlockNumberOrTag::Latest.into()))?;
        let block_id = latest.hash().into();
        let block = self
            .provider()
            .recovered_block(block_id, Default::default())?
            .ok_or(EthApiError::HeaderNotFound(block_id.into()))?;

        let receipts = self
            .provider()
            .receipts_by_block(block_id)?
            .ok_or(EthApiError::ReceiptsNotFound(block_id.into()))?;

        let block_and_receipts = BlockAndReceipts {
            block: block.into(),
            receipts: receipts.into(),
        };

        Ok(Some(block_and_receipts))
    }

    fn pending_block_kind(&self) -> reth_rpc_eth_types::builder::config::PendingBlockKind {
        self.inner.pending_block_kind()
    }
}

impl<N, Rpc> FlashblocksEthApi<N, Rpc>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert + Clone,
{
    fn flashblock_pending_block(&self) -> Option<ExecutedBlock<OpPrimitives>> {
        self.pending_block.as_ref()?.borrow().clone()
    }
}

fn flashblock_state_provider(
    pending_block: ExecutedBlock<OpPrimitives>,
    parent_state: StateProviderBox,
) -> StateProviderBox {
    Box::new(BlockState::new(pending_block).state_provider(parent_state))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use alloy_primitives::Address;
    use reth_provider::{StateProvider, noop::NoopProvider};
    use revm::{database::BundleAccount, state::AccountInfo};

    use super::*;

    #[test]
    fn flashblock_state_overlays_historical_state() {
        let address = Address::random();
        let mut pending_block = ExecutedBlock::<OpPrimitives>::default();
        Arc::make_mut(&mut pending_block.execution_output)
            .state
            .state
            .insert(
                address,
                BundleAccount {
                    info: Some(AccountInfo {
                        nonce: 7,
                        ..Default::default()
                    }),
                    original_info: None,
                    storage: Default::default(),
                    status: Default::default(),
                },
            );

        let state = flashblock_state_provider(pending_block, Box::new(NoopProvider::default()));

        assert_eq!(state.account_nonce(&address).unwrap(), Some(7));
    }
}
