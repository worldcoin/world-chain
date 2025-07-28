use std::{future::Future, pin::Pin, sync::Arc};

use alloy_eips::eip7685::Requests;
use alloy_primitives::{BlockHash, B256, U64};
use alloy_rpc_types_engine::{
    ClientVersionV1, ExecutionPayloadBodiesV1, ExecutionPayloadInputV2, ExecutionPayloadV3,
    ForkchoiceState, ForkchoiceUpdated, PayloadId, PayloadStatus,
};
use futures::{Stream, StreamExt};
use jsonrpsee_core::{async_trait, server::RpcModule, RpcResult};
use op_alloy_rpc_types_engine::{
    OpExecutionData, OpExecutionPayloadV4, ProtocolVersion, SuperchainSignal,
};
use reth::{
    api::{EngineTypes, EngineValidator},
    rpc::api::IntoEngineApiRpcModule,
    tasks::TaskSpawner,
};
use reth_chainspec::EthereumHardforks;
use reth_optimism_rpc::{OpEngineApi, OpEngineApiServer};
use reth_provider::{BlockReader, HeaderProvider, StateProviderFactory};
use reth_transaction_pool::TransactionPool;
use rollup_boost::FlashblocksPayloadV1;
use tokio::sync::RwLock;

pub type Flashblocks = Vec<FlashblocksPayloadV1>;

/// The current state of all known pre confirmations received over the P2P layer
/// or generated from the payload building job of this node.
///
/// The state is flushed when FCU is received with a parent hash that matches the block hash
/// of the latest pre confirmation _or_ when an FCU is received that does not match the latest pre confirmation,
/// in which case the pre confirmations were not included as part of the canonical chain.
#[derive(Debug, Clone)]
pub struct FlashblocksState(pub Arc<RwLock<Flashblocks>>);

impl Default for FlashblocksState {
    fn default() -> Self {
        Self::new()
    }
}

impl FlashblocksState {
    /// Creates a new instance of [`FlashblocksState`].
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(Vec::new())))
    }

    /// Returns a reference to the latest flashblock.
    pub async fn last(&self) -> Option<FlashblocksPayloadV1> {
        self.0.read().await.last().cloned()
    }

    /// Appends a new flashblock to the state.
    pub async fn push(&self, payload: FlashblocksPayloadV1) {
        let mut state = self.0.write().await;
        state.retain(|p| p.payload_id == payload.payload_id);
        state.push(payload);
    }

    /// Clears the current state of flashblocks.
    pub async fn clear(&self) {
        self.0.write().await.clear();
    }
}

#[derive(Debug)]
pub struct OpEngineApiExt<Provider, EngineT: EngineTypes, Pool, Validator, ChainSpec> {
    /// The inner [`OpEngineApi`] instance that this extension wraps.
    inner: OpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>,
    /// The current store of all pre confirmations ahead of the canonical chain.
    flashblocks_state: FlashblocksState,
}

impl<Provider, EngineT: EngineTypes, Pool, Validator, ChainSpec>
    OpEngineApiExt<Provider, EngineT, Pool, Validator, ChainSpec>
{
    /// Creates a new instance of [`OpEngineApiExt`], and spawns a task to handle incoming flashblocks.
    pub fn new(
        inner: OpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>,
        flashblocks_state: FlashblocksState,
        executor: impl TaskSpawner,
        stream: impl Stream<Item = FlashblocksPayloadV1> + Send + Unpin + 'static,
    ) -> Self {
        executor.spawn_critical(
            "subscription_handle",
            Self::spawn_subscription_handle(stream, flashblocks_state.clone()),
        );

        Self {
            inner,
            flashblocks_state,
        }
    }

    /// Returns a reference to the inner [`FlashblocksState`].
    pub fn flashblocks_state(&self) -> FlashblocksState {
        self.flashblocks_state.clone()
    }

    /// Spawns a task _solely_ responsible for appending new flashblocks to the state.
    /// Flushing happens when FCU's arrive with parent attributes matching the latest pre confirmed block hash.
    fn spawn_subscription_handle(
        mut stream: impl Stream<Item = FlashblocksPayloadV1> + Send + Unpin + 'static,
        flashblocks_state: FlashblocksState,
    ) -> Pin<Box<impl Future<Output = ()> + Send + 'static>> {
        Box::pin(async move {
            while let Some(payload) = stream.next().await {
                flashblocks_state.push(payload).await;
            }
        })
    }
}

#[async_trait]
impl<Provider, EngineT, Pool, Validator, ChainSpec> OpEngineApiServer<EngineT>
    for OpEngineApiExt<Provider, EngineT, Pool, Validator, ChainSpec>
