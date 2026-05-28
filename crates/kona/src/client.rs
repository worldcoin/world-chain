//! In-process Engine API client for Kona.
//!
//! This module provides [`WorldChainKonaEngineClient`], which implements Kona's
//! [`EngineClient`](kona_engine::EngineClient) trait by dispatching the *consensus hot path* —
//! `engine_forkchoiceUpdated`, `engine_newPayload`, and `engine_getPayload` — directly to reth's
//! [`ConsensusEngineHandle`] and [`PayloadStore`] as in-process Rust calls. There is **no HTTP, no
//! JWT, and no JSON (de)serialization** on this path.
//!
//! # How it works
//!
//! Reth's engine is driven by a [`ConsensusEngineHandle`], which forwards messages over a tokio
//! channel to the engine tree — the exact same channel reth's authenticated Engine API RPC handler
//! uses. We wrap that handle and translate each Kona Engine API call into the corresponding reth
//! call:
//!
//! - `new_payload_v{2,3,4}` -> [`ConsensusEngineHandle::new_payload`]
//! - `fork_choice_updated_v{2,3}` -> [`ConsensusEngineHandle::fork_choice_updated`]
//! - `get_payload_v{2,3,4}` -> [`PayloadStore::resolve`]
//!
//! Infrequent reads that the engine actor performs during sync / forkchoice reconstruction
//! (`get_l2_block`, `l2_block_by_label`, `l2_block_info_by_label`, `get_proof`, `new_payload_v1`)
//! are delegated to alloy providers: an L2 [`RootProvider<Optimism>`] connected to reth's standard
//! (unauthenticated) IPC RPC endpoint, and an L1 [`RootProvider`] over HTTP (`--kona.l1-rpc-url`).
//! These are not on the consensus hot path, so the network/IPC transport is acceptable; keeping
//! them on alloy avoids reth<->alloy block type conversions while preserving correctness.

use std::sync::Arc;

use alloy_eips::{BlockId, eip1898::BlockNumberOrTag};
use alloy_network::{Ethereum, Network};
use alloy_primitives::{Address, B256, BlockHash, StorageKey};
use alloy_provider::{EthGetBlock, Provider, RootProvider, RpcWithBlock};
use alloy_rpc_types_engine::{
    ClientCode, ClientVersionV1, ExecutionPayloadBodiesV1, ExecutionPayloadEnvelopeV2,
    ExecutionPayloadInputV2, ExecutionPayloadV1, ExecutionPayloadV3, ForkchoiceState,
    ForkchoiceUpdated, PayloadId, PayloadStatus,
};
use alloy_rpc_types_eth::{Block, EIP1186AccountProofResponse};
use alloy_transport::{TransportErrorKind, TransportResult};
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
    OpExecutionPayloadV4, OpPayloadAttributes,
};

use reth_engine_primitives::ConsensusEngineHandle;
use reth_optimism_node::OpEngineTypes;
use reth_optimism_payload_builder::{OpPayloadAttrs, payload::OpExecData};
use reth_payload_builder::PayloadStore;

/// An in-process Engine API client that bridges Kona's consensus layer to reth's execution engine
/// for the consensus hot path, without any network transport.
///
/// The fork-choice / new-payload / get-payload methods are dispatched directly to reth's
/// [`ConsensusEngineHandle`] and [`PayloadStore`]. Infrequent read methods are delegated to alloy
/// providers — the L2 over reth's IPC RPC, the L1 over HTTP (see the module docs).
pub struct WorldChainKonaEngineClient {
    /// The OP Stack rollup configuration, shared between Kona and reth.
    cfg: Arc<RollupConfig>,
    /// Handle to reth's consensus engine tree. `new_payload` and `fork_choice_updated` calls are
    /// dispatched here over the same channel reth's authenticated Engine API uses internally.
    engine_handle: ConsensusEngineHandle<OpEngineTypes>,
    /// Reth's payload store, used to resolve built payloads for `get_payload_v*`.
    payload_store: PayloadStore<OpEngineTypes>,
    /// L2 EL provider over reth's standard IPC RPC, used for the infrequent read methods that the
    /// engine actor performs during sync and forkchoice reconstruction.
    l2_provider: RootProvider<Optimism>,
    /// L1 EL provider, used for `get_l1_block`. Reth only stores L2 data.
    l1_provider: RootProvider,
}

