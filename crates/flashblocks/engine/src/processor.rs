//! The [`WorldChainPayloadProcessor`] — orchestrates flashblock epoch stepping,
//! caching, P2P publishing, and broadcast of [`ExecutedBlock`]s to the engine tree.
//!
//! Validation logic is fully delegated to a [`PendingPayloadValidator`] implementation.
//!
//! Spawned on a dedicated blocking task via [`PayloadEventsStreamValidator::spawn_new`],
//! mirroring reth's `EngineApiTreeHandler::spawn_new` pattern.
//!
//! Pending block state is managed entirely by reth's `CanonicalInMemoryState` —
//! the processor broadcasts `Events::BuiltPayload` which triggers
//! `InsertExecutedBlock` → `set_pending_block` in the engine tree. No separate
//! watch channel is needed.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use alloy_consensus::Header;
use alloy_rpc_types_engine::PayloadId;
use flashblocks_p2p::protocol::{
    event::{ChainEvent, WorldChainEvent, WorldChainEventsStream},
    handler::FlashblocksHandle,
};
use flashblocks_primitives::{
    flashblocks::{Flashblock, Flashblocks},
    p2p::AuthorizedPayload,
    primitives::FlashblocksPayloadV1,
};
use futures::{FutureExt, Stream, StreamExt, stream::ReadyChunks};
use pin_project::pin_project;
use reth_chain_state::{DeferredTrieData, ExecutedBlock};
use reth_node_api::{BuiltPayloadExecutedBlock, Events, NodePrimitives};
use reth_optimism_node::{OpBuiltPayload, OpEngineTypes};
use reth_optimism_primitives::OpPrimitives;
use reth_provider::{BlockNumReader, CanonStateSubscriptions, HeaderProvider};
use tokio::{
    sync::broadcast::Sender,
    task::{JoinHandle, JoinSet},
};
use tracing::{error, trace};

use crate::{
    metrics::PROCESSOR_METRICS,
    validator::{PendingPayloadValidator, ValidatedFlashblock},
};

// ---------------------------------------------------------------------------
// PendingBlockExecutionContext — execution state owned by the stream validator
// ---------------------------------------------------------------------------

/// Epoch-scoped execution state. Tracks the accumulated [`ExecutedBlock`] and
/// the chain of [`DeferredTrieData`] ancestor handles across incremental diffs.
///
/// Reset on epoch boundary (new `payload_id`).
#[derive(Debug, Clone, Default)]
pub struct PendingBlockExecutionContext {
    /// The latest committed executed block from prior flashblocks in this epoch.
    /// `None` at the start of a new epoch.
    pub pending_block: Option<ExecutedBlock<OpPrimitives>>,
    /// The flashblock buffer for the current epoch.
    pub buffered_flashblocks: Flashblocks,
    /// Chain of deferred trie data handles from prior flashblocks in this epoch.
    /// Used to construct `DeferredTrieData::pending()` with ancestor handles,
    /// enabling non-blocking trie resolution.
    pub trie_ancestors: Vec<DeferredTrieData>,
}

// ---------------------------------------------------------------------------
// skip_to_tip — greedy aggregation of buffered flashblocks
// ---------------------------------------------------------------------------

/// Result of draining a chunk of stream events down to per-epoch tips.
pub struct EpochTips {
    /// Ordered list of (tip flashblock, skipped count) per epoch encountered.
    pub tips: Vec<(FlashblocksPayloadV1, u64)>,
}

