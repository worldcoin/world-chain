use std::sync::Arc;

use alloy_op_evm::{OpBlockExecutionCtx, OpBlockExecutor, OpEvmFactory};
use eyre::eyre::eyre;
use flashblocks_p2p::protocol::{
    event::{ChainEvent, WorldChainEvent, WorldChainEventsStream},
    handler::FlashblocksHandle,
};
use flashblocks_primitives::{
    flashblocks::{Flashblock, Flashblocks},
    p2p::AuthorizedPayload,
    primitives::{ExecutionPayloadBaseV1, FlashblocksPayloadV1},
};
use futures::StreamExt;
use parking_lot::RwLock;
use reth::rpc::api::eth::helpers::pending_block;
use reth_chain_state::ExecutedBlock;
use reth_evm::{ConfigureEvm, EvmFactory, block::BlockExecutionError, execute::BlockBuilder};
use reth_node_api::{BuiltPayload as _, Events, FullNodeTypes, NodePrimitives, NodeTypes};
use reth_node_builder::BuilderContext;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpNextBlockEnvAttributes, OpRethReceiptBuilder};
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::{OpBuiltPayload, OpEngineTypes, OpEvmConfig};
use reth_optimism_primitives::OpPrimitives;
use reth_provider::{
    BlockNumReader, CanonStateSubscriptions, ChainSpecProvider, HeaderProvider, StateProvider,
    StateProviderFactory,
};
use tokio::{
    sync::broadcast::{self, Sender},
    task::JoinSet,
};
use tokio_stream::wrappers::BroadcastStream;
use tracing::{error, trace};

use crate::{
    FlashblockBlockValidator,
    executor::FlashblocksBlockBuilder,
    processor::{FlashblockProcessor, PendingBlockNotification},
};

use crate::processor::PendingPayloadProcessor;

const BROADCAST_CAPACITY: usize = 12;

/// Orchestrates flashblock execution: receives flashblocks from the P2P event
/// stream, spawns per-epoch [`PendingPayloadProcessor`]s, and routes results
/// through the [`FlashblockProcessor`] notification channel.
///
/// Read-only accessors (`last`, `flashblocks`, `pending_block`) are delegated
/// to the shared [`FlashblockProcessor`].
#[derive(Debug, Clone)]
pub struct FlashblocksExecutionCoordinator {
    p2p_handle: FlashblocksHandle,

    notifications_sender: broadcast::Sender<PendingBlockNotification>,
}

impl FlashblocksExecutionCoordinator {
    pub fn new(p2p_handle: FlashblocksHandle) -> Self {
        let (notifications_sender, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            p2p_handle,
            notifications_sender,
        }
    }

    pub fn event_stream<N, P, T, F>(&self, provider: P, mut f: F) -> WorldChainEventsStream<T>
    where
        P: CanonStateSubscriptions<Primitives = N>
            + HeaderProvider
            + BlockNumReader
            + Clone
            + Send
            + StateProvider
            + Sync
            + 'static,
        F: FnMut(&WorldChainEvent<T>) -> Option<WorldChainEvent<T>> + Send + 'static,
        T: Send + Sync + 'static,
        N: NodePrimitives + 'static,
    {
        self.p2p_handle
            .event_stream(provider, move |event| f(event))
    }

