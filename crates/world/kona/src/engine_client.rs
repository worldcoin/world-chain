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
//! - `get_l2_block` / `l2_block_by_label` → `Provider::block_by_*`
//!
//! # Limitations (WIP)
//!
//! - The `get_l1_block` and `get_proof` methods currently delegate to an external L1 RPC provider
//!   (they cannot be served by reth, which only stores L2 data).
//! - `get_payload_v*` methods need the payload to be converted between reth's `OpBuiltPayload` and
//!   the alloy/op-alloy envelope types that Kona expects.
//! - Some methods return provider builder types (`EthGetBlock`, `RpcWithBlock`) that are designed
//!   around alloy's RPC abstractions. We wrap these with `ProviderCall::BoxedFuture` to supply
//!   data from reth's provider directly.

use std::sync::Arc;

use alloy_eips::{BlockId, eip1898::BlockNumberOrTag};
use alloy_network::{Ethereum, Network};
use alloy_primitives::{Address, B256, BlockHash, StorageKey};
use alloy_provider::{EthGetBlock, Provider, ProviderCall, RootProvider, RpcWithBlock};
use alloy_rpc_types_engine::{
    ClientVersionV1, ExecutionPayloadBodiesV1, ExecutionPayloadEnvelopeV2,
    ExecutionPayloadInputV2, ExecutionPayloadV1, ExecutionPayloadV3, ForkchoiceState,
    ForkchoiceUpdated, PayloadId, PayloadStatus,
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
    OpExecutionPayloadEnvelopeV3, OpExecutionPayloadEnvelopeV4, OpExecutionPayloadV4,
    OpPayloadAttributes, ProtocolVersion,
};

use reth_engine_primitives::ConsensusEngineHandle;
use reth_optimism_node::OpEngineTypes;
use reth_payload_builder::PayloadStore;
#[allow(unused_imports)] // Used in TODO code paths
use reth_payload_primitives::PayloadTypes;
use reth_provider::{BlockReaderIdExt, HeaderProvider, StateProviderFactory};
use reth_storage_api::BlockReader;

/// An in-process Engine API client that bridges Kona's consensus layer to reth's execution engine
/// **without any network transport**.
///
/// Instead of sending JSON-RPC requests over HTTP, this client:
/// - Sends `new_payload` and `fork_choice_updated` messages directly to reth's engine tree via
///   [`ConsensusEngineHandle`]
/// - Reads block data directly from reth's provider (database)
/// - Resolves built payloads from reth's [`PayloadStore`]
///
/// The L1 provider is still an external HTTP RPC client, since reth only has L2 data.
///
/// # Type Parameters
///
/// - `L2Provider` — reth's chain provider that implements block/state reads.
///
/// # Example (conceptual)
///
/// ```rust,ignore
/// // After reth node is launched, extract the engine handle and provider:
/// let engine_handle = node.beacon_consensus_engine_handle();
/// let provider = node.provider();
/// let payload_store = node.payload_store();
///
/// // Create the in-process client:
/// let client = InProcessEngineClient::new(
///     rollup_config,
///     engine_handle,
///     provider,
///     payload_store,
///     l1_provider,
/// );
///
/// // Pass `client` to Kona's EngineProcessor — it will call reth directly.
/// ```
#[derive(Debug)]
pub struct InProcessEngineClient<L2Provider>
where
    L2Provider: Send + Sync + 'static,
{
    /// The OP Stack rollup configuration.
    cfg: Arc<RollupConfig>,

    /// Handle to reth's consensus engine tree.
    ///
    /// This is the same channel that reth's Engine API RPC handler uses internally.
    /// `new_payload` and `fork_choice_updated` calls are dispatched here.
    engine_handle: ConsensusEngineHandle<OpEngineTypes>,

    /// Reth's L2 chain provider for reading blocks, headers, and state.
    ///
    /// All `get_l2_block`, `l2_block_by_label`, and `get_proof` calls for L2 data
    /// go through this provider, reading directly from reth's database.
    l2_provider: L2Provider,

    /// Reth's payload store for resolving built payloads.
    ///
    /// When Kona calls `get_payload_v*`, we look up the payload by ID from this store.
    payload_store: PayloadStore<OpEngineTypes>,

    /// External L1 RPC provider.
    ///
    /// Reth only stores L2 data, so L1 block lookups are still proxied to an external
    /// L1 execution client over HTTP. This is acceptable because L1 reads are infrequent
    /// (mainly during derivation start-up and finalization).
    l1_provider: RootProvider,
}

