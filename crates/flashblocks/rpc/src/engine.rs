use alloy_eips::eip7685::Requests;
use alloy_primitives::{B256, BlockHash, U64};
use alloy_rpc_types_engine::{
    ClientVersionV1, ExecutionPayloadBodiesV1, ExecutionPayloadInputV2, ExecutionPayloadV3,
    ForkchoiceState, ForkchoiceUpdated, PayloadId, PayloadStatus,
};
use flashblocks_primitives::p2p::Authorization;
use jsonrpsee::{proc_macros::rpc, types::ErrorObject};
use jsonrpsee_core::{RpcResult, async_trait, server::RpcModule};
use op_alloy_rpc_types_engine::{
    OpExecutionData, OpExecutionPayloadV4, ProtocolVersion, SuperchainSignal,
};
use reth::{
    api::{EngineApiValidator, EngineTypes},
    rpc::api::IntoEngineApiRpcModule,
};
use reth_chainspec::EthereumHardforks;
use reth_optimism_rpc::{OpEngineApi, OpEngineApiServer};
use reth_provider::{BlockReader, HeaderProvider, StateProviderFactory};
use reth_transaction_pool::TransactionPool;
use tracing::trace;

#[derive(Debug, Clone)]
pub struct OpEngineApiExt<Provider, EngineT: EngineTypes, Pool, Validator, ChainSpec> {
    /// The inner [`OpEngineApi`] instance that this extension wraps.
    inner: OpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>,
    /// A watch channel notifier to the jobs generator.
    to_jobs_generator: tokio::sync::watch::Sender<Option<Authorization>>,
}

impl<Provider, EngineT: EngineTypes, Pool, Validator, ChainSpec>
    OpEngineApiExt<Provider, EngineT, Pool, Validator, ChainSpec>
{
    /// Creates a new instance of [`OpEngineApiExt`], and spawns a task to handle incoming flashblocks.
    pub fn new(
        inner: OpEngineApi<Provider, EngineT, Pool, Validator, ChainSpec>,
        to_jobs_generator: tokio::sync::watch::Sender<Option<Authorization>>,
    ) -> Self {
        Self {
            inner,
            to_jobs_generator,
        }
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