where
    Provider: HeaderProvider + BlockReader + StateProviderFactory + 'static,
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Pool: TransactionPool + 'static,
    Validator: EngineValidator<EngineT>,
    ChainSpec: EthereumHardforks + Send + Sync + 'static,
{
    async fn new_payload_v2(&self, payload: ExecutionPayloadInputV2) -> RpcResult<PayloadStatus> {
        Ok(self.inner.new_payload_v2(payload).await?)
    }

    async fn new_payload_v3(
        &self,
        payload: ExecutionPayloadV3,
        versioned_hashes: Vec<B256>,
        parent_beacon_block_root: B256,
    ) -> RpcResult<PayloadStatus> {
        Ok(self
            .inner
            .new_payload_v3(payload, versioned_hashes, parent_beacon_block_root)
            .await?)
    }

    async fn new_payload_v4(
        &self,
        payload: OpExecutionPayloadV4,
        versioned_hashes: Vec<B256>,
        parent_beacon_block_root: B256,
        execution_requests: Requests,
    ) -> RpcResult<PayloadStatus> {
        Ok(self
            .inner
            .new_payload_v4(
                payload,
                versioned_hashes,
                parent_beacon_block_root,
                execution_requests,
            )
            .await?)
    }

    async fn fork_choice_updated_v1(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<EngineT::PayloadAttributes>,
    ) -> RpcResult<ForkchoiceUpdated> {
        let (res, _) = tokio::join!(
            self.inner
                .fork_choice_updated_v1(fork_choice_state, payload_attributes),
            self.handle_fork_choice_updated(fork_choice_state)
        );
        Ok(res?)
    }

    async fn fork_choice_updated_v2(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<EngineT::PayloadAttributes>,
    ) -> RpcResult<ForkchoiceUpdated> {
        let (res, _) = tokio::join!(
            self.inner
                .fork_choice_updated_v2(fork_choice_state, payload_attributes),
            self.handle_fork_choice_updated(fork_choice_state)
        );
        Ok(res?)
    }

    async fn fork_choice_updated_v3(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<EngineT::PayloadAttributes>,
    ) -> RpcResult<ForkchoiceUpdated> {
        let (res, _) = tokio::join!(
            self.inner
                .fork_choice_updated_v3(fork_choice_state, payload_attributes),
            self.handle_fork_choice_updated(fork_choice_state)
        );
        Ok(res?)
    }

    async fn get_payload_v2(
        &self,
        payload_id: PayloadId,
    ) -> RpcResult<EngineT::ExecutionPayloadEnvelopeV2> {
        Ok(self.inner.get_payload_v2(payload_id).await?)
    }

    async fn get_payload_v3(
        &self,
        payload_id: PayloadId,
    ) -> RpcResult<EngineT::ExecutionPayloadEnvelopeV3> {
        Ok(self.inner.get_payload_v3(payload_id).await?)
    }

    async fn get_payload_v4(
        &self,
        payload_id: PayloadId,
    ) -> RpcResult<EngineT::ExecutionPayloadEnvelopeV4> {
        Ok(self.inner.get_payload_v4(payload_id).await?)
    }

    async fn get_payload_bodies_by_hash_v1(
        &self,
        block_hashes: Vec<BlockHash>,
    ) -> RpcResult<ExecutionPayloadBodiesV1> {
        Ok(self
            .inner
            .get_payload_bodies_by_hash_v1(block_hashes)
            .await?)
    }

    async fn get_payload_bodies_by_range_v1(
        &self,
        start: U64,
        count: U64,
    ) -> RpcResult<ExecutionPayloadBodiesV1> {
        Ok(self
            .inner
            .get_payload_bodies_by_range_v1(start, count)
            .await?)
    }

    async fn signal_superchain_v1(&self, signal: SuperchainSignal) -> RpcResult<ProtocolVersion> {
        Ok(self.inner.signal_superchain_v1(signal).await?)
    }

    async fn get_client_version_v1(
        &self,
        client: ClientVersionV1,
    ) -> RpcResult<Vec<ClientVersionV1>> {
        Ok(self.inner.get_client_version_v1(client).await?)
    }

    async fn exchange_capabilities(&self, _capabilities: Vec<String>) -> RpcResult<Vec<String>> {
        Ok(self.inner.exchange_capabilities(_capabilities).await?)
    }
}

impl<Provider, EngineT, Pool, Validator, ChainSpec>
    OpEngineApiExt<Provider, EngineT, Pool, Validator, ChainSpec>
where
    Provider: HeaderProvider + BlockReader + StateProviderFactory + 'static,
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Pool: TransactionPool + 'static,
    Validator: EngineValidator<EngineT>,
    ChainSpec: EthereumHardforks + Send + Sync + 'static,
{
    /// Handles a [`ForkchoiceState`] update by checking if the latest flashblock matches the
    /// `head_block_hash` of the `ForkchoiceState`. If it does, it clears the flashblocks state.
    ///
    /// It is up to the consumer of [`FlashblocksState`] to ensure that the block number of the latest
    /// flashblock is 1 + latest block number in the canonical chain.
    pub async fn handle_fork_choice_updated(&self, fork_choice_state: ForkchoiceState) {
        let confirmed = self
            .flashblocks_state
            .last()
            .await
            .map(|p| p.diff.block_hash == fork_choice_state.head_block_hash);

        if confirmed.unwrap_or(false) {
            self.flashblocks_state.clear().await;
        }
    }
}

impl<Provider, EngineT, Pool, Validator, ChainSpec> IntoEngineApiRpcModule
    for OpEngineApiExt<Provider, EngineT, Pool, Validator, ChainSpec>
where
    EngineT: EngineTypes,
    Self: OpEngineApiServer<EngineT>,
{
    fn into_rpc_module(self) -> RpcModule<()> {
        self.into_rpc().remove_context()
    }
}
