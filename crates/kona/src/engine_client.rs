//! In-process Engine API client for Kona.
//!
//! This module provides [`InProcessEngineClient`], which implements Kona's
//! [`EngineClient`](kona_engine::EngineClient) trait by dispatching Engine API calls directly to
//! reth's engine handler — **no HTTP, no IPC, no serialization overhead**.
//!
//! # How it works
//!
//! Reth's engine is internally driven by a [`ConsensusEngineHandle`], which sends messages over a
//! tokio channel to the engine tree. We wrap this handle (along with reth's chain provider for
//! reads) and translate each Kona Engine API method into the corresponding reth call:
//!
//! - `new_payload_v*` → `ConsensusEngineHandle::new_payload`
//! - `fork_choice_updated_v*` → `ConsensusEngineHandle::fork_choice_updated`
//! - `get_payload_v*` → `PayloadStore::resolve`
//! - `get_l2_block` / `l2_block_by_label` → reth's served L2 RPC
//! - `get_payload_bodies_*` → reth's L2 chain provider (direct DB read)

use std::sync::Arc;

use alloy_eips::eip7685::Requests;
use alloy_eips::{BlockId, eip1898::BlockNumberOrTag};
use alloy_network::{Ethereum, Network};
use alloy_primitives::{Address, B256, BlockHash, StorageKey};
use alloy_provider::{EthGetBlock, Provider, RootProvider, RpcWithBlock};
use alloy_rpc_types_engine::{
    ClientCode, ClientVersionV1, ExecutionPayloadBodiesV1, ExecutionPayloadBodyV1,
    ExecutionPayloadEnvelopeV2, ExecutionPayloadInputV2, ExecutionPayloadV1, ExecutionPayloadV3,
    ForkchoiceState, ForkchoiceUpdated, PayloadId, PayloadStatus,
};
use alloy_rpc_types_eth::{Block, EIP1186AccountProofResponse};
use alloy_transport::{TransportError, TransportErrorKind, TransportResult};
use alloy_transport_http::Http;
use async_trait::async_trait;

use kona_engine::{EngineClient, EngineClientError, HyperAuthClient};
use kona_genesis::RollupConfig;
use kona_protocol::L2BlockInfo;

use op_alloy_network::Optimism;
use op_alloy_provider::ext::engine::OpEngineApi;
use op_alloy_rpc_types::Transaction;
use op_alloy_rpc_types_engine::{
    OpExecutionData, OpExecutionPayloadEnvelopeV3, OpExecutionPayloadEnvelopeV4,
    OpExecutionPayloadV4, OpPayloadAttributes, ProtocolVersion,
};

use reth_engine_primitives::ConsensusEngineHandle;
use reth_optimism_node::OpEngineTypes;
use reth_payload_builder::PayloadStore;
use reth_payload_primitives::EngineApiMessageVersion;
use reth_primitives_traits::{Block as _, BlockBody as _};
use reth_provider::{BlockReaderIdExt, HeaderProvider, StateProviderFactory};
use reth_storage_api::BlockReader;

fn engine_err(e: impl core::fmt::Display) -> TransportError {
    TransportError::from(TransportErrorKind::custom_str(&e.to_string()))
}

/// An in-process Engine API client that bridges Kona's consensus layer to reth's execution engine
/// **without any network transport** for the performance-critical Engine API methods.
///
/// The architecture splits operations into two paths:
///
/// - **In-process (fast path):** `new_payload`, `fork_choice_updated`, `get_payload`, and payload
///   body reads go directly through reth's internal handles and chain provider — no serialization,
///   no network hops.
///
/// - **RPC-backed (block reads):** `get_l2_block`, `l2_block_by_label`, `get_proof` delegate to
///   reth's own served L2 RPC endpoint. These methods return alloy RPC types that require complex
///   conversion from reth's internal types; using the already-served RPC avoids duplicating reth's
///   own RPC conversion machinery.
///
/// # Type Parameters
///
/// - `L2Provider` — reth's chain provider that implements block/state reads for payload bodies.
#[derive(Debug)]
pub struct InProcessEngineClient<L2Provider>
where
    L2Provider: Send + Sync + 'static,
{
    /// The OP Stack rollup configuration.
    cfg: Arc<RollupConfig>,

    /// Handle to reth's consensus engine tree — used for `new_payload` and `fork_choice_updated`.
    engine_handle: ConsensusEngineHandle<OpEngineTypes>,

    /// Reth's L2 chain provider for reading blocks directly from the database.
    /// Used for `get_payload_bodies_*` which doesn't need full RPC conversion.
    l2_provider: L2Provider,

    /// Reth's payload store for resolving built payloads via `get_payload_v*`.
    /// Wrapped in `Arc` since `PayloadStore` does not implement `Clone`.
    payload_store: Arc<PayloadStore<OpEngineTypes>>,

    /// External L1 RPC provider for `get_l1_block`.
    l1_provider: RootProvider,

    /// L2 RPC provider pointing at reth's own HTTP endpoint. Used for block reads and proofs
    /// that need full alloy RPC type conversion.
    l2_rpc: RootProvider<Optimism>,
}