/// Greedily reduces a chunk of [`WorldChainEvent`]s to per-epoch tips.
///
/// Within a single epoch (same `payload_id`), only the flashblock with the
/// highest index survives — all earlier ones are skipped. Epoch boundaries
/// flush the previous epoch's tip before starting the new one.
///
/// Canon events are ignored — pending block lifecycle is managed entirely by
/// reth's `CanonicalInMemoryState` via the `InsertExecutedBlock` path.
pub fn skip_to_tip(events: Vec<WorldChainEvent<()>>) -> EpochTips {
    let mut tips: Vec<(FlashblocksPayloadV1, u64)> = Vec::new();
    let mut current_epoch: Option<PayloadId> = None;

    for event in events {
        if let WorldChainEvent::Chain(ChainEvent::Pending(flashblock)) = event {
            let flashblock = Arc::try_unwrap(flashblock).unwrap_or_else(|arc| (*arc).clone());

            let is_new_epoch = current_epoch
                .as_ref()
                .is_none_or(|id| *id != flashblock.payload_id);

            if is_new_epoch {
                current_epoch = Some(flashblock.payload_id);
                tips.push((flashblock, 0));
            } else {
                let last = tips.last_mut().expect("current_epoch is set");
                last.0 = flashblock;
                last.1 += 1;
            }
        }
    }

    EpochTips { tips }
}

// ---------------------------------------------------------------------------
// PayloadEventsStreamValidator — owns all processor state, drives flashblock processing
// ---------------------------------------------------------------------------

/// A `Stream`-based driver for the flashblock processing lifecycle.
///
/// Uses [`ReadyChunks`] to greedily drain all buffered events, then
/// [`skip_to_tip`] to reduce them to per-epoch tips before validation.
///
/// Each validated flashblock's trie inputs are chained as
/// [`DeferredTrieData::pending`] ancestor handles, and pre-warmed in the
/// background so the engine tree has resolved trie data when FCU arrives.
#[pin_project]
pub struct PayloadEventsStreamValidator<V, Provider>
where
    V: PendingPayloadValidator,
    Provider: HeaderProvider<Header = Header> + Clone + 'static,
{
    /// The underlying P2P event stream, chunked to drain all ready events at once.
    #[pin]
    stream: ReadyChunks<WorldChainEventsStream<()>>,

    /// Epoch-scoped execution context — owned, not shared.
    ctx: PendingBlockExecutionContext,

    /// P2P handle for publishing flashblocks.
    p2p_handle: FlashblocksHandle,

    /// Broadcast channel for payload events to the engine tree.
    payload_events: Option<Sender<Events<OpEngineTypes>>>,

    /// The [`PendingPayloadValidator`] implementation.
    validator: Arc<V>,

    /// Provider for header lookups.
    provider: Provider,

    /// Background tasks pre-warming deferred trie data. After broadcasting the
    /// `ExecutedBlock` to the engine tree, we spawn a blocking task that calls
    /// `wait_cloned()` to trigger the sort + trie overlay computation. This
    /// ensures the data is `Ready` by the time the tree needs it (FCU, persistence).
    trie_warmup: JoinSet<()>,
}

impl<V, Provider> PayloadEventsStreamValidator<V, Provider>
where
    V: PendingPayloadValidator,
    Provider: HeaderProvider<Header = alloy_consensus::Header> + Clone + Send + 'static,
{
    /// Spawns the stream validator on a blocking task with its own single-threaded
    /// tokio runtime. Returns a [`JoinHandle`] that resolves when the stream ends.
    pub fn spawn_new(
        shutdown: tokio::sync::oneshot::Receiver<()>,
        stream: WorldChainEventsStream<()>,
        p2p_handle: FlashblocksHandle,
        payload_events: Option<Sender<Events<OpEngineTypes>>>,
        validator: Arc<V>,
        provider: Provider,
    ) -> JoinHandle<()> {
        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime for flashblocks processor");

            rt.block_on(async move {
                let mut st = Self {
                    stream: stream.ready_chunks(64),
                    ctx: PendingBlockExecutionContext::default(),
                    p2p_handle,
                    payload_events,
                    validator,
                    provider,
                    trie_warmup: JoinSet::new(),
                };

                tokio::select! {
                    _ = async { while st.next().await.is_some() {} } => {
                        error!(target: "flashblocks::processor", "event stream terminated");
                    }
                    _ = shutdown => {
                        trace!(target: "flashblocks::processor", "shutdown signal received");
                    }
                }

                // Drain remaining warmup tasks before shutdown.
                while let Some(result) = st.trie_warmup.join_next().await {
                    if let Err(e) = result {
                        error!(target: "flashblocks::processor", "trie warmup task panicked: {e:#?}");
                    }
                }
            });
        })
    }
}