    pub fn launch<Node>(
        self,
        ctx: &BuilderContext<Node>,
        evm_config: OpEvmConfig,
        payload_events_tx: broadcast::Sender<Events<OpEngineTypes>>,
        pending_block_tx: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
    ) where
        Node: FullNodeTypes,
        Node::Provider: StateProviderFactory
            + HeaderProvider<Header = alloy_consensus::Header>
            + CanonStateSubscriptions
            + StateProvider
            + Clone
            + 'static,
        Node::Types: NodeTypes<ChainSpec = OpChainSpec>,
    {
        let provider = ctx.provider().clone();
        let processor = FlashblockProcessor::new(
            pending_block_tx.clone(),
            self.notifications_sender.clone(),
            payload_events_tx,
        );

        ctx.task_executor()
            .spawn_critical_task("flashblocks processor", async move {
                processor.spawn_nofification_handler().await;
            });

        let stream = self.event_stream(provider.clone(), move |event: &WorldChainEvent<()>| {
            if let WorldChainEvent::Chain(ChainEvent::Canon(tip)) = event {
                pending_block_tx.clone().send_if_modified(|block| {
                    let should_clear = block.as_ref().is_some_and(|b| {
                        let pending = b.recovered_block();
                        pending.hash() == tip.hash || pending.number <= tip.number
                    });
                    if should_clear {
                        *block = None;
                    }
                    should_clear
                });
            }
            None
        });

        let chain_spec = ctx.chain_spec().clone();
        let this = Arc::new(self);

        ctx.task_executor()
            .spawn_critical_task("flashblocks executor", async move {
                run_flashblock_processor(this, stream, provider, evm_config, chain_spec).await;
            });
    }