impl<L2Provider> InProcessEngineClient<L2Provider>
where
    L2Provider: BlockReader
        + BlockReaderIdExt
        + HeaderProvider
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    pub fn new(
        cfg: Arc<RollupConfig>,
        engine_handle: ConsensusEngineHandle<OpEngineTypes>,
        l2_provider: L2Provider,
        payload_store: PayloadStore<OpEngineTypes>,
        l1_provider: RootProvider,
        l2_rpc: RootProvider<Optimism>,
    ) -> Self {
        Self {
            cfg,
            engine_handle,
            l2_provider,
            payload_store: Arc::new(payload_store),
            l1_provider,
            l2_rpc,
        }
    }

    /// Read a block from reth's database by block hash or number and return its body as an
    /// [`ExecutionPayloadBodyV1`].
    fn read_payload_body(
        &self,
        block_id: alloy_eips::BlockHashOrNumber,
    ) -> Result<Option<ExecutionPayloadBodyV1>, TransportError> {
        let block = self.l2_provider.block(block_id).map_err(engine_err)?;

        Ok(block.map(|b| {
            let transactions = b.body().encoded_2718_transactions();
            let withdrawals = b.body().withdrawals().cloned().map(|w| w.into_inner());
            ExecutionPayloadBodyV1 {
                transactions,
                withdrawals,
            }
        }))
    }
}

impl<L2Provider> Clone for InProcessEngineClient<L2Provider>
where
    L2Provider: Clone + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            cfg: self.cfg.clone(),
            engine_handle: self.engine_handle.clone(),
            l2_provider: self.l2_provider.clone(),
            payload_store: self.payload_store.clone(),
            l1_provider: self.l1_provider.clone(),
            l2_rpc: self.l2_rpc.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// EngineClient implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl<L2Provider> EngineClient for InProcessEngineClient<L2Provider>
where
    L2Provider: BlockReader
        + BlockReaderIdExt
        + HeaderProvider
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    fn cfg(&self) -> &RollupConfig {
        &self.cfg
    }

    fn get_l1_block(&self, block: BlockId) -> EthGetBlock<<Ethereum as Network>::BlockResponse> {
        self.l1_provider.get_block(block)
    }

    fn get_l2_block(&self, block: BlockId) -> EthGetBlock<<Optimism as Network>::BlockResponse> {
        self.l2_rpc.get_block(block)
    }

    fn get_proof(
        &self,
        address: Address,
        keys: Vec<StorageKey>,
    ) -> RpcWithBlock<(Address, Vec<StorageKey>), EIP1186AccountProofResponse> {
        self.l2_rpc.get_proof(address, keys)
    }

    async fn new_payload_v1(&self, _payload: ExecutionPayloadV1) -> TransportResult<PayloadStatus> {
        Err(engine_err(
            "new_payload_v1 is not supported for OP Stack post-Bedrock",
        ))
    }

    async fn l2_block_by_label(
        &self,
        numtag: BlockNumberOrTag,
    ) -> Result<Option<Block<Transaction>>, EngineClientError> {
        Ok(self.l2_rpc.get_block_by_number(numtag).full().await?)
    }

    async fn l2_block_info_by_label(
        &self,
        numtag: BlockNumberOrTag,
    ) -> Result<Option<L2BlockInfo>, EngineClientError> {
        let block = self.l2_rpc.get_block_by_number(numtag).full().await?;
        let Some(block) = block else {
            return Ok(None);
        };
        Ok(Some(L2BlockInfo::from_block_and_genesis(
            &block.into_consensus(),
            &self.cfg.genesis,
        )?))
    }
}

// ---------------------------------------------------------------------------
// OpEngineApi implementation — the core Engine API methods (in-process)
// ---------------------------------------------------------------------------