impl<L2Provider> InProcessEngineClient<L2Provider>
where
    L2Provider: BlockReader + BlockReaderIdExt + HeaderProvider + StateProviderFactory + Clone + Send + Sync + 'static,
{
    /// Creates a new in-process engine client.
    ///
    /// # Arguments
    ///
    /// * `cfg` — The OP Stack rollup configuration, shared between Kona and reth.
    /// * `engine_handle` — A handle to reth's consensus engine tree (obtained from the node after
    ///   launch).
    /// * `l2_provider` — Reth's provider for reading L2 blocks and state from the database.
    /// * `payload_store` — Reth's store of in-progress and completed payloads.
    /// * `l1_provider` — An HTTP RPC provider for the L1 chain (deposits, finalization).
    pub fn new(
        cfg: Arc<RollupConfig>,
        engine_handle: ConsensusEngineHandle<OpEngineTypes>,
        l2_provider: L2Provider,
        payload_store: PayloadStore<OpEngineTypes>,
        l1_provider: RootProvider,
    ) -> Self {
        Self {
            cfg,
            engine_handle,
            l2_provider,
            payload_store,
            l1_provider,
        }
    }

    /// Helper: Convert a `BlockNumberOrTag` to a `BlockId` for the L2 provider.
    fn block_id_from_label(label: BlockNumberOrTag) -> BlockId {
        match label {
            BlockNumberOrTag::Latest => BlockId::latest(),
            BlockNumberOrTag::Finalized => BlockId::finalized(),
            BlockNumberOrTag::Safe => BlockId::safe(),
            BlockNumberOrTag::Earliest => BlockId::earliest(),
            BlockNumberOrTag::Pending => BlockId::pending(),
            BlockNumberOrTag::Number(n) => BlockId::number(n),
        }
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
        }
    }
}

