use alloy_consensus::BlockHeader;
use alloy_op_evm::OpBlockExecutionCtx;
use eyre::eyre::eyre;
use futures::StreamExt;
use parking_lot::RwLock;
use reth_chain_state::ExecutedBlock;
use reth_evm::ConfigureEvm;
use reth_node_api::{BuiltPayload as _, Events, FullNodeTypes, NodePrimitives, NodeTypes};
use reth_node_builder::BuilderContext;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpNextBlockEnvAttributes;
use reth_optimism_node::{OpBuiltPayload, OpEngineTypes, OpEvmConfig};
use reth_optimism_primitives::OpPrimitives;
use world_chain_p2p::protocol::{
    event::{ChainEvent, WorldChainEvent, WorldChainEventsStream},
    handler::FlashblocksHandle,
};
use world_chain_primitives::{p2p::AuthorizedPayload, primitives::FlashblocksPayloadV1};

use reth_provider::{
    BlockNumReader, CanonStateSubscriptions, ChainSpecProvider, HeaderProvider,
    StateProviderFactory,
};
use std::{
    sync::{Arc, LazyLock},
    time::Instant,
};
use tokio::{
    sync::{
        Semaphore,
        broadcast::{self, Sender},
    },
    task::JoinSet,
};
use tracing::{error, trace};

use crate::{
    flashblock_validation_metrics::FlashblockValidationMetrics,
    validator::FlashblocksBlockValidator,
};
use world_chain_primitives::flashblocks::{Flashblock, Flashblocks};

/// Task-level permit to ensure only one flashblock is processed at a time.
static SEMAPHORE_TASK_PERMIT: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::const_new(1)));

/// The current state of all known pre confirmations received over the P2P layer
/// or generated from the payload building job of this node.
///
/// The state is flushed when FCU is received with a parent hash that matches the block hash
/// of the latest pre confirmation _or_ when an FCU is received that does not match the latest pre confirmation,
/// in which case the pre confirmations were not included as part of the canonical chain.
#[derive(Debug, Clone)]
pub struct FlashblocksExecutionCoordinator {
    inner: Arc<RwLock<FlashblocksExecutionCoordinatorInner>>,
    p2p_handle: FlashblocksHandle,
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
}

#[derive(Debug, Clone)]
pub struct FlashblocksExecutionCoordinatorInner {
    /// List of flashblocks for the current payload
    flashblocks: Flashblocks,
    /// The latest built payload with its associated flashblock index
    latest_payload: Option<(OpBuiltPayload, u64)>,
    /// Broadcast channel for built payload events
    payload_events: Option<Sender<Events<OpEngineTypes>>>,
}

impl FlashblocksExecutionCoordinator {
    /// Creates a new instance of [`FlashblocksStateExecutor`].
    ///
    /// This function spawn a task that handles updates. It should only be called once.
    pub fn new(
        p2p_handle: FlashblocksHandle,
        pending_block: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
    ) -> Self {
        let inner = Arc::new(RwLock::new(FlashblocksExecutionCoordinatorInner {
            flashblocks: Default::default(),
            latest_payload: None,
            payload_events: None,
        }));

        Self {
            inner,
            p2p_handle,
            pending_block,
        }
    }

    /// Maps a closure over the [`WorldChainEventStream<T>`] from the P2P handle.
    pub fn map_worldchain_event_stream<N, P, T, F>(
        &self,
        provider: P,
        mut f: F,
    ) -> WorldChainEventsStream<T>
    where
        P: CanonStateSubscriptions<Primitives = N>
            + HeaderProvider
            + BlockNumReader
            + Clone
            + Send
            + Sync
            + 'static,
        F: FnMut(&WorldChainEvent<T>) -> Option<WorldChainEvent<T>> + Send + 'static,
        T: Send + Sync + 'static,
        N: NodePrimitives + 'static,
    {
        self.p2p_handle
            .event_stream(provider, move |event| f(event))
    }

    pub fn event_hook(
        event: &WorldChainEvent<()>,
        pending_block: &tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
    ) -> Option<WorldChainEvent<()>> {
        if let WorldChainEvent::Chain(ChainEvent::Canon(tip)) = event {
            pending_block.send_if_modified(|block| {
                let should_clear = block.as_ref().is_some_and(|b| {
                    let pending = b.recovered_block();
                    pending.hash() == tip.hash || pending.number() <= tip.number
                });
                if should_clear {
                    *block = None;
                }
                should_clear
            });
        }
        None
    }

    /// Launches the executor to listen for new flashblocks and build payloads.
    ///
    /// Uses a canon-aware event stream that gates flashblock delivery on canonical
    /// tip matching, preventing stale flashblocks from being processed.
    pub fn launch<Node>(self, ctx: &BuilderContext<Node>, evm_config: OpEvmConfig)
    where
        Node: FullNodeTypes,
        Node::Provider: StateProviderFactory
            + HeaderProvider<Header = alloy_consensus::Header>
            + CanonStateSubscriptions,
        Node::Types: NodeTypes<ChainSpec = OpChainSpec>,
    {
        let provider = ctx.provider().clone();
        let pending_block = self.pending_block.clone();
        let pending_block_clone = pending_block.clone();

        let stream = self.map_worldchain_event_stream(provider.clone(), move |event| {
            Self::event_hook(event, &pending_block)
        });

        let chain_spec = ctx.chain_spec().clone();

        let this = Arc::new(self);

        ctx.task_executor()
            .spawn_critical_task("flashblocks executor", async move {
                run_flashblock_processor(
                    this,
                    stream,
                    provider,
                    evm_config,
                    chain_spec,
                    pending_block_clone,
                )
                .await;
            });
    }