// ---------------------------------------------------------------------------
// process_flashblock — public free function for validation + broadcast
// ---------------------------------------------------------------------------

/// Broadcasts a validated flashblock to the engine tree immediately.
///
/// Uses unsorted trie inputs (`Either::Left`). The engine launch loop's
/// `into_executed_payload()` sorts them and sends `InsertExecutedBlock` to
/// the tree handler.
fn broadcast_to_tree(
    payload_events: &Option<Sender<Events<OpEngineTypes>>>,
    validated: &ValidatedFlashblock,
    payload_id: PayloadId,
) {
    let Some(tx) = payload_events else {
        return;
    };

    let built_executed = BuiltPayloadExecutedBlock {
        recovered_block: validated.recovered_block.clone(),
        execution_output: validated.execution_output.clone(),
        hashed_state: either::Left(validated.hashed_state.clone()),
        trie_updates: either::Left(validated.trie_updates.clone()),
    };

    let sealed_block = Arc::new(validated.recovered_block.sealed_block().clone());
    let payload = OpBuiltPayload::new(
        payload_id,
        sealed_block,
        alloy_primitives::U256::ZERO,
        Some(built_executed),
    );

    if let Err(e) = tx.send(Events::BuiltPayload(payload)) {
        PROCESSOR_METRICS.broadcast_failed.increment(1);
        error!("error broadcasting payload to engine tree: {e:#?}");
    }
}

/// Process a single flashblock: validate, broadcast, update state, and
/// kick off background trie warmup.
///
/// This is the core step used by [`PayloadEventsStreamValidator`]'s stream
/// loop. Exposed as a public function so benchmarks can drive it directly.
pub fn process_flashblock<V, Provider>(
    ctx: &mut PendingBlockExecutionContext,
    validator: &V,
    provider: &Provider,
    payload_events: &Option<Sender<Events<OpEngineTypes>>>,
    trie_warmup: &mut JoinSet<()>,
    flashblock: FlashblocksPayloadV1,
) -> eyre::Result<()>
where
    V: PendingPayloadValidator,
    Provider: HeaderProvider<Header = alloy_consensus::Header> + Clone + 'static,
{
    let start = std::time::Instant::now();
    let flashblock = Flashblock { flashblock };

    // --- Epoch detection ---
    let is_new_epoch = ctx.buffered_flashblocks.is_new_payload(&flashblock)?;
    let base = if is_new_epoch {
        flashblock.base().unwrap().clone()
    } else {
        ctx.buffered_flashblocks.base().clone()
    };

    if is_new_epoch {
        ctx.pending_block = None;
        ctx.trie_ancestors.clear();
    }

    let diff = flashblock.diff().clone();
    let index = flashblock.flashblock.index;
    let payload_id = *flashblock.payload_id();

    // Fetch sealed parent header
    let sealed_header = provider
        .sealed_header_by_hash(base.parent_hash)
        .inspect_err(|e| error!("failed to fetch sealed header {}: {e:#?}", base.parent_hash))?
        .ok_or_else(|| {
            eyre::eyre::eyre!("sealed header not found for hash {}", base.parent_hash)
        })?;

    // --- Delegate to validator (synchronous) ---
    let validated = crate::metrics::metered_fn(
        tracing::trace_span!(
            target: "flashblocks::processor",
            "validate_diff",
            %payload_id,
            index,
            path = if diff.access_list_data.is_some() { "bal" } else { "legacy" },
            tx_count = diff.transactions.len(),
            duration_ms = tracing::field::Empty,
        ),
        PROCESSOR_METRICS.validate_diff_duration.clone(),
        |_span| {
            validator.validate_diff(
                ctx.pending_block.as_ref(),
                diff,
                &sealed_header,
                payload_id,
                &base,
            )
        },
    )?;

    // --- Broadcast to engine tree (non-blocking) ---
    broadcast_to_tree(payload_events, &validated, payload_id);

    // --- Construct ExecutedBlock with deferred trie data ---
    let trie_data = DeferredTrieData::pending(
        validated.hashed_state.clone(),
        validated.trie_updates.clone(),
        validated.anchor_hash,
        ctx.trie_ancestors.clone(),
    );

    let executed_block = ExecutedBlock::with_deferred_trie_data(
        validated.recovered_block,
        validated.execution_output,
        trie_data.clone(),
    );

    // --- Pre-warm trie data in the background ---
    let warmup_handle = trie_data.clone();
    trie_warmup.spawn_blocking(move || {
        let _ = warmup_handle.wait_cloned();
    });

    // --- Update execution context ---
    ctx.pending_block = Some(executed_block);
    ctx.buffered_flashblocks.push(flashblock)?;
    ctx.trie_ancestors.push(trie_data);

    trace!(
        target: "flashblocks::processor",
        %payload_id,
        %index,
        "validated flashblock"
    );

    PROCESSOR_METRICS
        .process_flashblock_duration
        .record(start.elapsed().as_secs_f64());

    Ok(())
}

