//! Loads OP pending block for a RPC response.

use alloy_eips::{BlockId, BlockNumberOrTag};
use reth_chain_state::ExecutedBlock;
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
                    pending_block.execution_output.into(),
                ),
            ));
        }

        self.inner.pending_block_env_and_cfg()
    }

    async fn local_pending_state(&self) -> Result<Option<StateProviderBox>, Self::Error> {
        let Some(pending_block) = self.flashblock_pending_block(BlockId::pending())? else {
            return self.inner.local_pending_state().await;
        };

        self.provider()
            .state_with_block_appended(pending_block.recovered_block.parent_hash, pending_block)
            .map(Some)
            .map_err(Self::Error::from_eth_err)
    }

    /// Returns the locally built pending block
    async fn local_pending_block(
        &self,
    ) -> Result<Option<BlockAndReceipts<<N as RpcNodeCore>::Primitives>>, Self::Error> {
        if let Some(pending_block) = self.flashblock_pending_block(BlockId::pending())? {
            return Ok(Some(BlockAndReceipts {
                block: pending_block.recovered_block,
                receipts: pending_block.execution_output.into(),
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

        let block_and_receipts = BlockAndReceipts::new(block.into(), receipts.into());

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
        if let Some(pending_block) = self.flashblock_state_block(at)? {
            // Execution calls only need the header, not a copy of every pending receipt.
            let env = self.evm_env_for_header(pending_block.recovered_block.sealed_header())?;
            return Ok((env, pending_block.recovered_block.hash().into()));
        }

        LoadState::evm_env_at(&self.inner, at).await
    }

    async fn state_at_block_id(&self, at: BlockId) -> Result<StateProviderBox, Self::Error> {
        if let Some(pending_block) = self.flashblock_state_block(at)? {
            return self
                .provider()
                .state_with_block_appended(pending_block.recovered_block.parent_hash, pending_block)
                .map_err(Self::Error::from_eth_err);
        }

        LoadState::state_at_block_id(&self.inner, at).await
    }

    async fn spawn_blocking_io_with_state<F, R>(&self, at: BlockId, f: F) -> Result<R, Self::Error>
    where
        F: FnOnce(Self, StateProviderBox) -> Result<R, Self::Error> + Send + 'static,
        R: Send + 'static,
    {
        if let Some(pending_block) = self.flashblock_state_block(at)? {
            return self
                .spawn_blocking_io(move |this| {
                    if (at.is_pending() || this.pending_request)
                        && !this.pending_extends_tip(&pending_block)?
                    {
                        return Err(EthApiError::HeaderNotFound(BlockId::pending()).into());
                    }
                    let state = this
                        .provider()
                        .state_with_block_appended(
                            pending_block.recovered_block.parent_hash,
                            pending_block,
                        )
                        .map_err(Self::Error::from_eth_err)?;
                    f(this, state)
                })
                .await;
        }

        let this = self.clone();
        self.inner
            .spawn_blocking_io_with_state(at, move |_, state| f(this, state))
            .await
    }
}

impl<N, Rpc, const PINNED: bool> FlashblocksEthApi<N, Rpc, PINNED>
where
    N: RpcNodeCore<Primitives = OpPrimitives>,
    Rpc: RpcConvert + Clone,
    OpEthApi<N, Rpc>: RpcNodeCore<Primitives = OpPrimitives>,
{
    fn flashblock_state_block(
        &self,
        at: BlockId,
    ) -> Result<Option<ExecutedBlock<OpPrimitives>>, OpEthApiError> {
        let pending = self.flashblock_pending_block(at)?;
        let allows_pending = at.is_pending()
            || matches!(at, BlockId::Hash(hash) if hash.require_canonical != Some(true));
        if allows_pending
            && let Some(pending) = pending
            && (at.is_pending() || at.as_block_hash() == Some(pending.recovered_block.hash()))
            && (at.is_pending() || self.pending_request || self.pending_extends_tip(&pending)?)
        {
            return Ok(Some(pending));
        }
        Ok(None)
    }

    pub(super) fn pending_extends_tip(
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
        let pending_snapshot = self.flashblock_state_block(at)?;
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use alloy_primitives::Address;
    use revm::{database::BundleAccount, state::AccountInfo};

    use super::*;

    fn assert_pending_header_not_found<T>(result: Result<T, OpEthApiError>) {
        match result {
            Err(OpEthApiError::Eth(EthApiError::HeaderNotFound(at))) if at.is_pending() => {}
            Err(error) => panic!("expected pending header not found, got {error}"),
            Ok(_) => panic!("expected pending header not found, got success"),
        }
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
        let (hash_env, hash_at) = pinned.evm_env_at(selected).await?;
        assert_eq!(hash_env, original_env);
        assert_eq!(hash_at, selected);
        assert_eq!(
            pinned
                .state_at_block_id(selected)
                .await?
                .account_nonce(&address)?,
            Some(7)
        );
        assert_eq!(
            pinned
                .spawn_blocking_io_with_state(selected, move |_, state| {
                    Ok(state.account_nonce(&address)?)
                })
                .await?,
            Some(7)
        );
        assert_eq!(
            pinned
                .load_block_and_receipts(selected)
                .await?
                .expect("retained Flashblock receipts by hash")
                .0
                .hash(),
            selected.as_block_hash().expect("selected hash")
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
        assert!(pinned.evm_env_at(canonical).await.is_err());
        assert!(
            pinned
                .spawn_blocking_io_with_state(canonical, |_, _| Ok(()))
                .await
                .is_err()
        );
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
        let (_, receipts) = api
            .load_block_and_receipts(selected)
            .await?
            .expect("live Flashblock receipts by hash");
        assert_eq!(receipts.len(), 1);
        let canonical_hash: BlockId = serde_json::from_value(serde_json::json!({
            "blockHash": selected.as_block_hash().expect("selected hash"),
            "requireCanonical": true,
        }))?;
        assert!(api.load_block_and_receipts(canonical_hash).await?.is_none());
        assert!(
            api.pin_pending(canonical_hash)?
                .recovered_block(selected)
                .await?
                .is_none(),
            "canonical-only selection must survive internal hash rewriting"
        );
        let canonical = node.provider.canonical_in_memory_state();
        canonical.set_pending_block(make_block(2, 9));
        let unknown_transaction = B256::repeat_byte(0xbb);
        assert!(
            api.transaction_receipt(unknown_transaction)
                .await?
                .is_none()
        );
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
            api.transaction_receipt(unknown_transaction)
                .await?
                .is_none()
        );
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
                .spawn_blocking_io_with_state(selected, |_, _| Ok(()))
                .await
                .is_err()
        );
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
        assert!(
            api.transaction_receipt(unknown_transaction)
                .await?
                .is_none()
        );
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

    #[tokio::test]
    async fn pending_state_retains_unpersisted_parent_across_reorg() -> eyre::Result<()> {
        use alloy_consensus::{Block, BlockBody};
        use alloy_eips::Decodable2718;
        use alloy_primitives::{B64, U256};
        use alloy_rpc_types_engine::{ForkchoiceState, PayloadStatusEnum};
        use op_alloy_consensus::encode_jovian_extra_data;
        use reth_chainspec::EthChainSpec;
        use reth_primitives_traits::{NodePrimitives, RecoveredBlock, SignerRecoverable};
        use reth_provider::{BlockNumReader, DatabaseProviderFactory};
        use reth_rpc_eth_api::helpers::{EthBlocks, LoadBlock};
        use world_chain_chainspec::WorldChainSpecBuilder;
        use world_chain_node::context::WorldChainDefaultContext;
        use world_chain_test_utils::e2e_harness::setup::{
            CHAIN_SPEC, TX_SET_L1_BLOCK, WorldChainTestBuilder, build_payload_attributes,
            encode_eip1559_params,
        };

        let chain_spec = WorldChainSpecBuilder::default()
            .chain(CHAIN_SPEC.chain)
            .genesis(
                CHAIN_SPEC
                    .genesis
                    .clone()
                    .with_base_fee(Some(1_000_000_000))
                    .with_extra_data(encode_jovian_extra_data(
                        B64::ZERO,
                        CHAIN_SPEC.base_fee_params_at_timestamp(0),
                        0,
                    )?),
            )
            .with_forks(CHAIN_SPEC.hardforks.clone())
            .build();
        let (_, mut nodes, _tasks, _env, _spammer) = WorldChainTestBuilder::builder()
            .nodes(2)
            .flashblocks(false)
            .chain_spec(Arc::new(chain_spec))
            .build()
            .setup_with::<WorldChainDefaultContext, _>(|timestamp| {
                build_payload_attributes(
                    timestamp,
                    encode_eip1559_params(&*CHAIN_SPEC, timestamp).expect("test parameters"),
                    Some(vec![TX_SET_L1_BLOCK.clone()]),
                )
            })
            .await?;
        let genesis = nodes[0]
            .node
            .inner
            .provider
            .latest_header()?
            .expect("genesis");
        let payload = nodes[0].node.new_payload().await?;
        let replacement_payload = nodes[1].node.new_payload().await?;
        let node = &nodes[0].node.inner;
        let status = node
            .add_ons_handle
            .beacon_engine_handle
            .new_payload(payload.into())
            .await?;
        assert!(
            matches!(status.status, PayloadStatusEnum::Valid),
            "{status:?}"
        );
        let parent_hash = status.latest_valid_hash.expect("accepted parent hash");
        let choice = node
            .add_ons_handle
            .beacon_engine_handle
            .fork_choice_updated(
                ForkchoiceState {
                    head_block_hash: parent_hash,
                    safe_block_hash: genesis.hash(),
                    finalized_block_hash: genesis.hash(),
                },
                None,
            )
            .await?;
        assert!(matches!(
            choice.payload_status.status,
            PayloadStatusEnum::Valid
        ));
        let parent = node.provider.latest_header()?.expect("imported parent");
        assert_eq!(parent.number, 1);
        assert_eq!(
            node.provider.database_provider_ro()?.last_block_number()?,
            0
        );
        let transaction =
            <OpPrimitives as NodePrimitives>::SignedTx::decode_2718(&mut TX_SET_L1_BLOCK.as_ref())?;
        let sender = transaction.recover_signer()?;
        let parent_nonce = node
            .provider
            .state_by_block_hash(parent.hash())?
            .account_nonce(&sender)?;
        assert!(parent_nonce.is_some_and(|nonce| nonce > 0));

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
        let mut header = parent.header().clone();
        header.parent_hash = parent.hash();
        header.number += 1;
        header.timestamp += 2;
        let mut child = ExecutedBlock::<OpPrimitives> {
            recovered_block: Arc::new(RecoveredBlock::new_unhashed(
                Block::new(header, BlockBody::default()),
                Vec::new(),
            )),
            ..Default::default()
        };
        let child_address = Address::repeat_byte(0x43);
        Arc::make_mut(&mut child.execution_output)
            .state
            .state
            .insert(
                child_address,
                BundleAccount {
                    info: Some(AccountInfo {
                        nonce: 11,
                        ..Default::default()
                    }),
                    original_info: None,
                    storage: Default::default(),
                    status: Default::default(),
                },
            );
        let (_sender, receiver) = tokio::sync::watch::channel(Some(child));
        let api = FlashblocksEthApi::new(inner, Some(receiver));
        let pinned = api.pin_pending(BlockId::pending())?;
        let (_, selected) = pinned.evm_env_at(BlockId::pending()).await?;
        let hash_request = api.pin_pending(selected)?;
        let canonical_hash: BlockId = serde_json::from_value(serde_json::json!({
            "blockHash": selected.as_block_hash().expect("selected hash"),
            "requireCanonical": true,
        }))?;
        for selector in [BlockId::pending(), selected] {
            assert!(api.recovered_block(selector).await?.is_some());
            assert_eq!(
                api.load_block_and_receipts(selector)
                    .await?
                    .expect("live Flashblock receipts")
                    .0
                    .hash(),
                selected.as_block_hash().expect("selected hash")
            );
        }
        assert!(api.load_block_and_receipts(canonical_hash).await?.is_none());
        let state = pinned.state_at_block_id(BlockId::pending()).await?;
        let unread_state = pinned.state_at_block_id(BlockId::pending()).await?;
        assert_eq!(state.account_nonce(&sender)?, parent_nonce);
        assert_eq!(state.account_nonce(&child_address)?, Some(11));

        let replacement = node
            .add_ons_handle
            .beacon_engine_handle
            .new_payload(replacement_payload.into())
            .await?;
        assert!(matches!(replacement.status, PayloadStatusEnum::Valid));
        let replacement_hash = replacement.latest_valid_hash.expect("replacement hash");
        assert_ne!(replacement_hash, parent_hash);
        let response = node
            .add_ons_handle
            .beacon_engine_handle
            .fork_choice_updated(
                ForkchoiceState {
                    head_block_hash: replacement_hash,
                    safe_block_hash: genesis.hash(),
                    finalized_block_hash: genesis.hash(),
                },
                None,
            )
            .await?;
        assert!(matches!(
            response.payload_status.status,
            PayloadStatusEnum::Valid
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if node.provider.latest_header()?.expect("reorg head").hash() == replacement_hash {
                    break Ok::<_, eyre::Report>(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await??;
        assert_pending_header_not_found(pinned.state_at_block_id(BlockId::pending()).await);
        assert_pending_header_not_found(api.load_block_and_receipts(BlockId::pending()).await);
        assert!(api.recovered_block(selected).await?.is_none());
        assert!(hash_request.recovered_block(selected).await?.is_none());
        assert!(api.load_block_and_receipts(selected).await?.is_none());
        assert!(
            hash_request
                .load_block_and_receipts(selected)
                .await?
                .is_none()
        );
        assert!(api.load_block_and_receipts(canonical_hash).await?.is_none());
        assert!(api.state_at_block_id(selected).await.is_err());
        assert_eq!(state.account_nonce(&sender)?, parent_nonce);
        assert_eq!(state.account_nonce(&child_address)?, Some(11));
        assert_eq!(unread_state.account_nonce(&sender)?, parent_nonce);
        assert_eq!(unread_state.account_nonce(&child_address)?, Some(11));
        Ok(())
    }
}