    pub fn publish_built_payload(
        &self,
        authorized_payload: AuthorizedPayload<FlashblocksPayloadV1>,
        built_payload: OpBuiltPayload,
    ) -> eyre::Result<()> {
        let flashblock = authorized_payload.msg().clone();

        let index = flashblock.index;
        let flashblock = Flashblock { flashblock };

        let mut lock = self.inner.write();
        lock.flashblocks.push(flashblock.clone())?;
        lock.latest_payload = Some((built_payload, index));
        drop(lock);

        self.p2p_handle.publish_new(authorized_payload.clone())?;

        Ok(())
    }

    /// Returns a reference to the latest flashblock.
    pub fn last(&self) -> Flashblock {
        self.inner.read().flashblocks.last().clone()
    }

    /// Returns a reference to the latest flashblock.
    pub fn flashblocks(&self) -> Flashblocks {
        self.inner.read().flashblocks.clone()
    }

    /// Returns a receiver for the pending block.
    pub fn pending_block(
        &self,
    ) -> tokio::sync::watch::Receiver<Option<ExecutedBlock<OpPrimitives>>> {
        self.pending_block.subscribe()
    }

    /// Registers a new broadcast channel for built payloads.
    pub fn register_payload_events(&self, tx: broadcast::Sender<Events<OpEngineTypes>>) {
        self.inner.write().payload_events = Some(tx);
    }

    /// Broadcasts a new payload to cache in the in memory tree.
    pub fn broadcast_payload(
        &self,
        event: Events<OpEngineTypes>,
        payload_events: Option<broadcast::Sender<Events<OpEngineTypes>>>,
    ) {
        if let Some(payload_events) = payload_events
            && let Err(e) = payload_events.send(event)
        {
            error!("error broadcasting payload: {e:#?}");
        }
    }
}

/// Core flashblock processing loop extracted from [`FlashblocksExecutionCoordinator::launch`].
///
/// Consumes flashblocks from `stream` and processes each one through
/// [`process_flashblock`]. This is the same loop that `launch` spawns as a
/// critical task. The function only returns after any in-flight flashblock
/// processing jobs have completed, which lets benchmarks and tests measure the
/// full processing cost of a bounded stream directly without needing a full
/// [`BuilderContext`].
pub async fn run_flashblock_processor<T, S, Provider>(
    coordinator: Arc<FlashblocksExecutionCoordinator>,
    stream: S,
    provider: Provider,
    evm_config: OpEvmConfig,
    chain_spec: Arc<OpChainSpec>,
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
) where
    S: futures::Stream<Item = WorldChainEvent<T>> + Unpin,
    Provider: StateProviderFactory
        + HeaderProvider<Header = alloy_consensus::Header>
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + Clone
        + 'static,
{
    futures::pin_mut!(stream);
    let mut in_flight = JoinSet::new();
    let mut stream_closed = false;
    let flashblock_validation_metrics = Arc::new(FlashblockValidationMetrics::default());

    loop {
        tokio::select! {
            maybe_event = stream.next(), if !stream_closed => {
                match maybe_event {
                    Some(WorldChainEvent::Chain(ChainEvent::Pending(flashblock))) => {
                        let flashblock = Arc::try_unwrap(flashblock).unwrap_or_else(|arc| (*arc).clone());

                        trace!(
                            target: "flashblocks::coordinator",
                            payload_id = %flashblock.payload_id,
                            index = %flashblock.index,
                            is_base = flashblock.base.is_some(),
                            "received pending flashblock"
                        );

                        let permit = SEMAPHORE_TASK_PERMIT
                            .clone()
                            .acquire_owned()
                            .await
                            .expect("semaphore is never closed");

                        let provider = provider.clone();
                        let evm_config = evm_config.clone();
                        let coordinator = coordinator.clone();
                        let chain_spec = chain_spec.clone();
                        let pending_block = pending_block.clone();

                        let flashblock_validation_metrics_clone = flashblock_validation_metrics.clone();
                        in_flight.spawn_blocking(move || {
                            let _permit = permit;

                            if let Err(e) = process_flashblock(
                                provider,
                                &evm_config,
                                &coordinator,
                                chain_spec,
                                flashblock,
                                pending_block,
                                flashblock_validation_metrics_clone
                            ) {
                                error!("error processing flashblock: {e:#?}");
                            }
                        });
                    }
                    Some(_) => {}
                    None => {
                        stream_closed = true;
                    }
                }
            }
            join_result = in_flight.join_next(), if !in_flight.is_empty() => {
                if let Some(Err(err)) = join_result {
                    error!("flashblock processing task failed: {err}");
                }
            }
            else => {
                if stream_closed {
                    break;
                }
            }
        }
    }
}