impl std::fmt::Debug for WorldChainKonaEngineClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldChainKonaEngineClient")
            .field("l2_chain_id", &self.cfg.l2_chain_id)
            .finish_non_exhaustive()
    }
}

impl WorldChainKonaEngineClient {
    /// Creates a new in-process engine client.
    ///
    /// # Arguments
    ///
    /// * `cfg` — The OP Stack rollup configuration, shared between Kona and reth.
    /// * `engine_handle` — A handle to reth's consensus engine tree, obtained from the node's
    ///   [`AddOnsContext`](reth_node_api::AddOnsContext) after launch.
    /// * `payload_store` — Reth's store of in-progress and completed payloads.
    /// * `l2_provider` — An alloy provider connected to reth's standard L2 IPC RPC, used for reads.
    /// * `l1_provider` — An alloy provider for the L1 chain (deposits, finalization).
    pub const fn new(
        cfg: Arc<RollupConfig>,
        engine_handle: ConsensusEngineHandle<OpEngineTypes>,
        payload_store: PayloadStore<OpEngineTypes>,
        l2_provider: RootProvider<Optimism>,
        l1_provider: RootProvider,
    ) -> Self {
        Self {
            cfg,
            engine_handle,
            payload_store,
            l2_provider,
            l1_provider,
        }
    }

    /// Dispatches a [`OpExecutionData`] payload to reth's engine and maps the result into a
    /// [`TransportResult`], as the Kona trait surface expects.
    async fn dispatch_new_payload(&self, data: OpExecutionData) -> TransportResult<PayloadStatus> {
        self.engine_handle
            .new_payload(OpExecData(data))
            .await
            .map_err(|e| TransportErrorKind::custom_str(&e.to_string()))
    }

    /// Dispatches a forkchoice update to reth's engine and maps the result into a
    /// [`TransportResult`].
    async fn dispatch_fcu(
        &self,
        state: ForkchoiceState,
        attrs: Option<OpPayloadAttributes>,
    ) -> TransportResult<ForkchoiceUpdated> {
        self.engine_handle
            .fork_choice_updated(state, attrs.map(OpPayloadAttrs))
            .await
            .map_err(|e| TransportErrorKind::custom_str(&e.to_string()))
    }

    /// Resolves a built payload from reth's [`PayloadStore`] by id, mapping the absence of a job
    /// or a build error into a [`TransportError`].
    async fn resolve_payload(
        &self,
        payload_id: PayloadId,
    ) -> TransportResult<reth_optimism_payload_builder::OpBuiltPayload> {
        match self.payload_store.resolve(payload_id).await {
            Some(Ok(payload)) => Ok(payload),
            Some(Err(e)) => Err(TransportErrorKind::custom_str(&e.to_string())),
            None => Err(TransportErrorKind::custom_str(
                "payload job not found in reth payload store",
            )),
        }
    }
}

#[async_trait]
impl EngineClient for WorldChainKonaEngineClient {
    fn cfg(&self) -> &RollupConfig {
        &self.cfg
    }

    fn get_l1_block(&self, block: BlockId) -> EthGetBlock<<Ethereum as Network>::BlockResponse> {
        // L1 blocks must be fetched from the external L1 RPC — reth only has L2 data.
        self.l1_provider.get_block(block)
    }

    fn get_l2_block(&self, block: BlockId) -> EthGetBlock<<Optimism as Network>::BlockResponse> {
        self.l2_provider.get_block(block)
    }

    fn get_proof(
        &self,
        address: Address,
        keys: Vec<StorageKey>,
    ) -> RpcWithBlock<(Address, Vec<StorageKey>), EIP1186AccountProofResponse> {
        self.l2_provider.get_proof(address, keys)
    }

    async fn new_payload_v1(&self, payload: ExecutionPayloadV1) -> TransportResult<PayloadStatus> {
        // V1 (pre-Shanghai) is unused in OP Stack post-Bedrock, but the engine task queue still
        // calls it via the version-dispatched insert task. Dispatch it in-process for completeness.
        self.dispatch_new_payload(OpExecutionData::v2(ExecutionPayloadInputV2 {
            execution_payload: payload,
            withdrawals: None,
        }))
        .await
    }

    async fn l2_block_by_label(
        &self,
        numtag: BlockNumberOrTag,
    ) -> Result<Option<Block<Transaction>>, EngineClientError> {
        Ok(self.l2_provider.get_block_by_number(numtag).full().await?)
    }