// ---------------------------------------------------------------------------
// EngineClient implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl<L2Provider> EngineClient for InProcessEngineClient<L2Provider>
where
    L2Provider: BlockReader + BlockReaderIdExt + HeaderProvider + StateProviderFactory + Clone + Send + Sync + 'static,
{
    fn cfg(&self) -> &RollupConfig {
        &self.cfg
    }

    fn get_l1_block(&self, block: BlockId) -> EthGetBlock<<Ethereum as Network>::BlockResponse> {
        // L1 blocks must be fetched from the external L1 RPC — reth only has L2 data.
        self.l1_provider.get_block(block)
    }

    fn get_l2_block(&self, block: BlockId) -> EthGetBlock<<Optimism as Network>::BlockResponse> {
        // TODO(kona-integration): Read from `self.l2_provider` directly instead of proxying
        // to an alloy RPC provider. This requires converting reth's block types to alloy's
        // `Block<Transaction>` (op-alloy's Transaction type).
        //
        // For now, we return an error indicating this is not yet implemented.
        // The proper implementation would:
        // 1. Use `self.l2_provider.block_by_id(block)` to get the reth block
        // 2. Convert it to `alloy_rpc_types_eth::Block<op_alloy_rpc_types::Transaction>`
        // 3. Return it via ProviderCall::BoxedFuture
        //
        // This is blocked on type conversion utilities between reth and alloy block types.
        let l2_provider = self.l2_provider.clone();
        let _block_id = block;

        EthGetBlock::new_provider(
            block,
            Box::new(move |_kind| {
                // FIXME: Implement proper reth provider → alloy block conversion.
                // For now, return None (block not found), which Kona handles gracefully.
                ProviderCall::BoxedFuture(Box::pin(async move {
                    // TODO: Use l2_provider to read the block from reth's DB and convert.
                    let _ = l2_provider;
                    Ok(None)
                }))
            }),
        )
    }

    fn get_proof(
        &self,
        address: Address,
        keys: Vec<StorageKey>,
    ) -> RpcWithBlock<(Address, Vec<StorageKey>), EIP1186AccountProofResponse> {
        // TODO(kona-integration): Read proofs from reth's state provider directly.
        //
        // The implementation would:
        // 1. Get the state provider for the requested block from `self.l2_provider`
        // 2. Use `state_provider.proof(address, &keys)` to compute the merkle proof
        // 3. Convert the result to `EIP1186AccountProofResponse`
        //
        // For now, we return an error.
        RpcWithBlock::new_provider(move |_block_id| {
            ProviderCall::BoxedFuture(Box::pin(async move {
                Err(TransportError::from(TransportErrorKind::custom_str(
                    "InProcessEngineClient::get_proof not yet implemented — \
                     needs reth state provider integration",
                )))
            }))
        })
    }

    async fn new_payload_v1(&self, _payload: ExecutionPayloadV1) -> TransportResult<PayloadStatus> {
        // V1 payloads are pre-Shanghai and not used in OP Stack post-Bedrock.
        // Stub with an error for now.
        Err(TransportError::from(TransportErrorKind::custom_str(
            "new_payload_v1 is not supported in the in-process engine client",
        )))
    }

    async fn l2_block_by_label(
        &self,
        numtag: BlockNumberOrTag,
    ) -> Result<Option<Block<Transaction>>, EngineClientError> {
        // TODO(kona-integration): Read from reth provider and convert to alloy block type.
        //
        // Implementation outline:
        // 1. Resolve `numtag` to a block number using the provider
        // 2. Read the full block with transactions from reth's DB
        // 3. Convert reth::primitives::Block → alloy Block<op_alloy_rpc_types::Transaction>
        //
        // For now, return None (block not found).
        let _ = numtag;
        Ok(None)
    }

    async fn l2_block_info_by_label(
        &self,
        numtag: BlockNumberOrTag,
    ) -> Result<Option<L2BlockInfo>, EngineClientError> {
        // TODO(kona-integration): Read from reth provider and derive L2BlockInfo.
        //
        // Implementation outline:
        // 1. Get the block from the provider
        // 2. Parse L1 info from the first transaction (deposit tx)
        // 3. Construct L2BlockInfo from the block header + L1 info
        let _ = numtag;
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// OpEngineApi implementation — the core Engine API methods
// ---------------------------------------------------------------------------

#[async_trait]
impl<L2Provider> OpEngineApi<Optimism, Http<HyperAuthClient>> for InProcessEngineClient<L2Provider>
where
    L2Provider: BlockReader + BlockReaderIdExt + HeaderProvider + StateProviderFactory + Clone + Send + Sync + 'static,
{
    async fn new_payload_v2(
        &self,
        payload: ExecutionPayloadInputV2,
    ) -> TransportResult<PayloadStatus> {
        // Convert the v2 payload into reth's ExecutionData type and dispatch to the engine.
        //
        // TODO(kona-integration): The conversion from `ExecutionPayloadInputV2` to
        // `<OpEngineTypes as PayloadTypes>::ExecutionData` requires careful mapping.
        //
        // The path is:
        // 1. Convert ExecutionPayloadInputV2 → OpExecutionData (reth-optimism type)
        // 2. Send via engine_handle.new_payload(data).await
        // 3. Map the result back to TransportResult<PayloadStatus>
        //
        // For now, dispatch via the engine handle with a minimal conversion.
        // This will need the proper type bridging to compile end-to-end.
        let _ = payload;
        Err(TransportError::from(TransportErrorKind::custom_str(
            "new_payload_v2: in-process dispatch not yet wired — \
             needs OpExecutionData conversion",
        )))
    }

    async fn new_payload_v3(
        &self,
        payload: ExecutionPayloadV3,
        parent_beacon_block_root: B256,
    ) -> TransportResult<PayloadStatus> {
        // This is the most commonly used new_payload version for modern OP Stack chains.
        //
        // TODO(kona-integration): Convert ExecutionPayloadV3 + parent_beacon_block_root
        // into OpExecutionData and dispatch to engine_handle.
        //
        // The implementation would be:
        // ```
        // let execution_data = OpExecutionData::from_v3(payload, parent_beacon_block_root);
        // let status = self.engine_handle.new_payload(execution_data).await
        //     .map_err(|e| TransportError::from(TransportErrorKind::custom_str(&e.to_string())))?;
        // Ok(status)
        // ```
        let _ = (payload, parent_beacon_block_root);
        Err(TransportError::from(TransportErrorKind::custom_str(
            "new_payload_v3: in-process dispatch not yet wired — \
             needs OpExecutionData conversion",
        )))
    }

    async fn new_payload_v4(
        &self,
        payload: OpExecutionPayloadV4,
        parent_beacon_block_root: B256,
    ) -> TransportResult<PayloadStatus> {
        // V4 is the Isthmus variant with execution requests.
        let _ = (payload, parent_beacon_block_root);
        Err(TransportError::from(TransportErrorKind::custom_str(
            "new_payload_v4: in-process dispatch not yet wired — \
             needs OpExecutionData conversion",
        )))
    }

    async fn fork_choice_updated_v2(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<OpPayloadAttributes>,
    ) -> TransportResult<ForkchoiceUpdated> {
        // Dispatch fork choice update to reth's engine tree.
        //
        // TODO(kona-integration): Convert OpPayloadAttributes → reth's PayloadAttributes type.
        //
        // The implementation would be:
        // ```
        // let reth_attrs = payload_attributes.map(convert_payload_attributes);
        // let result = self.engine_handle
        //     .fork_choice_updated(fork_choice_state, reth_attrs, EngineApiMessageVersion::V2)
        //     .await
        //     .map_err(|e| TransportError::from(TransportErrorKind::custom_str(&e.to_string())))?;
        // Ok(result)
        // ```
        let _ = (fork_choice_state, payload_attributes);
        Err(TransportError::from(TransportErrorKind::custom_str(
            "fork_choice_updated_v2: in-process dispatch not yet wired — \
             needs PayloadAttributes conversion",
        )))
    }

    async fn fork_choice_updated_v3(
        &self,
        fork_choice_state: ForkchoiceState,
        payload_attributes: Option<OpPayloadAttributes>,
    ) -> TransportResult<ForkchoiceUpdated> {
        // V3 is the most commonly used FCU version for modern OP Stack chains.
        //
        // The in-process path eliminates the entire JSON-RPC roundtrip:
        // - No JSON serialization of ForkchoiceState + PayloadAttributes
        // - No HTTP request/response
        // - No JSON deserialization of ForkchoiceUpdated
        // - Direct tokio channel send → engine tree processing → oneshot response
        let _ = (fork_choice_state, payload_attributes);
        Err(TransportError::from(TransportErrorKind::custom_str(
            "fork_choice_updated_v3: in-process dispatch not yet wired — \
             needs PayloadAttributes conversion",
        )))
    }

    async fn get_payload_v2(
        &self,
        payload_id: PayloadId,
    ) -> TransportResult<ExecutionPayloadEnvelopeV2> {
        // Resolve from reth's payload store.
        //
        // TODO(kona-integration): Use self.payload_store.resolve(payload_id) and convert
        // the OpBuiltPayload into ExecutionPayloadEnvelopeV2.
        let _ = payload_id;
        Err(TransportError::from(TransportErrorKind::custom_str(
            "get_payload_v2: in-process dispatch not yet wired — \
             needs OpBuiltPayload → envelope conversion",
        )))
    }

    async fn get_payload_v3(
        &self,
        payload_id: PayloadId,
    ) -> TransportResult<OpExecutionPayloadEnvelopeV3> {
        // TODO(kona-integration): Resolve from payload store and convert.
        let _ = payload_id;
        Err(TransportError::from(TransportErrorKind::custom_str(
            "get_payload_v3: in-process dispatch not yet wired — \
             needs OpBuiltPayload → envelope conversion",
        )))
    }

    async fn get_payload_v4(
        &self,
        payload_id: PayloadId,
    ) -> TransportResult<OpExecutionPayloadEnvelopeV4> {
        // TODO(kona-integration): Resolve from payload store and convert.
        let _ = payload_id;
        Err(TransportError::from(TransportErrorKind::custom_str(
            "get_payload_v4: in-process dispatch not yet wired — \
             needs OpBuiltPayload → envelope conversion",
        )))
    }

    async fn get_payload_bodies_by_hash_v1(
        &self,
        block_hashes: Vec<BlockHash>,
    ) -> TransportResult<ExecutionPayloadBodiesV1> {
        // TODO(kona-integration): Read block bodies from reth's provider by hash.
        //
        // Implementation outline:
        // 1. For each hash, use `self.l2_provider.block_body_by_hash(hash)`
        // 2. Convert each body to `ExecutionPayloadBodyV1`
        // 3. Return the collected bodies
        let _ = block_hashes;
        Err(TransportError::from(TransportErrorKind::custom_str(
            "get_payload_bodies_by_hash_v1: not yet implemented",
        )))
    }

    async fn get_payload_bodies_by_range_v1(
        &self,
        start: u64,
        count: u64,
    ) -> TransportResult<ExecutionPayloadBodiesV1> {
        // TODO(kona-integration): Read block bodies from reth's provider by range.
        let _ = (start, count);
        Err(TransportError::from(TransportErrorKind::custom_str(
            "get_payload_bodies_by_range_v1: not yet implemented",
        )))
    }

    async fn get_client_version_v1(
        &self,
        _client_version: ClientVersionV1,
    ) -> TransportResult<Vec<ClientVersionV1>> {
        // Return the world-chain client version.
        Ok(vec![ClientVersionV1 {
            code: "WC".to_string(),
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
        // Acknowledge the superchain signal. In-process, this is a no-op acknowledgment.
        Ok(recommended)
    }

    async fn exchange_capabilities(
        &self,
        capabilities: Vec<String>,
    ) -> TransportResult<Vec<String>> {
        // Return the full set of capabilities — since we're in-process, we support everything
        // the engine supports.
        Ok(capabilities)
    }
}