pub fn process_flashblock<Provider>(
    provider: Provider,
    evm_config: &OpEvmConfig,
    coordinator: &FlashblocksExecutionCoordinator,
    chain_spec: Arc<OpChainSpec>,
    flashblock: FlashblocksPayloadV1,
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
    flashblock_validation_metrics: Arc<FlashblockValidationMetrics>,
) -> eyre::Result<()>
where
    Provider: StateProviderFactory
        + HeaderProvider<Header = alloy_consensus::Header>
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + Clone
        + 'static,
{
    let process_flashblock_started = Instant::now();
    let flashblock = Flashblock { flashblock };

    // --- Short read: check if already processed, extract base info ---
    let (base, is_new_epoch) = {
        let inner = coordinator.inner.read();

        if let Some(latest_payload) = &inner.latest_payload
            && latest_payload.0.id() == flashblock.flashblock.payload_id
            && latest_payload.1 >= flashblock.flashblock.index
        {
            flashblock_validation_metrics.increment_already_processed_flashblocks();
            pending_block.send_replace(
                latest_payload
                    .0
                    .executed_block()
                    .map(|p| p.into_executed_payload()),
            );
            return Ok(());
        }

        let is_new = match inner.flashblocks.is_new_payload(&flashblock) {
            Ok(is_new) => is_new,
            Err(err) => {
                flashblock_validation_metrics.increment_validation_errors();
                return Err(err);
            }
        };
        let base = if is_new {
            flashblock.base().unwrap().clone()
        } else {
            inner.flashblocks.base().clone()
        };

        (base, is_new)
    };

    // Clear latest_payload on new epoch (brief write lock)
    if is_new_epoch {
        coordinator.inner.write().latest_payload = None;
    }

    let latest_payload = {
        let inner = coordinator.inner.read();
        inner.latest_payload.as_ref().map(|(p, _)| p.clone())
    };

    let diff = flashblock.diff().clone();
    let index = flashblock.flashblock.index;

    // This should never fail — the canon-aware event stream guarantees the
    // parent header is available by the time we process a flashblock.
    let sealed_header = match provider.sealed_header_by_hash(base.parent_hash) {
        Ok(Some(header)) => header,
        Ok(None) => {
            flashblock_validation_metrics.increment_validation_errors();
            return Err(eyre!(
                "sealed header not found for hash {}",
                base.parent_hash
            ));
        }
        Err(e) => {
            error!("failed to fetch sealed header {}: {e:#?}", base.parent_hash);
            flashblock_validation_metrics.increment_validation_errors();
            return Err(e.into());
        }
    };

    let execution_context = OpBlockExecutionCtx {
        parent_hash: base.parent_hash,
        parent_beacon_block_root: Some(base.parent_beacon_block_root),
        extra_data: base.extra_data.clone(),
    };

    let next_block_context = OpNextBlockEnvAttributes {
        timestamp: base.timestamp,
        suggested_fee_recipient: base.fee_recipient,
        prev_randao: base.prev_randao,
        gas_limit: base.gas_limit,
        parent_beacon_block_root: Some(base.parent_beacon_block_root),
        extra_data: base.extra_data.clone(),
    };

    let evm_env = evm_config.next_evm_env(sealed_header.header(), &next_block_context)?;

    let sealed_header = Arc::new(sealed_header);

    let block_validator = FlashblocksBlockValidator {
        chain_spec: chain_spec.clone(),
        evm_config: evm_config.clone(),
        execution_context: execution_context.clone(),
        evm_env: evm_env.clone(),
        header: sealed_header.clone(),
        flashblock_validation_metrics: flashblock_validation_metrics.clone(),
    };

    let next_payload = block_validator.validate_flashblock_with_state(
        provider,
        diff.clone(),
        &sealed_header,
        flashblock.flashblock.payload_id,
        latest_payload.as_ref(),
    )?;

    {
        let mut inner = coordinator.inner.write();
        inner.latest_payload = Some((next_payload.clone(), index));
        if let Err(err) = inner.flashblocks.push(flashblock) {
            flashblock_validation_metrics.increment_validation_errors();
            return Err(err);
        }
    }

    let into_executed_block_started = Instant::now();
    pending_block.send_replace(
        next_payload
            .executed_block()
            .map(|p| p.into_executed_payload()),
    );
    flashblock_validation_metrics.record_into_executed_block(into_executed_block_started.elapsed());

    trace!(
        target: "flashblocks::state_executor",
        id = %next_payload.id(),
        index = %index,
        block_hash = %next_payload.block().hash(),
        "built payload from flashblock"
    );

    let payload_events = coordinator.inner.read().payload_events.clone();
    coordinator.broadcast_payload(Events::BuiltPayload(next_payload), payload_events);

    flashblock_validation_metrics
        .record_full_process_flashblock(process_flashblock_started.elapsed());
    Ok(())
}
