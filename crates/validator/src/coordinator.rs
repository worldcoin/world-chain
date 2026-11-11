use alloy_consensus::BlockHeader;
use alloy_eips::BlockNumHash;
use alloy_op_evm::OpBlockExecutionCtx;
use alloy_primitives::B256;
use eyre::eyre::eyre;
use futures::{StreamExt, future::OptionFuture};
use parking_lot::RwLock;
use reth_chain_state::ExecutedBlock;
use reth_evm::ConfigureEvm;
use reth_node_api::{BuiltPayload as _, Events, FullNodeTypes, NodePrimitives, NodeTypes};
use reth_node_builder::BuilderContext;
use reth_optimism_evm::OpNextBlockEnvAttributes;
use reth_optimism_node::{OpBuiltPayload, OpEngineTypes};
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
    future::Future,
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
use tokio_stream::wrappers::BroadcastStream;
use tracing::{error, trace};

use crate::{
    execution_strategy::{FlashblocksBalExecutionStrategy, FlashblocksLegacyExecutionStrategy},
    flashblock_types::{BalFlashblockTypes, LegacyFlashblockTypes},
    flashblock_validation_metrics::FlashblockValidationMetrics,
    preconfirmed::{ValidatedEvent, ValidatedFlashblock, ValidatedSnapshot},
    validator::{FlashblocksBlockValidator, into_executed_payload},
};
use world_chain_chainspec::WorldChainSpec;
use world_chain_evm::WorldChainEvmConfig;
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
    validated_events: Sender<ValidatedEvent>,
}

#[derive(Debug, Clone)]
pub struct FlashblocksExecutionCoordinatorInner {
    /// List of flashblocks for the current payload
    flashblocks: Flashblocks,
    /// The latest built payload with its associated flashblock index
    latest_payload: Option<(OpBuiltPayload, u64)>,
    latest_payload_at: Option<Instant>,
    /// Unbound only for a local build whose parent is not yet observed canonical.
    latest_payload_generation: Option<u64>,
    /// Broadcast channel for built payload events
    payload_events: Option<Sender<Events<OpEngineTypes>>>,
    validated: ValidatedSnapshot,
    validated_sequence: u64,
}