/// Drives a stream of flashblock events through [`process_flashblock`].
///
/// This is the same loop that [`PayloadEventsStreamValidator`] runs internally,
/// exposed as a public async function for benchmarks and tests that want to
/// drive the processor directly without spawning a full blocking task.
pub async fn run_flashblock_processor<V, T, S, Provider>(
    validator: &V,
    stream: S,
    provider: &Provider,
    payload_events: &Option<Sender<Events<OpEngineTypes>>>,
) where
    V: PendingPayloadValidator,
    S: futures::Stream<Item = WorldChainEvent<T>> + Unpin,
    Provider: HeaderProvider<Header = alloy_consensus::Header> + Clone + 'static,
{
    futures::pin_mut!(stream);
    let mut ctx = PendingBlockExecutionContext::default();
    let mut trie_warmup = JoinSet::new();

    while let Some(event) = stream.next().await {
        if let WorldChainEvent::Chain(ChainEvent::Pending(flashblock)) = event {
            let flashblock = Arc::try_unwrap(flashblock).unwrap_or_else(|arc| (*arc).clone());

            if let Err(e) = process_flashblock(
                &mut ctx,
                validator,
                provider,
                payload_events,
                &mut trie_warmup,
                flashblock,
            ) {
                error!("error processing flashblock: {e:#?}");
            }
        }
    }

    // Drain warmup tasks.
    while let Some(result) = trie_warmup.join_next().await {
        if let Err(e) = result {
            error!(target: "flashblocks::processor", "trie warmup task panicked: {e:#?}");
        }
    }
}