    async fn l2_block_info_by_label(
        &self,
        numtag: BlockNumberOrTag,
    ) -> Result<Option<L2BlockInfo>, EngineClientError> {
        let Some(block) = self.l2_provider.get_block_by_number(numtag).full().await? else {
            return Ok(None);
        };
        Ok(Some(L2BlockInfo::from_block_and_genesis(
            &block.into_consensus(),
            &self.cfg.genesis,
        )?))
    }
}

// ---------------------------------------------------------------------------
// OpEngineApi — the consensus hot path (dispatched in-process)
// ---------------------------------------------------------------------------

#[async_trait]
impl OpEngineApi<Optimism, Http<HyperAuthClient>> for WorldChainKonaEngineClient {
    async fn new_payload_v2(
        &self,
        payload: ExecutionPayloadInputV2,
    ) -> TransportResult<PayloadStatus> {
        self.dispatch_new_payload(OpExecutionData::v2(payload))
            .await
    }

    async fn new_payload_v3(
        &self,
        payload: ExecutionPayloadV3,
        parent_beacon_block_root: B256,
    ) -> TransportResult<PayloadStatus> {
        // OP `newPayloadV3` carries no versioned hashes (they must be empty).
        self.dispatch_new_payload(OpExecutionData::v3(
            payload,
            Vec::new(),
            parent_beacon_block_root,
        ))
        .await
    }

    async fn new_payload_v4(
        &self,
        payload: OpExecutionPayloadV4,
        parent_beacon_block_root: B256,
    ) -> TransportResult<PayloadStatus> {
        // Isthmus variant. OP carries no versioned hashes and no execution requests on L2.
        self.dispatch_new_payload(OpExecutionData::v4(
            payload,
            Vec::new(),
            parent_beacon_block_root,
            Default::default(),
        ))
        .await
    }

    async fn fork_choice_updated_v2(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<OpPayloadAttributes>,
    ) -> TransportResult<ForkchoiceUpdated> {
        self.dispatch_fcu(fork_choice_state, payload_attributes)
            .await
    }

    async fn fork_choice_updated_v3(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<OpPayloadAttributes>,
    ) -> TransportResult<ForkchoiceUpdated> {
        self.dispatch_fcu(fork_choice_state, payload_attributes)
            .await
    }

    async fn get_payload_v2(
        &self,
        payload_id: PayloadId,
    ) -> TransportResult<ExecutionPayloadEnvelopeV2> {
        Ok(self.resolve_payload(payload_id).await?.into())
    }

    async fn get_payload_v3(
        &self,
        payload_id: PayloadId,
    ) -> TransportResult<OpExecutionPayloadEnvelopeV3> {
        Ok(self.resolve_payload(payload_id).await?.into())
    }

    async fn get_payload_v4(
        &self,
        payload_id: PayloadId,
    ) -> TransportResult<OpExecutionPayloadEnvelopeV4> {
        Ok(self.resolve_payload(payload_id).await?.into())
    }

    async fn get_payload_bodies_by_hash_v1(
        &self,
        block_hashes: Vec<BlockHash>,
    ) -> TransportResult<ExecutionPayloadBodiesV1> {
        // Not on the consensus hot path; delegate to reth's standard RPC.
        OpEngineApi::<Optimism, Http<HyperAuthClient>>::get_payload_bodies_by_hash_v1(
            &self.l2_provider,
            block_hashes,
        )
        .await
    }

    async fn get_payload_bodies_by_range_v1(
        &self,
        start: u64,
        count: u64,
    ) -> TransportResult<ExecutionPayloadBodiesV1> {
        OpEngineApi::<Optimism, Http<HyperAuthClient>>::get_payload_bodies_by_range_v1(
            &self.l2_provider,
            start,
            count,
        )
        .await
    }

    async fn get_client_version_v1(
        &self,
        _client_version: ClientVersionV1,
    ) -> TransportResult<Vec<ClientVersionV1>> {
        Ok(vec![ClientVersionV1 {
            // No World Chain client code exists in the enum; reth is the closest match.
            code: ClientCode::RH,
            name: "world-chain".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            commit: "unknown".to_string(),
        }])
    }

    async fn exchange_capabilities(
        &self,
        capabilities: Vec<String>,
    ) -> TransportResult<Vec<String>> {
        // In-process, we support everything the engine supports; echo the peer's capabilities.
        Ok(capabilities)
    }
}
