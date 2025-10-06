use alloy_consensus::BlockHeader;
use alloy_eips::eip7685::Requests;
use alloy_primitives::{BlockHash, B256, U64};
use alloy_rpc_types_engine::{
    ClientVersionV1, ExecutionPayloadBodiesV1, ExecutionPayloadInputV2, ExecutionPayloadV3,
    ForkchoiceState, ForkchoiceUpdated, PayloadId, PayloadStatus, PayloadStatusEnum,
};
use flashblocks_primitives::p2p::Authorization;
use jsonrpsee::{proc_macros::rpc, types::ErrorObject};
use jsonrpsee_core::{async_trait, server::RpcModule, RpcResult};
use op_alloy_rpc_types_engine::{
    OpExecutionData, OpExecutionPayloadV4, ProtocolVersion, SuperchainSignal,
};
use reth::{
    api::{EngineApiValidator, EngineTypes},
    rpc::api::IntoEngineApiRpcModule,
};
use reth_chain_state::ExecutedBlockWithTrieUpdates;
use reth_chainspec::EthereumHardforks;
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::{OpEngineApi, OpEngineApiServer};
use reth_provider::{BlockReader, HeaderProvider, StateProviderFactory};
use reth_transaction_pool::TransactionPool;
use tracing::{debug, trace};

#[derive(Debug, Clone)]
pub struct OpEngineApiExt<Provider, EngineT: EngineTypes, Pool, Validator, ChainSpec> {
    /// The inner [`OpEngineApi`] instance that this extension wraps.
    inner: OpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>,
    /// A watch channel notifier to the jobs generator.
    to_jobs_generator: tokio::sync::watch::Sender<Option<Authorization>>,
    /// Watch channel receiver for pending flashblock.
    pending_block_rx:
        tokio::sync::watch::Receiver<Option<ExecutedBlockWithTrieUpdates<OpPrimitives>>>,
}

impl<Provider, EngineT: EngineTypes, Pool, Validator, ChainSpec>
    OpEngineApiExt<Provider, EngineT, Pool, Validator, ChainSpec>
{
    /// Creates a new instance of [`OpEngineApiExt`], and spawns a task to handle incoming flashblocks.
    pub fn new(
        inner: OpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>,
        to_jobs_generator: tokio::sync::watch::Sender<Option<Authorization>>,
        pending_block_rx: tokio::sync::watch::Receiver<
            Option<ExecutedBlockWithTrieUpdates<OpPrimitives>>,
        >,
    ) -> Self {
        Self {
            inner,
            to_jobs_generator,
            pending_block_rx,
        }
    }

    /// Checks if the given payload matches the cached pending block.
    /// Returns a valid PayloadStatus if there's a match, None otherwise.
    ///
    /// Compares:
    /// - Block hash
    /// - Parent hash
    /// - Timestamp
    /// - Transaction list (order and content)
    fn check_cached_payload(
        &self,
        block_hash: B256,
        parent_hash: B256,
        timestamp: u64,
        transactions: &[alloy_primitives::Bytes],
    ) -> Option<PayloadStatus> {
        let pending_block = self.pending_block_rx.borrow();

        if let Some(ref executed_block) = *pending_block {
            let cached_block = &executed_block.block.recovered_block;

            // Compare basic block attributes
            if cached_block.hash() != block_hash
                || cached_block.parent_hash != parent_hash
                || cached_block.timestamp != timestamp
            {
                return None;
            }

            // Compare transaction count first for quick rejection
            if cached_block.body().transactions().count() != transactions.len() {
                return None;
            }

            // Compare each transaction
            let cached_txs: Vec<_> = cached_block
                .body()
                .transactions()
                .map(|tx| alloy_eips::eip2718::Encodable2718::encoded_2718(tx))
                .collect();

            for (cached_tx, input_tx) in cached_txs.iter().zip(transactions.iter()) {
                if &cached_tx[..] != &input_tx[..] {
                    return None;
                }
            }

            debug!(
                target: "flashblocks::rpc::engine",
                %block_hash,
                "Returning cached payload from flashblocks state executor"
            );

            return Some(PayloadStatus::new(
                PayloadStatusEnum::Valid,
                Some(cached_block.state_root()),
            ));
        }

        None
    }
}

#[async_trait]
impl<Provider, EngineT, Pool, Validator, ChainSpec> OpEngineApiServer<EngineT>
    for OpEngineApiExt<Provider, EngineT, Pool, Validator, ChainSpec>