    /// Publishes a locally-built payload. Handles P2P broadcast directly,
    /// then notifies the [`FlashblockProcessor`] for state updates.
    pub fn publish_built_payload(
        &self,
        authorized_payload: AuthorizedPayload<FlashblocksPayloadV1>,
        built_payload: OpBuiltPayload,
    ) -> eyre::Result<()> {
        self.p2p_handle.publish_new(authorized_payload.clone())?;

        let block = built_payload
            .executed_block()
            .map(|b| b.into_executed_payload())
            .ok_or_else(|| eyre!("built payload missing executed block"))?;

        self.notifications_sender
            .send(PendingBlockNotification::Internal {
                block,
                authorized: authorized_payload,
            })?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Event loop
// ---------------------------------------------------------------------------

/// Core flashblock processing loop.
///
/// Consumes flashblocks from the event `stream` and routes them to per-epoch
/// [`PendingPayloadProcessor`] instances via a broadcast channel. A new processor
/// is spawned (on a blocking thread) whenever a new payload_id is detected.
/// Dropping the broadcast sender cancels the previous processor.
pub async fn run_flashblock_processor<T, S, Provider>(
    coordinator: Arc<FlashblocksExecutionCoordinator>,
    stream: S,
    provider: Provider,
    evm_config: OpEvmConfig,
    chain_spec: Arc<OpChainSpec>,
) where
    S: futures::Stream<Item = WorldChainEvent<T>> + Unpin,
    Provider: StateProviderFactory
        + HeaderProvider<Header = alloy_consensus::Header>
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + StateProvider
        + Clone
        + 'static,
{
    futures::pin_mut!(stream);

    let mut current: Option<(
        alloy_rpc_types_engine::PayloadId,
        broadcast::Sender<FlashblocksPayloadV1>,
    )> = None;
    let mut in_flight = JoinSet::new();
    let mut stream_closed = false;

    loop {
        tokio::select! {
            maybe_event = stream.next(), if !stream_closed => {
                match maybe_event {
                    Some(WorldChainEvent::Chain(ChainEvent::Pending(flashblock))) => {
                        let flashblock = Arc::try_unwrap(flashblock)
                            .unwrap_or_else(|arc| (*arc).clone());

                        trace!(
                            target: "flashblocks::coordinator",
                            payload_id = %flashblock.payload_id,
                            index = %flashblock.index,
                            is_base = flashblock.base.is_some(),
                            "received pending flashblock"
                        );

                        let is_new_epoch = current
                            .as_ref()
                            .map(|(id, _)| *id != flashblock.payload_id)
                            .unwrap_or(true);

                        if is_new_epoch {
                            let base = match flashblock.base.clone() {
                                Some(base) => base,
                                None => {
                                    error!("first flashblock of epoch must contain base payload");
                                    continue;
                                }
                            };

                            let payload_id = flashblock.payload_id;

                            // Drop old sender → old processor's stream ends.
                            current = None;

                            match spawn_processor(
                                flashblock,
                                &provider,
                                &evm_config,
                                &chain_spec,
                                coordinator.notifications_sender.clone(),
                                &mut in_flight,
                            ) {
                                Ok(tx) => {
                                    current = Some((payload_id, tx));
                                }
                                Err(e) => {
                                    error!("error spawning epoch processor: {e:#?}");
                                }
                            }
                        } else if let Some((_, ref tx)) = current {
                            if let Err(e) = tx.send(flashblock) {
                                error!("error sending flashblock to processor: {e:#?}");
                            }
                        }
                    }
                    Some(WorldChainEvent::Chain(ChainEvent::Canon(tip))) => {
                        if current.is_some() {
                            current = None;
                            trace!(
                                target: "flashblocks::coordinator",
                                tip_hash = %tip.hash,
                                tip_number = tip.number,
                                "canon tip invalidated current epoch, cancelled processor"
                            );
                        }
                    }
                    Some(_) => {}
                    None => {
                        current = None;
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

// ---------------------------------------------------------------------------
// Processor spawning
// ---------------------------------------------------------------------------

/// Derives epoch-scoped context from a base payload and spawns a
/// [`PendingPayloadProcessor`] on a blocking thread.
///
/// The processor is given a factory closure that produces a fresh
/// [`FlashblockBlockValidator`] + [`StateProvider`] per flashblock.
fn spawn_processor<Provider>(
    flashblock_base: FlashblocksPayloadV1,
    provider: &Provider,
    evm_config: &OpEvmConfig,
    chain_spec: &Arc<OpChainSpec>,
    notifications_sender: broadcast::Sender<PendingBlockNotification>,
    in_flight: &mut JoinSet<()>,
) -> eyre::Result<broadcast::Sender<FlashblocksPayloadV1>>
where
    Provider: StateProviderFactory
        + HeaderProvider<Header = alloy_consensus::Header>
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + StateProvider
        + Clone
        + 'static,
{
    let base = flashblock_base.clone().base.unwrap();
    let sealed_header = provider
        .sealed_header_by_hash(base.parent_hash)
        .inspect_err(|e| error!("failed to fetch sealed header {}: {e:#?}", base.parent_hash))?
        .ok_or_else(|| eyre!("sealed header not found for hash {}", base.parent_hash))?;

    let execution_context = OpBlockExecutionCtx {
        parent_hash: base.parent_hash,
        parent_beacon_block_root: Some(base.parent_beacon_block_root),
        extra_data: base.extra_data.clone(),
    };

    let evm_env = evm_config.next_evm_env(
        sealed_header.header(),
        &OpNextBlockEnvAttributes {
            timestamp: base.timestamp,
            suggested_fee_recipient: base.fee_recipient,
            prev_randao: base.prev_randao,
            gas_limit: base.gas_limit,
            parent_beacon_block_root: Some(base.parent_beacon_block_root),
            extra_data: base.extra_data.clone(),
        },
    )?;

    let provider = provider.clone();
    let chain_spec: Arc<OpChainSpec> = chain_spec.clone();

    let validate = move |flashblock: &FlashblocksPayloadV1,
                         pending_block: Option<&ExecutedBlock<OpPrimitives>>| {
        let validator = FlashblockBlockValidator::new(
            chain_spec.clone(),
            execution_context.clone(),
            sealed_header.clone(),
            evm_env.clone(),
        );

        validator.validate_payload_with_state(
            provider.clone(),
            &flashblock.diff.transactions,
            pending_block.cloned(),
        )
    };
    let processor = PendingPayloadProcessor::new(validate, flashblock_base);

    let (broadcast_tx, rx) = broadcast::channel(BROADCAST_CAPACITY);

    in_flight.spawn_blocking(move || {
        processor.run(rx, notifications_sender.clone());
    });

    Ok(broadcast_tx)
}