impl<V, Provider> Stream for PayloadEventsStreamValidator<V, Provider>
where
    V: PendingPayloadValidator,
    Provider: HeaderProvider<Header = alloy_consensus::Header> + Clone + 'static,
{
    type Item = ();

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        // Drive background trie warmup tasks to completion.
        while let Poll::Ready(Some(result)) = this.trie_warmup.poll_join_next(cx) {
            if let Err(e) = result {
                error!(target: "flashblocks::processor", "trie warmup task panicked: {e:#?}");
            }
        }

        // ReadyChunks yields Vec<WorldChainEvent<()>> — all events that were
        // buffered and ready at poll time. skip_to_tip reduces them to per-epoch tips.
        match this.stream.poll_next(cx) {
            Poll::Ready(Some(chunk)) => {
                let epoch_tips = skip_to_tip(chunk);

                for (tip, skipped) in epoch_tips.tips {
                    if skipped > 0 {
                        trace!(
                            target: "flashblocks::processor",
                            payload_id = %tip.payload_id,
                            index = %tip.index,
                            skipped,
                            "skipped to epoch tip"
                        );
                        PROCESSOR_METRICS.skipped_flashblocks.increment(skipped);
                    }

                    if let Err(e) = process_flashblock(
                        this.ctx,
                        this.validator.as_ref(),
                        this.provider,
                        this.payload_events,
                        this.trie_warmup,
                        tip,
                    ) {
                        error!("error processing flashblock: {e:#?}");
                    }
                }

                Poll::Ready(Some(()))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

// ---------------------------------------------------------------------------
// WorldChainPayloadProcessor — owns the task handle + shutdown signal
// ---------------------------------------------------------------------------

/// The core flashblocks payload processor.
///
/// Owns the shutdown signal and [`JoinHandle`] for the
/// [`PayloadEventsStreamValidator`] blocking task. Implements [`Future`] so it
/// can be driven as a critical task — resolves when the stream terminates or
/// is shut down.
pub struct WorldChainPayloadProcessor {
    /// Shutdown signal for the processor task.
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    /// Handle to the processor blocking task.
    handle: JoinHandle<()>,
}

impl Future for WorldChainPayloadProcessor {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this.handle.poll_unpin(cx) {
            Poll::Ready(result) => {
                if let Err(e) = result {
                    error!(
                        target: "flashblocks::processor",
                        "processor task terminated with error: {e:#?}"
                    );
                }
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// External handle for interacting with the running processor.
///
/// Returned by [`WorldChainPayloadProcessor::spawn_new`]. Provides P2P
/// payload publishing.
#[derive(Debug, Clone)]
pub struct WorldChainPayloadProcessorHandle {
    p2p_handle: FlashblocksHandle,
}

impl WorldChainPayloadProcessorHandle {
    /// Publishes a built payload to P2P.
    pub fn publish_built_payload(
        &self,
        authorized_payload: AuthorizedPayload<FlashblocksPayloadV1>,
    ) -> eyre::Result<()> {
        self.p2p_handle.publish_new(authorized_payload)?;
        Ok(())
    }
}

impl WorldChainPayloadProcessor {
    /// Spawns the processor on a blocking task.
    ///
    /// Returns `(processor, handle)` — the processor is a `Future` that should
    /// be driven as a critical task; the handle is for external consumers.
    ///
    /// Pending block state is managed entirely by reth's `CanonicalInMemoryState`
    /// via the `InsertExecutedBlock` path — no separate watch channel needed.
    pub fn spawn_new<V, Provider, N>(
        p2p_handle: FlashblocksHandle,
        payload_events: Option<Sender<Events<OpEngineTypes>>>,
        validator: V,
        provider: Provider,
    ) -> (Self, WorldChainPayloadProcessorHandle)
    where
        V: PendingPayloadValidator,
        N: NodePrimitives + 'static,
        Provider: HeaderProvider<Header = alloy_consensus::Header>
            + CanonStateSubscriptions<Primitives = N>
            + BlockNumReader
            + Clone
            + Send
            + Sync
            + 'static,
    {
        let handle = WorldChainPayloadProcessorHandle {
            p2p_handle: p2p_handle.clone(),
        };

        let stream = p2p_handle.event_stream(provider.clone(), |_| None);

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        let task_handle = PayloadEventsStreamValidator::spawn_new(
            shutdown_rx,
            stream,
            p2p_handle,
            payload_events,
            Arc::new(validator),
            provider,
        );

        let processor = Self {
            shutdown: Some(shutdown_tx),
            handle: task_handle,
        };

        (processor, handle)
    }

    /// Sends the shutdown signal to the processor task.
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for WorldChainPayloadProcessor {
    fn drop(&mut self) {
        self.shutdown();
    }
}