where
    Provider: HeaderProvider + BlockReader + StateProviderFactory + 'static,
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Pool: TransactionPool + 'static,
    Validator: EngineApiValidator<EngineT>,
    ChainSpec: EthereumHardforks + Send + Sync + 'static,
{
    async fn new_payload_v2(&self, payload: ExecutionPayloadInputV2) -> RpcResult<PayloadStatus> {
        // Check if we have this payload cached
        if let Some(cached_status) = self.check_cached_payload(
            payload.execution_payload.block_hash,
            payload.execution_payload.parent_hash,
            payload.execution_payload.timestamp,
            &payload.execution_payload.transactions,
        ) {
            return Ok(cached_status);
        }

        Ok(self.inner.new_payload_v2(payload).await?)
    }

    async fn new_payload_v3(
        &self,
        payload: ExecutionPayloadV3,
        versioned_hashes: Vec<B256>,
        parent_beacon_block_root: B256,
    ) -> RpcResult<PayloadStatus> {
        // Check if we have this payload cached
        if let Some(cached_status) = self.check_cached_payload(
            payload.payload_inner.payload_inner.block_hash,
            payload.payload_inner.payload_inner.parent_hash,
            payload.payload_inner.payload_inner.timestamp,
            &payload.payload_inner.payload_inner.transactions,
        ) {
            return Ok(cached_status);
        }

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
        // Check if we have this payload cached
        if let Some(cached_status) = self.check_cached_payload(
            payload.payload_inner.payload_inner.payload_inner.block_hash,
            payload
                .payload_inner
                .payload_inner
                .payload_inner
                .parent_hash,
            payload.payload_inner.payload_inner.payload_inner.timestamp,
            &payload
                .payload_inner
                .payload_inner
                .payload_inner
                .transactions,
        ) {
            return Ok(cached_status);
        }

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
        self.inner
            .fork_choice_updated_v1(fork_choice_state, payload_attributes)
            .await
    }

    async fn fork_choice_updated_v2(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<EngineT::PayloadAttributes>,
    ) -> RpcResult<ForkchoiceUpdated> {
        self.inner
            .fork_choice_updated_v2(fork_choice_state, payload_attributes)
            .await
    }

    async fn fork_choice_updated_v3(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<EngineT::PayloadAttributes>,
    ) -> RpcResult<ForkchoiceUpdated> {
        self.inner
            .fork_choice_updated_v3(fork_choice_state, payload_attributes)
            .await
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

impl<Provider, EngineT, Pool, Validator, ChainSpec> IntoEngineApiRpcModule
    for OpEngineApiExt<Provider, EngineT, Pool, Validator, ChainSpec>
where
    EngineT: EngineTypes,
    Self: OpEngineApiServer<EngineT> + FlashblocksEngineApiExtServer<EngineT> + Clone,
{
    fn into_rpc_module(self) -> RpcModule<()> {
        let mut module = RpcModule::new(());
        module
            .merge(OpEngineApiServer::into_rpc(self.clone()))
            .unwrap();

        module
            .merge(FlashblocksEngineApiExtServer::into_rpc(self))
            .unwrap();

        module.remove_context()
    }
}

#[rpc(server, client, namespace = "flashblocks", client_bounds(Engine::PayloadAttributes: jsonrpsee::core::Serialize + Clone), server_bounds(Engine::PayloadAttributes: jsonrpsee::core::DeserializeOwned))]
pub trait FlashblocksEngineApiExt<Engine: EngineTypes> {
    #[method(name = "forkchoiceUpdatedV3")]
    async fn flashblocks_fork_choice_updated_v3(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<Engine::PayloadAttributes>,
        authorization: Option<Authorization>,
    ) -> RpcResult<ForkchoiceUpdated>;
}

#[async_trait]
impl<Provider, EngineT, Pool, Validator, ChainSpec> FlashblocksEngineApiExtServer<EngineT>
    for OpEngineApiExt<Provider, EngineT, Pool, Validator, ChainSpec>
where
    Provider: HeaderProvider + BlockReader + StateProviderFactory + 'static,
    EngineT: EngineTypes<ExecutionData = OpExecutionData>,
    Pool: TransactionPool + 'static,
    Validator: EngineApiValidator<EngineT>,
    ChainSpec: EthereumHardforks + Send + Sync + 'static,
{
    async fn flashblocks_fork_choice_updated_v3(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<EngineT::PayloadAttributes>,
        authorization: Option<Authorization>,
    ) -> RpcResult<ForkchoiceUpdated> {
        trace!(
            target: "flashblocks::rpc::engine",
            ?fork_choice_state,
            "Received flashblocks_fork_choice_updated_v3"
        );

        if payload_attributes.is_some() && authorization.is_none()
            || authorization.is_some() && payload_attributes.is_none()
        {
            return Err(ErrorObject::owned(
                -32000,
                "Both payload attributes and authorization must be provided together",
                None::<()>,
            ));
        }

        if let Some(a) = authorization {
            self.to_jobs_generator.send_modify(|b| *b = Some(a))
        }

        self.fork_choice_updated_v3(fork_choice_state, payload_attributes)
            .await
    }
}
