//! Loads OP pending block for a RPC response.

use alloy_eips::{BlockId, BlockNumberOrTag};
use reth_chain_state::{BlockState, ExecutedBlock};
use reth_errors::RethError;
use reth_evm::{ConfigureEvm, EvmEnvFor};
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::{OpEthApi, OpEthApiError};
use reth_provider::{
    BlockReader, BlockReaderIdExt, ReceiptProvider, StateProviderBox, StateProviderFactory,
};
use reth_rpc_eth_api::{
    EthApiTypes, FromEthApiError, FromEvmError, RpcConvert, RpcNodeCore,
    helpers::{LoadPendingBlock, LoadState, SpawnBlocking, pending_block::PendingEnvBuilder},
};
use reth_rpc_eth_types::{
    EthApiError, PendingBlock, PendingBlockEnv, PendingBlockEnvOrigin, block::BlockAndReceipts,
};

use crate::eth::FlashblocksEthApi;

impl<N, Rpc, const PINNED: bool> LoadPendingBlock for FlashblocksEthApi<N, Rpc, PINNED>
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
        if let Some(pending_block) = self.flashblock_pending_block(BlockId::pending())? {
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
        let Some(pending_block) = self.flashblock_pending_block(BlockId::pending())? else {
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
        if let Some(pending_block) = self.flashblock_pending_block(BlockId::pending())? {
            return Ok(Some(BlockAndReceipts {
                block: pending_block.recovered_block,
                receipts: pending_block.execution_output.receipts.clone().into(),
            }));
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

impl<N: RpcNodeCore<Primitives = OpPrimitives>, Rpc: RpcConvert + Clone, const PINNED: bool>
    LoadState for FlashblocksEthApi<N, Rpc, PINNED>
where
    OpEthApiError: FromEvmError<N::Evm>,
    OpEthApi<N, Rpc>: RpcNodeCore<Primitives = OpPrimitives>
        + LoadPendingBlock
        + EthApiTypes<Error = OpEthApiError>
        + LoadState
        + Clone
        + SpawnBlocking,
{
    async fn evm_env_at(
        &self,
        at: BlockId,
    ) -> Result<(EvmEnvFor<Self::Evm>, BlockId), Self::Error> {
        let pending_block = self.flashblock_pending_block(at)?;
        if at.is_pending()
            && let Some(pending_block) = pending_block
        {
            // Execution calls only need the header, not a copy of every pending receipt.
            let env = self.evm_env_for_header(pending_block.recovered_block.sealed_header())?;
            return Ok((env, pending_block.recovered_block.hash().into()));
        }

        LoadState::evm_env_at(&self.inner, at).await
    }

    async fn state_at_block_id(&self, at: BlockId) -> Result<StateProviderBox, Self::Error> {
        let pending_block = self.flashblock_pending_block(at)?;
        let allows_pending = at.is_pending()
            || matches!(at, BlockId::Hash(hash) if hash.require_canonical != Some(true));
        if allows_pending
            && let Some(pending_block) = pending_block
            && (at.is_pending() || at.as_block_hash() == Some(pending_block.recovered_block.hash()))
            && (at.is_pending()
                || self.pending_request
                || self.pending_extends_tip(&pending_block)?)
        {
            let parent_state = self
                .provider()
                .state_by_block_hash(pending_block.recovered_block.parent_hash)
                .map_err(Self::Error::from_eth_err)?;
            return Ok(flashblock_state_provider(pending_block, parent_state));
        }

        LoadState::state_at_block_id(&self.inner, at).await
    }
}

impl<N, Rpc, const PINNED: bool> FlashblocksEthApi<N, Rpc, PINNED>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert + Clone,
    OpEthApi<N, Rpc>: RpcNodeCore<Primitives = OpPrimitives>,
{
    fn pending_extends_tip(
        &self,
        pending: &ExecutedBlock<OpPrimitives>,
    ) -> Result<bool, OpEthApiError> {
        let latest = self
            .provider()
            .latest_header()?
            .ok_or(EthApiError::HeaderNotFound(BlockNumberOrTag::Latest.into()))?;
        Ok(pending.recovered_block.parent_hash == latest.hash()
            && latest.number.checked_add(1) == Some(pending.recovered_block.number))
    }

    /// Freeze the source and retain the original selector's pending policy.
    pub(super) fn pin_pending(
        &self,
        at: BlockId,
    ) -> Result<FlashblocksEthApi<N, Rpc, true>, OpEthApiError> {
        let pending_snapshot = self.flashblock_pending_block(at)?;
        Ok(FlashblocksEthApi {
            inner: self.inner.clone(),
            pending_block: None,
            pending_snapshot,
            pending_request: self.pending_request || at.is_pending(),
        })
    }

    pub(super) fn flashblock_pending_block(
        &self,
        at: BlockId,
    ) -> Result<Option<ExecutedBlock<OpPrimitives>>, OpEthApiError> {
        let pending = if PINNED {
            self.pending_snapshot.clone()
        } else {
            self.pending_block
                .as_ref()
                .and_then(|source| source.borrow().clone())
        };
        if (at.is_pending() || self.pending_request)
            && let Some(pending) = &pending
            && !self.pending_extends_tip(pending)?
        {
            return Err(EthApiError::HeaderNotFound(BlockId::pending()).into());
        }
        Ok(pending)
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

    fn assert_pending_header_not_found<T>(result: Result<T, OpEthApiError>) {
        match result {
            Err(OpEthApiError::Eth(EthApiError::HeaderNotFound(at))) if at.is_pending() => {}
            Err(error) => panic!("expected pending header not found, got {error}"),
            Ok(_) => panic!("expected pending header not found, got success"),
        }
    }

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

    #[tokio::test]
    async fn pending_requests_keep_one_snapshot_and_propagate_parent_errors() -> eyre::Result<()> {
        use alloy_consensus::{Block, BlockBody};
        use alloy_eips::Decodable2718;
        use alloy_primitives::{B256, U256};
        use reth_optimism_primitives::OpReceipt;
        use reth_primitives_traits::{NodePrimitives, RecoveredBlock, SignerRecoverable};
        use reth_rpc_eth_api::{
            EthApiTypes, RpcNodeCoreExt,
            helpers::{EthBlocks, EthTransactions, LoadBlock, LoadState},
        };
        use world_chain_node::context::WorldChainDefaultContext;
        use world_chain_test_utils::e2e_harness::setup::{TX_SET_L1_BLOCK, WorldChainTestBuilder};

        let (_, nodes, _tasks, _env, _spammer) = WorldChainTestBuilder::builder()
            .nodes(1)
            .flashblocks(false)
            .build()
            .setup::<WorldChainDefaultContext>()
            .await?;
        let node = &nodes[0].node.inner;
        let existing = node.add_ons_handle.rpc_registry.eth_api();
        let backend = reth_rpc::EthApiBuilder::new(
            node.provider.clone(),
            node.pool.clone(),
            node.network.clone(),
            node.evm_config.clone(),
        )
        .with_rpc_converter(existing.converter().clone())
        .task_spawner(node.task_executor.clone())
        .build_inner();
        let inner = OpEthApi::new(backend, None, U256::ZERO, None, false);
        let (sender, receiver) = tokio::sync::watch::channel(None);
        let api = FlashblocksEthApi::new(inner, Some(receiver));
        let no_flashblock = api.pin_pending(BlockId::pending())?;
        let parent = node.provider.latest_header()?.expect("genesis header");
        let address = Address::repeat_byte(0x42);
        let make_block = |number: u64, nonce: u64| {
            let mut header = parent.header().clone();
            header.parent_hash = parent.hash();
            header.number = 1;
            header.timestamp += number * 2;
            let block =
                RecoveredBlock::new_unhashed(Block::new(header, BlockBody::default()), Vec::new());
            let mut executed = ExecutedBlock::<OpPrimitives> {
                recovered_block: Arc::new(block),
                ..Default::default()
            };
            Arc::make_mut(&mut executed.execution_output)
                .state
                .state
                .insert(
                    address,
                    BundleAccount {
                        info: Some(AccountInfo {
                            nonce,
                            ..Default::default()
                        }),
                        original_info: None,
                        storage: Default::default(),
                        status: Default::default(),
                    },
                );
            executed
        };
        sender.send_replace(Some(make_block(1, 7)));
        let pinned = api.pin_pending(BlockId::pending())?;
        let original_env = pinned.pending_block_env_and_cfg()?.evm_env;
        let (_, selected) = api.evm_env_at(BlockId::pending()).await?;
        sender.send_replace(Some(make_block(2, 8)));
        tokio::task::yield_now().await;
        let state = pinned.state_at_block_id(BlockId::pending()).await?;
        assert_eq!(state.account_nonce(&address)?, Some(7));
        assert_eq!(pinned.pending_block_env_and_cfg()?.evm_env, original_env);
        let (env, at) = pinned.evm_env_at(BlockId::pending()).await?;
        assert_eq!(env, original_env);
        assert_eq!(at, selected);
        assert_eq!(
            pinned
                .state_at_block_id(selected)
                .await?
                .account_nonce(&address)?,
            Some(7)
        );
        assert!(
            api.state_at_block_id(selected).await.is_err(),
            "a replaced view must not resolve to newer state"
        );
        let canonical: BlockId = serde_json::from_value(serde_json::json!({
            "blockHash": selected.as_block_hash().expect("selected hash"),
            "requireCanonical": true,
        }))?;
        assert!(pinned.state_at_block_id(canonical).await.is_err());
        assert_eq!(
            pinned
                .local_pending_block()
                .await?
                .expect("pinned block")
                .block
                .number,
            1
        );
        assert_eq!(
            api.state_at_block_id(BlockId::pending())
                .await?
                .account_nonce(&address)?,
            Some(8)
        );
        assert_eq!(
            api.state_at_block_id(BlockNumberOrTag::Latest.into())
                .await?
                .account_nonce(&address)?,
            None
        );
        assert_eq!(
            no_flashblock
                .state_at_block_id(BlockId::pending())
                .await?
                .account_nonce(&address)?,
            None
        );
        sender.send_replace(None);
        assert_eq!(
            pinned
                .state_at_block_id(BlockId::pending())
                .await?
                .account_nonce(&address)?,
            Some(7)
        );
        assert_eq!(
            api.state_at_block_id(BlockId::pending())
                .await?
                .account_nonce(&address)?,
            None
        );

        let mut invalid = make_block(1, 9);
        let mut header = invalid.recovered_block.header().clone();
        header.parent_hash = B256::repeat_byte(0xaa);
        invalid.recovered_block = Arc::new(RecoveredBlock::new_unhashed(
            Block::new(header, BlockBody::default()),
            Vec::new(),
        ));
        sender.send_replace(Some(invalid));
        assert!(api.local_pending_state().await.is_err());
        assert!(api.state_at_block_id(BlockId::pending()).await.is_err());

        // Retain the Flashblock after Reth canonicalizes it, before the source clears it.
        let mut canonicalized = make_block(1, 7);
        let transaction =
            <OpPrimitives as NodePrimitives>::SignedTx::decode_2718(&mut TX_SET_L1_BLOCK.as_ref())?;
        let transaction_sender = transaction.recover_signer()?;
        let transaction_hash = transaction.tx_hash();
        canonicalized.recovered_block = Arc::new(RecoveredBlock::new_unhashed(
            Block::new(
                canonicalized.recovered_block.header().clone(),
                BlockBody {
                    transactions: vec![transaction],
                    ..Default::default()
                },
            ),
            vec![transaction_sender],
        ));
        Arc::make_mut(&mut canonicalized.execution_output)
            .receipts
            .push(OpReceipt::Deposit(Default::default()));
        sender.send_replace(Some(canonicalized.clone()));
        let pending_request = api.pin_pending(BlockId::pending())?;
        let (_, selected) = pending_request.evm_env_at(BlockId::pending()).await?;
        assert_eq!(
            api.recovered_block(BlockId::pending())
                .await?
                .expect("pending block")
                .hash(),
            selected.as_block_hash().expect("selected hash")
        );
        let canonical = node.provider.canonical_in_memory_state();
        canonical.set_pending_block(make_block(2, 9));
        assert_eq!(
            api.load_transaction_and_receipt(transaction_hash)
                .await?
                .expect("Flashblock transaction receipt")
                .1
                .block_hash,
            canonicalized.recovered_block.hash()
        );
        assert_eq!(
            api.load_block_and_receipts(BlockId::pending())
                .await?
                .expect("pending block and receipts")
                .0
                .hash(),
            selected.as_block_hash().expect("selected hash")
        );
        assert!(
            api.pin_pending(selected)?
                .recovered_block(selected)
                .await?
                .is_some()
        );
        canonical.update_chain(reth_chain_state::NewCanonicalChain::Commit {
            new: vec![canonicalized.clone()],
        });
        canonical.set_canonical_head(canonicalized.recovered_block.clone_sealed_header());
        let mut provider_pending = make_block(2, 9);
        let mut header = provider_pending.recovered_block.header().clone();
        header.parent_hash = canonicalized.recovered_block.hash();
        header.number = 2;
        provider_pending.recovered_block = Arc::new(RecoveredBlock::new_unhashed(
            Block::new(header, BlockBody::default()),
            Vec::new(),
        ));
        canonical.set_pending_block(provider_pending);
        assert_pending_header_not_found(api.recovered_block(BlockId::pending()).await);
        assert_pending_header_not_found(api.local_pending_block().await);
        assert_pending_header_not_found(api.block_transaction_count(BlockId::pending()).await);
        assert_pending_header_not_found(
            api.transaction_by_block_and_tx_index(BlockId::pending(), 0)
                .await,
        );
        assert_pending_header_not_found(api.block_receipts(BlockId::pending()).await);
        assert_pending_header_not_found(pending_request.recovered_block(selected).await);
        assert_pending_header_not_found(pending_request.load_block_and_receipts(selected).await);
        assert!(
            api.cache()
                .get_transaction_by_hash(transaction_hash)
                .await
                .is_none()
        );
        assert!(
            api.load_transaction_and_receipt(transaction_hash)
                .await?
                .is_some()
        );
        assert!(api.recovered_block(selected).await?.is_some());
        assert!(
            api.recovered_block(BlockNumberOrTag::Latest.into())
                .await?
                .is_some()
        );
        assert!(api.recovered_block(parent.hash().into()).await?.is_some());
        assert!(api.load_block_and_receipts(selected).await?.is_some());
        assert!(
            api.load_block_and_receipts(BlockNumberOrTag::Latest.into())
                .await?
                .is_some()
        );
        assert!(api.state_at_block_id(selected).await.is_ok());
        assert!(api.evm_env_at(selected).await.is_ok());
        assert!(api.state_at_block_id(BlockId::pending()).await.is_err());
        assert!(api.evm_env_at(BlockId::pending()).await.is_err());
        assert!(api.pending_block_env_and_cfg().is_err());
        assert!(api.local_pending_state().await.is_err());
        assert!(pending_request.state_at_block_id(selected).await.is_err());
        assert!(
            pending_request
                .state_at_block_id(parent.hash().into())
                .await
                .is_err()
        );
        assert!(api.pin_pending(BlockId::pending()).is_err());
        let canonical_request = api.pin_pending(selected)?;
        assert!(canonical_request.recovered_block(selected).await?.is_some());
        assert!(canonical_request.state_at_block_id(selected).await.is_ok());
        assert!(canonical_request.evm_env_at(selected).await.is_ok());
        sender.send_replace(None);
        assert_eq!(
            api.recovered_block(BlockId::pending())
                .await?
                .expect("latest fallback")
                .hash(),
            canonicalized.recovered_block.hash()
        );
        assert_eq!(
            api.load_block_and_receipts(BlockId::pending())
                .await?
                .expect("provider pending fallback")
                .0
                .number,
            2
        );
        Ok(())
    }
}