#[derive(Debug, Clone)]
pub struct CoordinatorSnapshot {
    pub flashblocks: Flashblocks,
    pub latest_payload: Option<(OpBuiltPayload, u64)>,
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
            latest_payload_at: None,
            latest_payload_generation: None,
            payload_events: None,
            validated: ValidatedSnapshot::default(),
            validated_sequence: 0,
        }));
        let (validated_events, _) = broadcast::channel(2);

        Self {
            inner,
            p2p_handle,
            pending_block,
            validated_events,
        }
    }

    /// Returns a future that resolves once the flashblock executing `hash` broadcasts
    /// its executed payload. If `hash` was already executed and broadcast, the caller's
    /// validation gets a full engine-tree cache hit and wins the race instead; if no
    /// flashblock produces `hash`, this future never resolves and simply loses.
    pub fn resolve_pending(&self, hash: B256) -> impl Future<Output = ()> {
        let rx = self
            .inner
            .read()
            .payload_events
            .as_ref()
            .map(Sender::subscribe);

        async move {
            let matched = OptionFuture::from(rx.map(|rx| {
                BroadcastStream::new(rx).any(move |event| {
                    std::future::ready(matches!(
                        event,
                        Ok(Events::BuiltPayload(payload)) if payload.block().hash() == hash
                    ))
                })
            }))
            .await
            .unwrap_or(false);

            // Pend forever when unmatched so the caller's full validation wins the race.
            if !matched {
                std::future::pending::<()>().await;
            }
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

    /// Updates the source checkpoint without assigning canonical log semantics.
    pub fn observe_canonical_tip(&self, tip: BlockNumHash) {
        let mut inner = self.inner.write();
        if inner.validated.canonical_tip == Some(tip) {
            return;
        }
        let initial = inner.validated.canonical_tip.is_none();
        let generation = if initial {
            inner.validated.canonical_generation
        } else {
            let Some(generation) = inner.validated.canonical_generation.checked_add(1) else {
                error!(target: "flashblocks::coordinator", "validated publication generation exhausted");
                return;
            };
            generation
        };
        inner.validated = ValidatedSnapshot {
            canonical_tip: Some(tip),
            canonical_generation: generation,
            current: None,
        };
        let bind_local = inner.latest_payload_generation.is_none()
            && inner.latest_payload.as_ref().is_some_and(|(payload, _)| {
                payload.block().parent_hash() == tip.hash
                    && payload.block().number().checked_sub(1) == Some(tip.number)
            });
        if bind_local {
            inner.latest_payload_generation = Some(generation);
        }
        // No receivers is normal before the RPC feed starts.
        let _ = self.validated_events.send(ValidatedEvent::Reset {
            canonical_tip: Some(tip),
            canonical_generation: generation,
        });
        let bootstrap = (initial || bind_local)
            .then(|| {
                let (payload, index) = inner.latest_payload.as_ref()?;
                Some((
                    payload.id(),
                    *index,
                    inner.latest_payload_generation?,
                    self.pending_block.borrow().clone()?,
                    inner.latest_payload_at?,
                ))
            })
            .flatten();
        drop(inner);
        if let Some((payload_id, index, generation, executed, validated_at)) = bootstrap {
            self.publish_validated(payload_id, index, generation, executed, validated_at);
        }
    }

    /// Subscribes and captures the current source state under the publication lock.
    pub fn subscribe_validated(&self) -> (ValidatedSnapshot, broadcast::Receiver<ValidatedEvent>) {
        let inner = self.inner.read();
        let receiver = self.validated_events.subscribe();
        (inner.validated.clone(), receiver)
    }

    /// Captures the current source checkpoint and validated payload atomically.
    pub fn snapshot_validated(&self) -> ValidatedSnapshot {
        self.inner.read().validated.clone()
    }

    fn publish_validated(
        &self,
        payload_id: alloy_rpc_types_engine::PayloadId,
        index: u64,
        canonical_generation: u64,
        executed: ExecutedBlock<OpPrimitives>,
        validated_at: Instant,
    ) {
        let mut inner = self.inner.write();
        let Some(tip) = inner.validated.canonical_tip else {
            return;
        };
        let block = executed.recovered_block();
        if canonical_generation != inner.validated.canonical_generation
            || block.parent_hash() != tip.hash
            || block.number().checked_sub(1) != Some(tip.number)
            || !inner
                .latest_payload
                .as_ref()
                .is_some_and(|(latest, latest_index)| {
                    latest.id() == payload_id
                        && *latest_index == index
                        && latest.block().hash() == block.hash()
                })
        {
            return;
        }
        if inner
            .validated
            .current
            .as_ref()
            .is_some_and(|current| current.payload_id == payload_id && current.index >= index)
        {
            return;
        }
        let Some(sequence) = inner.validated_sequence.checked_add(1) else {
            error!(target: "flashblocks::coordinator", "validated publication sequence exhausted");
            return;
        };
        let payload = ValidatedFlashblock {
            sequence,
            canonical_generation,
            payload_id,
            index,
            executed: Arc::new(executed),
            validated_at,
        };
        inner.validated_sequence = sequence;
        inner.validated.current = Some(payload.clone());
        // No receivers is normal before the RPC feed starts.
        let _ = self.validated_events.send(ValidatedEvent::Payload(payload));
    }

    /// Launches the executor to listen for new flashblocks and build payloads.
    ///
    /// Uses a canon-aware event stream that gates flashblock delivery on canonical
    /// tip matching, preventing stale flashblocks from being processed.
    pub fn launch<Node>(self, ctx: &BuilderContext<Node>, evm_config: WorldChainEvmConfig)
    where
        Node: FullNodeTypes,
        Node::Provider: StateProviderFactory
            + HeaderProvider<Header = alloy_consensus::Header>
            + CanonStateSubscriptions,
        Node::Types: NodeTypes<ChainSpec = WorldChainSpec>,
    {
        let provider = ctx.provider().clone();
        let pending_block = self.pending_block.clone();
        let pending_block_clone = pending_block.clone();
        let coordinator = self.clone();

        let stream = self.map_worldchain_event_stream(provider.clone(), move |event| {
            if let WorldChainEvent::Chain(ChainEvent::Canon(tip)) = event {
                coordinator.observe_canonical_tip(*tip);
            }
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
        let checkpoint_matches = lock.validated.canonical_tip.is_some_and(|tip| {
            built_payload.block().parent_hash() == tip.hash
                && built_payload.block().number().checked_sub(1) == Some(tip.number)
        });
        lock.latest_payload = Some((built_payload, index));
        lock.latest_payload_at = Some(Instant::now());
        lock.latest_payload_generation =
            checkpoint_matches.then_some(lock.validated.canonical_generation);
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

    /// Returns the cloned `latest_payload` if present
    pub fn latest_payload(&self) -> Option<OpBuiltPayload> {
        self.inner.read().latest_payload.clone().map(|p| p.0)
    }

    /// Fetches an atomic snapshot of the [`FlashblocksExecutionCoordinator`]
    pub fn snapshot(&self) -> CoordinatorSnapshot {
        let inner = self.inner.read();

        let latest_payload = inner.latest_payload.clone();
        let flashblocks = inner.flashblocks.clone();

        CoordinatorSnapshot {
            flashblocks,
            latest_payload,
        }
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
    evm_config: WorldChainEvmConfig,
    chain_spec: Arc<WorldChainSpec>,
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
) where
    S: futures::Stream<Item = WorldChainEvent<T>> + Unpin,
    Provider: StateProviderFactory
        + HeaderProvider<Header = alloy_consensus::Header>
        + ChainSpecProvider<ChainSpec = WorldChainSpec>
        + Clone
        + Sync
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
    evm_config: &WorldChainEvmConfig,
    coordinator: &FlashblocksExecutionCoordinator,
    chain_spec: Arc<WorldChainSpec>,
    flashblock: FlashblocksPayloadV1,
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
    flashblock_validation_metrics: Arc<FlashblockValidationMetrics>,
) -> eyre::Result<()>
where
    Provider: StateProviderFactory
        + HeaderProvider<Header = alloy_consensus::Header>
        + ChainSpecProvider<ChainSpec = WorldChainSpec>
        + Clone
        + Sync
        + 'static,
{
    let process_flashblock_started = Instant::now();
    let flashblock = Flashblock { flashblock };

    let cached = {
        let inner = coordinator.inner.read();
        inner.latest_payload.as_ref().and_then(|(payload, index)| {
            (payload.id() == flashblock.flashblock.payload_id
                && *index >= flashblock.flashblock.index)
                .then(|| {
                    (
                        payload.clone(),
                        *index,
                        inner.latest_payload_generation,
                        inner.latest_payload_at,
                    )
                })
        })
    };
    if let Some((payload, index, generation, validated_at)) = cached {
        flashblock_validation_metrics.increment_already_processed_flashblocks();
        let executed = payload.executed_block().map(into_executed_payload);
        if let (Some(executed), Some(validated_at), Some(generation)) =
            (&executed, validated_at, generation)
        {
            coordinator.publish_validated(
                payload.id(),
                index,
                generation,
                executed.clone(),
                validated_at,
            );
        }
        pending_block.send_replace(executed);
        return Ok(());
    }

    // --- Short read: extract base info and the validation checkpoint ---
    let (base, is_new_epoch, canonical_generation) = {
        let inner = coordinator.inner.read();

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

        (base, is_new, inner.validated.canonical_generation)
    };

    // Clear latest_payload on new epoch (brief write lock)
    if is_new_epoch {
        let mut inner = coordinator.inner.write();
        inner.latest_payload = None;
        inner.latest_payload_at = None;
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
        no_user_tx_activation_block: Default::default(),
        post_exec_mode: Default::default(),
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

    let has_bal = diff.access_list_data.is_some();

    let next_payload = if has_bal {
        FlashblocksBlockValidator::<_, BalFlashblockTypes>::new(
            chain_spec.clone(),
            evm_env.clone(),
            execution_context.clone(),
            flashblock_validation_metrics.clone(),
            FlashblocksBalExecutionStrategy,
        )
        .validate_flashblock_with_state(
            provider,
            diff,
            &sealed_header,
            flashblock.flashblock.payload_id,
            latest_payload.as_ref(),
        )?
    } else {
        FlashblocksBlockValidator::<_, LegacyFlashblockTypes>::new(
            chain_spec.clone(),
            evm_env.clone(),
            execution_context.clone(),
            flashblock_validation_metrics.clone(),
            FlashblocksLegacyExecutionStrategy,
        )
        .validate_flashblock_with_state(
            provider,
            diff,
            &sealed_header,
            flashblock.flashblock.payload_id,
            latest_payload.as_ref(),
        )?
    };

    let validated_at = Instant::now();
    {
        let mut inner = coordinator.inner.write();
        inner.latest_payload = Some((next_payload.clone(), index));
        inner.latest_payload_at = Some(validated_at);
        inner.latest_payload_generation = Some(canonical_generation);
        if let Err(err) = inner.flashblocks.push(flashblock) {
            flashblock_validation_metrics.increment_validation_errors();
            return Err(err);
        }
    }

    let into_executed_block_started = Instant::now();
    let executed = next_payload.executed_block().map(into_executed_payload);
    if let Some(executed) = &executed {
        coordinator.publish_validated(
            next_payload.id(),
            index,
            canonical_generation,
            executed.clone(),
            validated_at,
        );
    }
    pending_block.send_replace(executed);
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::{Block, BlockBody, Header};
    use alloy_primitives::U256;
    use alloy_rpc_types_engine::PayloadId;
    use std::time::Duration;

    use reth_node_api::BuiltPayloadExecutedBlock;
    use reth_optimism_primitives::OpTransactionSigned;
    use reth_primitives_traits::{RecoveredBlock, SealedBlock};
    use world_chain_primitives::{
        ed25519_dalek::SigningKey, p2p::Authorization, primitives::ExecutionPayloadBaseV1,
    };
    use world_chain_test_utils::builder::{BenchProvider, CHAIN_SPEC, EVM_CONFIG};

    fn test_coordinator() -> (
        FlashblocksExecutionCoordinator,
        broadcast::Sender<Events<OpEngineTypes>>,
    ) {
        let sk = SigningKey::from_bytes(&[1u8; 32]);
        let handle = FlashblocksHandle::new(sk.verifying_key(), Some(sk));
        let (pending_tx, _pending_rx) = tokio::sync::watch::channel(None);
        let coordinator = FlashblocksExecutionCoordinator::new(handle, pending_tx);
        let (events_tx, _events_rx) = broadcast::channel(16);
        coordinator.register_payload_events(events_tx.clone());
        (coordinator, events_tx)
    }

    /// Builds an [`OpBuiltPayload`] over an empty block; `number` seeds a distinct
    /// block hash. Returns the payload and its block hash.
    fn built_payload(number: u64) -> (OpBuiltPayload, B256) {
        let header = Header {
            number,
            ..Default::default()
        };
        let block = SealedBlock::seal_slow(Block {
            header,
            body: BlockBody::<OpTransactionSigned>::default(),
        });
        let hash = block.hash();
        let payload =
            OpBuiltPayload::new(PayloadId::new([0u8; 8]), Arc::new(block), U256::ZERO, None);
        (payload, hash)
    }

    fn cache_executed_payload(
        coordinator: &FlashblocksExecutionCoordinator,
        parent: BlockNumHash,
        index: u64,
        validated_at: Instant,
    ) -> (OpBuiltPayload, ExecutedBlock<OpPrimitives>) {
        let block = Block {
            header: Header {
                number: parent.number + 1,
                parent_hash: parent.hash,
                timestamp: index + 1,
                ..Default::default()
            },
            body: BlockBody::<OpTransactionSigned>::default(),
        };
        let executed: ExecutedBlock<OpPrimitives> = ExecutedBlock {
            recovered_block: Arc::new(RecoveredBlock::new_unhashed(block.clone(), vec![])),
            ..Default::default()
        };
        let payload = OpBuiltPayload::new(
            PayloadId::new([1u8; 8]),
            Arc::new(SealedBlock::seal_slow(block)),
            U256::ZERO,
            Some(BuiltPayloadExecutedBlock {
                recovered_block: executed.recovered_block.clone(),
                execution_output: executed.execution_output.clone(),
                hashed_state: Arc::default(),
                trie_updates: Arc::default(),
            }),
        );
        let mut inner = coordinator.inner.write();
        inner.latest_payload = Some((payload.clone(), index));
        inner.latest_payload_at = Some(validated_at);
        inner.latest_payload_generation = Some(inner.validated.canonical_generation);
        (payload, executed)
    }

    fn process_cached_payload(
        coordinator: &FlashblocksExecutionCoordinator,
        payload_id: PayloadId,
        index: u64,
    ) {
        process_flashblock(
            BenchProvider::new(),
            &EVM_CONFIG,
            coordinator,
            CHAIN_SPEC.clone(),
            FlashblocksPayloadV1 {
                payload_id,
                index,
                ..Default::default()
            },
            coordinator.pending_block.clone(),
            Arc::new(FlashblockValidationMetrics::default()),
        )
        .expect("cached builder payload");
    }

    #[tokio::test]
    async fn cached_builder_payload_publishes_once_without_refreshing_age() {
        let (coordinator, _) = test_coordinator();
        let parent = BlockNumHash::new(10, B256::repeat_byte(1));
        coordinator.observe_canonical_tip(parent);
        let validated_at = Instant::now() - Duration::from_secs(1);
        let (payload, _) = cache_executed_payload(&coordinator, parent, 0, validated_at);
        let (_, mut receiver) = coordinator.subscribe_validated();

        process_cached_payload(&coordinator, payload.id(), 0);
        let ValidatedEvent::Payload(first) = receiver.try_recv().expect("first publication") else {
            panic!("expected validated payload");
        };
        assert_eq!(first.sequence, 1);
        assert_eq!(first.validated_at, validated_at);

        process_cached_payload(&coordinator, payload.id(), 0);
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        let current = coordinator
            .snapshot_validated()
            .current
            .expect("current payload");
        assert_eq!(current.sequence, first.sequence);
        assert_eq!(current.validated_at, validated_at);
    }

    #[tokio::test]
    async fn initial_checkpoint_publishes_already_executed_builder_payload() {
        let (coordinator, _) = test_coordinator();
        let parent = BlockNumHash::new(10, B256::repeat_byte(1));
        let validated_at = Instant::now() - Duration::from_secs(1);
        let (payload, _) = cache_executed_payload(&coordinator, parent, 0, validated_at);
        process_cached_payload(&coordinator, payload.id(), 0);
        assert!(coordinator.snapshot_validated().current.is_none());

        coordinator.observe_canonical_tip(parent);
        let (snapshot, mut receiver) = coordinator.subscribe_validated();
        assert_eq!(snapshot.canonical_generation, 0);
        let current = snapshot.current.expect("bootstrap publication");
        assert_eq!(current.sequence, 1);
        assert_eq!(current.validated_at, validated_at);
        process_cached_payload(&coordinator, payload.id(), 0);
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn local_builder_binds_once_when_its_parent_becomes_canonical() {
        let (coordinator, _) = test_coordinator();
        coordinator.observe_canonical_tip(BlockNumHash::new(10, B256::repeat_byte(1)));
        let parent = BlockNumHash::new(11, B256::repeat_byte(2));
        let (payload, _) = cache_executed_payload(&coordinator, parent, 0, Instant::now());
        let signing_key = SigningKey::from_bytes(&[1u8; 32]);
        let authorization = Authorization::new(
            payload.id(),
            payload.block().timestamp(),
            &signing_key,
            signing_key.verifying_key(),
        );
        coordinator
            .p2p_handle
            .start_publishing(authorization)
            .unwrap();
        let flashblock = FlashblocksPayloadV1 {
            payload_id: payload.id(),
            index: 0,
            base: Some(ExecutionPayloadBaseV1 {
                parent_hash: parent.hash,
                block_number: parent.number + 1,
                timestamp: payload.block().timestamp(),
                ..Default::default()
            }),
            ..Default::default()
        };
        coordinator
            .publish_built_payload(
                AuthorizedPayload::new(&signing_key, authorization, flashblock),
                payload.clone(),
            )
            .unwrap();
        let validated_at = coordinator.inner.read().latest_payload_at.unwrap();
        assert_eq!(coordinator.inner.read().latest_payload_generation, None);
        process_cached_payload(&coordinator, payload.id(), 0);
        assert!(coordinator.snapshot_validated().current.is_none());
        let (_, mut receiver) = coordinator.subscribe_validated();

        coordinator.observe_canonical_tip(parent);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            ValidatedEvent::Reset { .. }
        ));
        let ValidatedEvent::Payload(published) = receiver.try_recv().unwrap() else {
            panic!("expected newly bound local payload");
        };
        assert_eq!(published.canonical_generation, 1);
        assert_eq!(published.validated_at, validated_at);

        coordinator.observe_canonical_tip(BlockNumHash::new(12, B256::repeat_byte(3)));
        coordinator.observe_canonical_tip(parent);
        let (_, mut receiver) = coordinator.subscribe_validated();
        process_cached_payload(&coordinator, payload.id(), 0);
        assert!(coordinator.snapshot_validated().current.is_none());
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn validated_snapshot_and_subscription_cover_following_updates() {
        let (coordinator, _) = test_coordinator();
        let parent = BlockNumHash::new(10, B256::repeat_byte(1));
        coordinator.observe_canonical_tip(parent);
        let (payload, executed) = cache_executed_payload(&coordinator, parent, 0, Instant::now());
        coordinator.publish_validated(payload.id(), 0, 0, executed, Instant::now());

        let (snapshot, mut receiver) = coordinator.subscribe_validated();
        assert_eq!(snapshot.current.expect("snapshot payload").sequence, 1);
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        let (payload, executed) = cache_executed_payload(&coordinator, parent, 1, Instant::now());
        coordinator.publish_validated(payload.id(), 1, 0, executed, Instant::now());
        let ValidatedEvent::Payload(next) = receiver.try_recv().expect("following update") else {
            panic!("expected validated payload");
        };
        assert_eq!(next.sequence, 2);
        assert_eq!(next.index, 1);

        let canonical_tip = BlockNumHash::new(payload.block().number(), payload.block().hash());
        coordinator.observe_canonical_tip(canonical_tip);
        assert!(matches!(
            receiver.try_recv().expect("checkpoint reset"),
            ValidatedEvent::Reset {
                canonical_tip: Some(tip),
                canonical_generation: 1,
            } if tip == canonical_tip
        ));
        assert!(coordinator.snapshot_validated().current.is_none());
        coordinator.observe_canonical_tip(canonical_tip);
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn old_generation_cannot_republish_when_parent_checkpoint_returns() {
        let (coordinator, _) = test_coordinator();
        let parent = BlockNumHash::new(10, B256::repeat_byte(1));
        coordinator.observe_canonical_tip(parent);
        let validated_at = Instant::now();
        let (payload, executed) = cache_executed_payload(&coordinator, parent, 0, validated_at);
        coordinator.observe_canonical_tip(BlockNumHash::new(11, B256::repeat_byte(2)));
        coordinator.observe_canonical_tip(parent);
        let (_, mut receiver) = coordinator.subscribe_validated();

        coordinator.publish_validated(payload.id(), 0, 0, executed, validated_at);
        process_cached_payload(&coordinator, payload.id(), 0);

        assert!(coordinator.snapshot_validated().current.is_none());
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        assert!(coordinator.pending_block().borrow().is_some());
    }

    #[tokio::test]
    async fn source_lag_is_explicit_and_snapshot_retains_latest_checkpoint() {
        let (coordinator, _) = test_coordinator();
        let (_, mut receiver) = coordinator.subscribe_validated();
        for number in 1..=3 {
            coordinator.observe_canonical_tip(BlockNumHash::new(number, B256::ZERO));
        }
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(1))
        ));
        let snapshot = coordinator.snapshot_validated();
        assert_eq!(
            snapshot.canonical_tip,
            Some(BlockNumHash::new(3, B256::ZERO))
        );
        assert_eq!(snapshot.canonical_generation, 2);
    }

    #[tokio::test]
    async fn resolve_pending_resolves_on_broadcast() {
        let (coordinator, events_tx) = test_coordinator();
        let (payload, hash) = built_payload(1);

        let task = {
            let coordinator = coordinator.clone();
            tokio::spawn(async move { coordinator.resolve_pending(hash).await })
        };

        // Let the task subscribe before the payload is broadcast.
        tokio::time::sleep(Duration::from_millis(50)).await;
        events_tx.send(Events::BuiltPayload(payload)).unwrap();

        // Resolves once the matching payload is broadcast.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), task)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn resolve_pending_never_resolves_without_broadcast() {
        let (coordinator, _events_tx) = test_coordinator();

        // Nothing is broadcast → never resolves, leaving validation to win the race.
        let resolved = tokio::time::timeout(
            Duration::from_millis(50),
            coordinator.resolve_pending(B256::repeat_byte(9)),
        )
        .await;
        assert!(resolved.is_err());
    }

    #[tokio::test]
    async fn resolve_pending_ignores_nonmatching_broadcast() {
        let (coordinator, events_tx) = test_coordinator();
        let (other_payload, _other_hash) = built_payload(1);

        let task = {
            let coordinator = coordinator.clone();
            tokio::spawn(async move { coordinator.resolve_pending(B256::repeat_byte(9)).await })
        };

        tokio::time::sleep(Duration::from_millis(50)).await;
        events_tx.send(Events::BuiltPayload(other_payload)).unwrap();

        // A broadcast for a different block hash must not resolve the future.
        let resolved = tokio::time::timeout(Duration::from_millis(50), task).await;
        assert!(resolved.is_err());
    }
}