#[async_trait]
impl<L2Provider> OpEngineApi<Optimism, Http<HyperAuthClient>> for InProcessEngineClient<L2Provider>
where
    L2Provider: BlockReader
        + BlockReaderIdExt
        + HeaderProvider
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    async fn new_payload_v2(
        &self,
        _payload: ExecutionPayloadInputV2,
    ) -> TransportResult<PayloadStatus> {
        Err(engine_err(
            "new_payload_v2: not supported for OP Stack post-Ecotone",
        ))
    }

    async fn new_payload_v3(
        &self,
        payload: ExecutionPayloadV3,
        parent_beacon_block_root: B256,
    ) -> TransportResult<PayloadStatus> {
        let execution_data = OpExecutionData::v3(payload, vec![], parent_beacon_block_root);

        self.engine_handle
            .new_payload(execution_data)
            .await
            .map_err(engine_err)
    }

    async fn new_payload_v4(
        &self,
        payload: OpExecutionPayloadV4,
        parent_beacon_block_root: B256,
    ) -> TransportResult<PayloadStatus> {
        let execution_data = OpExecutionData::v4(
            payload,
            vec![],
            parent_beacon_block_root,
            Requests::default(),
        );

        self.engine_handle
            .new_payload(execution_data)
            .await
            .map_err(engine_err)
    }

    async fn fork_choice_updated_v2(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<OpPayloadAttributes>,
    ) -> TransportResult<ForkchoiceUpdated> {
        self.engine_handle
            .fork_choice_updated(
                fork_choice_state,
                payload_attributes,
                EngineApiMessageVersion::V2,
            )
            .await
            .map_err(engine_err)
    }

    async fn fork_choice_updated_v3(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<OpPayloadAttributes>,
    ) -> TransportResult<ForkchoiceUpdated> {
        self.engine_handle
            .fork_choice_updated(
                fork_choice_state,
                payload_attributes,
                EngineApiMessageVersion::V3,
            )
            .await
            .map_err(engine_err)
    }

    async fn get_payload_v2(
        &self,
        _payload_id: PayloadId,
    ) -> TransportResult<ExecutionPayloadEnvelopeV2> {
        Err(engine_err(
            "get_payload_v2: not supported for OP Stack post-Ecotone",
        ))
    }

    async fn get_payload_v3(
        &self,
        payload_id: PayloadId,
    ) -> TransportResult<OpExecutionPayloadEnvelopeV3> {
        let result = self
            .payload_store
            .best_payload(payload_id)
            .await
            .ok_or_else(|| engine_err("payload not found"))?
            .map_err(engine_err)?;

        Ok(result.into())
    }

    async fn get_payload_v4(
        &self,
        payload_id: PayloadId,
    ) -> TransportResult<OpExecutionPayloadEnvelopeV4> {
        let result = self
            .payload_store
            .best_payload(payload_id)
            .await
            .ok_or_else(|| engine_err("payload not found"))?
            .map_err(engine_err)?;

        Ok(result.into())
    }

    async fn get_payload_bodies_by_hash_v1(
        &self,
        block_hashes: Vec<BlockHash>,
    ) -> TransportResult<ExecutionPayloadBodiesV1> {
        block_hashes
            .into_iter()
            .map(|hash| self.read_payload_body(alloy_eips::BlockHashOrNumber::Hash(hash)))
            .collect()
    }

    async fn get_payload_bodies_by_range_v1(
        &self,
        start: u64,
        count: u64,
    ) -> TransportResult<ExecutionPayloadBodiesV1> {
        (start..start.saturating_add(count))
            .map(|num| self.read_payload_body(alloy_eips::BlockHashOrNumber::Number(num)))
            .collect()
    }

    async fn get_client_version_v1(
        &self,
        _client_version: ClientVersionV1,
    ) -> TransportResult<Vec<ClientVersionV1>> {
        Ok(vec![ClientVersionV1 {
            code: ClientCode::RH,
            name: "world-chain".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            commit: "unknown".to_string(),
        }])
    }

    async fn signal_superchain_v1(
        &self,
        recommended: ProtocolVersion,
        _required: ProtocolVersion,
    ) -> TransportResult<ProtocolVersion> {
        Ok(recommended)
    }

    async fn exchange_capabilities(
        &self,
        capabilities: Vec<String>,
    ) -> TransportResult<Vec<String>> {
        Ok(capabilities)
    }
}
