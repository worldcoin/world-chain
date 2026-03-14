use alloy_eips::{Decodable2718, eip2718::WithEncoded, eip4895::Withdrawals};
use alloy_op_evm::OpBlockExecutionCtx;
use eyre::eyre::eyre;
use flashblocks_p2p::protocol::{
    event::{ChainEvent, WorldChainEvent, WorldChainEventsStream},
    handler::FlashblocksHandle,
};
use flashblocks_primitives::{p2p::AuthorizedPayload, primitives::FlashblocksPayloadV1};
use futures::StreamExt;
use op_alloy_consensus::{OpTxEnvelope, encode_holocene_extra_data};
use parking_lot::RwLock;
use reth::{
    payload::EthPayloadBuilderAttributes,
    revm::{cancelled::CancelOnDrop, database::StateProviderDatabase},
    rpc::types::BlockNumHash,
};
use reth_basic_payload_builder::PayloadConfig;
use reth_chain_state::{DeferredTrieData, ExecutedBlock};
use reth_engine_tree::tree::executor::WorkloadExecutor;
use reth_evm::ConfigureEvm;
use reth_node_api::{BuiltPayload as _, Events, FullNodeTypes, NodeTypes};
use reth_node_builder::BuilderContext;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpNextBlockEnvAttributes, OpRethReceiptBuilder};
use reth_optimism_node::{OpBuiltPayload, OpEngineTypes, OpEvmConfig, OpPayloadBuilderAttributes};
use reth_optimism_primitives::OpPrimitives;

use reth_payload_util::BestPayloadTransactions;
use reth_provider::{
    CanonStateSubscriptions, ChainSpecProvider, HeaderProvider, StateProviderFactory,
};
use reth_transaction_pool::{EthPooledTransaction, noop::NoopTransactionPool};
use std::sync::Arc;
use tokio::sync::{
    Semaphore, SemaphorePermit,
    broadcast::{self, Sender},
    oneshot,
};
use tracing::{debug, error, trace};

/// Placeholder for future task handle variants. Currently unused — the
/// hook updates P2P state directly via the flushed cursor.
#[derive(Clone, Debug)]
pub enum TrieTaskHandle {}

use crate::{
    bal_executor::CommittedState,
    bal_validator::{FlashblocksBlockValidator, decode_transactions_with_indices},
    metrics::EXECUTION,
    payload_builder::build,
    spawn_blocking_io_with_shutdown_signal,
    traits::{context::OpPayloadBuilderCtxBuilder, context_builder::PayloadBuilderCtxBuilder},
};
use flashblocks_primitives::flashblocks::{Flashblock, Flashblocks};

/// Semaphore locking the [`WorkloadExecutor`] thread pool for flashblock processing tasks.
/// Ensures the Pending Block is always in sync when a concurrent task is spawned.
static PENDING_BLOCK_WRITE_PERMIT: Semaphore = Semaphore::const_new(1);

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
    /// Deferred trie handles from prior flashblocks in the current epoch.
    /// Used as ancestors for the next flashblock's [`DeferredTrieData`].
    ancestor_handles: Vec<DeferredTrieData>,
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
            ancestor_handles: Vec::new(),
        }));

        Self {
            inner,
            p2p_handle,
            pending_block,
        }
    }

    /// Launches the executor to listen for new flashblocks and build payloads.
    pub fn launch<Node>(&self, ctx: &BuilderContext<Node>, evm_config: OpEvmConfig)
    where
        Node: FullNodeTypes,
        Node::Provider: StateProviderFactory
            + HeaderProvider<Header = alloy_consensus::Header>
            + CanonStateSubscriptions,
        Node::Types: NodeTypes<ChainSpec = OpChainSpec>,
    {
        let provider = ctx.provider().clone();
        let p2p_state = self.p2p_handle.state.clone();
        let mut stream: WorldChainEventsStream<TrieTaskHandle> =
            self.p2p_handle
                .event_stream(provider.clone(), move |event| {
                    if let WorldChainEvent::Chain(ChainEvent::Pending(fb)) = event {
                        let mut state = p2p_state.lock();
                        state.flushed_payload_id = Some(fb.payload_id);
                        state.flushed_index = fb.index;
                    }
                    None
                });

        let this = self.clone();
        let chain_spec = ctx.chain_spec().clone();

        let pending_block = self.pending_block.clone();

        let workload = WorkloadExecutor::default();

        let database_permit = &PENDING_BLOCK_WRITE_PERMIT;

        ctx.task_executor()
            .spawn_critical("flashblocks executor", async move {
                // Tracks the in-flight shutdown signal and current epoch block number.
                let mut inflight_shutdown: Option<oneshot::Sender<()>> = None;
                let mut epoch_block_number: Option<u64> = None;

                while let Some(event) = stream.next().await {
                    match event {
                        WorldChainEvent::Chain(ChainEvent::Pending(flashblock)) => {
                            let flashblock =
                                Arc::try_unwrap(flashblock).unwrap_or_else(|arc| (*arc).clone());

                            trace!(
                                target: "flashblocks::coordinator",
                                payload_id = %flashblock.payload_id,
                                index = %flashblock.index,
                                is_base = flashblock.base.is_some(),
                                "received pending flashblock"
                            );

                            // Track epoch block number from base flashblocks
                            if let Some(base) = &flashblock.base {
                                epoch_block_number = Some(base.block_number);
                            }

                            this.on_flashblock(
                                flashblock,
                                &mut inflight_shutdown,
                                database_permit,
                                &workload,
                                &provider,
                                &evm_config,
                                &chain_spec,
                                &pending_block,
                            )
                            .await;
                        }
                        WorldChainEvent::Chain(ChainEvent::Canon(tip)) => {
                            trace!(
                                target: "flashblocks::coordinator",
                                tip_number = tip.number,
                                tip_hash = %tip.hash,
                                "received canonical tip"
                            );

                            this.on_canon(
                                tip,
                                &mut inflight_shutdown,
                                &mut epoch_block_number,
                                &pending_block,
                            );
                        }
                        WorldChainEvent::Event(_) => {}
                    }
                }
            });
    }

    /// Handles a new pending flashblock event. Cancels any previous in-flight
    /// task, acquires a thread pool permit, and spawns processing on the
    /// [`WorkloadExecutor`].
    async fn on_flashblock<Provider>(
        &self,
        flashblock: FlashblocksPayloadV1,
        shutdown_tx: &mut Option<oneshot::Sender<()>>,
        database_permit: &'static Semaphore,
        workload: &WorkloadExecutor,
        provider: &Provider,
        evm_config: &OpEvmConfig,
        chain_spec: &Arc<OpChainSpec>,
        pending_block: &tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
    ) where
        Provider: StateProviderFactory
            + HeaderProvider<Header = alloy_consensus::Header>
            + ChainSpecProvider<ChainSpec = OpChainSpec>
            + Clone
            + 'static,
    {
        // Cancel any previous in-flight task. Ancestor handles are NOT cleared
        // here — the new flashblock is typically in the same epoch and needs them.
        // New epoch clearing is handled inside process_flashblock when is_new_payload.
        shutdown_tx.take();

        let (tx, rx) = oneshot::channel::<()>();
        *shutdown_tx = Some(tx);

        let provider = provider.clone();
        let evm_config = evm_config.clone();
        let this = self.clone();
        let chain_spec = chain_spec.clone();
        let pending_block = pending_block.clone();

        let payload_id = flashblock.payload_id;
        let index = flashblock.index;

        spawn_blocking_io_with_shutdown_signal(workload, rx, database_permit, move |permit| {
            if let Err(e) = process_flashblock(
                permit,
                provider,
                &evm_config,
                &this,
                chain_spec,
                flashblock,
                pending_block,
            ) {
                error!(
                    target: "flashblocks::coordinator",
                    %payload_id,
                    index,
                    "error processing flashblock: {e:#?}"
                );
                EXECUTION.errors.increment(1);
            }
        });
    }

    /// Handles a canonical chain tip update. Cancels any in-flight task,
    /// clears stale ancestor trie handles, and clears the pending block if
    /// it was built on the now-canonical tip.
    #[tracing::instrument(
        target = "flashblocks::coordinator",
        skip_all,
        fields(
            tip_number = tip.number,
            tip_hash = %tip.hash,
            is_stale,
        )
    )]
    fn on_canon(
        &self,
        tip: BlockNumHash,
        inflight_shutdown: &mut Option<oneshot::Sender<()>>,
        epoch_block_number: &mut Option<u64>,
        pending_block: &tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
    ) {
        // Only cancel in-flight work and clear ancestor handles if the current
        // epoch is at or behind the canonical tip (stale). If the epoch is
        // ahead of the tip, the work is still valid.
        let is_stale = epoch_block_number.is_none_or(|n| n <= tip.number);
        tracing::Span::current().record("is_stale", is_stale);

        if is_stale {
            debug!(
                target: "flashblocks::coordinator",
                epoch_block_number = *epoch_block_number,
                "stale epoch — cancelling inflight and clearing ancestors"
            );
            EXECUTION.stale_resets.increment(1);
            inflight_shutdown.take();
            self.inner.write().ancestor_handles.clear();
            *epoch_block_number = None;
        }

        // Clear pending block if it was built on the now-canonical tip.
        pending_block.send_if_modified(|block| {
            let matches = block
                .as_ref()
                .is_some_and(|b| b.recovered_block().parent_num_hash() == tip);

            if matches {
                *block = None;
            }
            matches
        });
    }

    pub fn publish_built_payload(
        &self,
        authorized_payload: AuthorizedPayload<FlashblocksPayloadV1>,
        built_payload: OpBuiltPayload,
    ) -> eyre::Result<()> {
        let flashblock = authorized_payload.msg().clone();

        let FlashblocksExecutionCoordinatorInner {
            ref mut flashblocks,
            ref mut latest_payload,
            ..
        } = *self.inner.write();

        let index = flashblock.index;
        let flashblock = Flashblock { flashblock };
        flashblocks.push(flashblock.clone())?;

        *latest_payload = Some((built_payload, index));

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
    ) -> eyre::Result<()> {
        if let Some(payload_events) = payload_events
            && let Err(e) = payload_events.send(event)
        {
            error!("error broadcasting payload: {e:#?}");
        }
        Ok(())
    }
}

fn process_flashblock<Provider>(
    database_permit: SemaphorePermit<'static>,
    provider: Provider,
    evm_config: &OpEvmConfig,
    coordinator: &FlashblocksExecutionCoordinator,
    chain_spec: Arc<OpChainSpec>,
    flashblock: FlashblocksPayloadV1,
    pending_block: tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
) -> eyre::Result<()>
where
    Provider: StateProviderFactory
        + HeaderProvider<Header = alloy_consensus::Header>
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + Clone
        + 'static,
{
    let flashblock = Flashblock { flashblock };

    // --- Short read: check if already processed, extract base info ---
    let (base, is_new_epoch) = {
        let inner = coordinator.inner.read();

        if let Some(latest_payload) = &inner.latest_payload
            && latest_payload.0.id() == flashblock.flashblock.payload_id
            && latest_payload.1 >= flashblock.flashblock.index
        {
            // Already processed — send current pending block and return
            if let Some(executed) = latest_payload.0.executed_block() {
                let block = ExecutedBlock::with_deferred_trie_data(
                    executed.recovered_block.clone(),
                    executed.execution_output.clone(),
                    DeferredTrieData::ready(Default::default()),
                );
                pending_block.send_replace(Some(block));
            }
            return Ok(());
        }

        let is_new = inner.flashblocks.is_new_payload(&flashblock)?;
        let base = if is_new {
            flashblock.base().unwrap().clone()
        } else {
            inner.flashblocks.base().clone()
        };

        (base, is_new)
    };
    // --- Read lock dropped ---

    // Clear ancestor handles on new epoch
    if is_new_epoch {
        let mut inner = coordinator.inner.write();
        inner.latest_payload = None;
        inner.ancestor_handles.clear();
    }

    // Accumulate committed state from latest payload (brief read lock)
    let committed_state = {
        let inner = coordinator.inner.read();
        CommittedState::<OpRethReceiptBuilder>::try_from(
            inner.latest_payload.as_ref().map(|(p, _)| p),
        )
        .map_err(|e| eyre!("Failed to construct committed state {:#?}", e))?
    };

    let diff = flashblock.diff().clone();
    let index = flashblock.flashblock.index;

    let sealed_header = provider
        .sealed_header_by_hash(base.parent_hash)
        .inspect_err(|e| error!("failed to fetch sealed header {}: {e:#?}", base.parent_hash))?
        .ok_or_else(|| eyre!("sealed header not found for hash {}", base.parent_hash))?;
    let anchor_hash = sealed_header.hash();

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
    let transactions_offset = committed_state.transactions.len() + 1;
    let has_bal = flashblock.diff().access_list_data.is_some();

    let _validate_span = crate::metrics::MetricsSpan::new(
        tracing::trace_span!(
            target: "flashblocks::coordinator",
            "validate",
            id = %flashblock.flashblock().payload_id,
            index,
            path = if has_bal { "bal" } else { "legacy" },
            tx_count = flashblock.diff().transactions.len(),
            duration_ms = tracing::field::Empty,
        ),
        EXECUTION.validate_duration.clone(),
    );

    let payload = if has_bal {
        let sealed_header = Arc::new(sealed_header);

        let executor_transactions = decode_transactions_with_indices(
            &flashblock.diff().transactions,
            transactions_offset as u16,
        )?;

        let block_validator = FlashblocksBlockValidator {
            chain_spec: chain_spec.clone(),
            evm_config: evm_config.clone(),
            execution_context: execution_context.clone(),
            executor_transactions: executor_transactions.clone(),
            committed_state,
            evm_env: evm_env.clone(),
        };

        block_validator.validate(
            provider,
            diff.clone(),
            &sealed_header,
            *flashblock.payload_id(),
        )?
    } else {
        // TODO: Lots of dup code we can remove here. WIP
        let transactions = diff
            .transactions
            .iter()
            .map(|b| {
                let tx: OpTxEnvelope = Decodable2718::decode_2718_exact(b)?;
                eyre::Result::Ok(WithEncoded::new(b.clone(), tx))
            })
            .collect::<eyre::Result<Vec<_>>>()?;

        let eth_attrs = EthPayloadBuilderAttributes {
            id: flashblock.payload_id().to_owned(),
            parent: base.parent_hash,
            timestamp: base.timestamp,
            suggested_fee_recipient: base.fee_recipient,
            prev_randao: base.prev_randao,
            withdrawals: Withdrawals(diff.withdrawals.clone()),
            parent_beacon_block_root: Some(base.parent_beacon_block_root),
        };

        let eip1559 = encode_holocene_extra_data(
            Default::default(),
            chain_spec.base_fee_params_at_timestamp(base.timestamp),
        )?;

        let attributes = OpPayloadBuilderAttributes {
            payload_attributes: eth_attrs,
            no_tx_pool: true,
            transactions: transactions.clone(),
            gas_limit: None,
            eip_1559_params: Some(eip1559[1..=8].try_into()?),
            min_base_fee: None,
        };

        let state_provider = provider.state_by_block_hash(sealed_header.hash())?;
        let config = PayloadConfig::new(Arc::new(sealed_header), attributes);
        let cancel = CancelOnDrop::default();

        let prev_payload = coordinator
            .inner
            .read()
            .latest_payload
            .as_ref()
            .map(|(p, _)| p.clone());

        let builder_ctx = OpPayloadBuilderCtxBuilder.build(
            provider.clone(),
            evm_config.clone(),
            Default::default(),
            config,
            &cancel,
            prev_payload.clone(),
        );

        let best = |_| BestPayloadTransactions::new(vec![].into_iter());
        let db = StateProviderDatabase::new(state_provider);

        let outcome = build(
            provider,
            best,
            Option::<NoopTransactionPool<EthPooledTransaction>>::None,
            db,
            &builder_ctx,
            prev_payload.as_ref(),
            false,
        )?;

        match outcome.0 {
            reth_basic_payload_builder::BuildOutcomeKind::Better { payload } => payload,
            reth_basic_payload_builder::BuildOutcomeKind::Freeze(payload) => payload,
            _ => return Err(eyre::eyre::eyre!("unexpected build outcome")),
        }
    };

    // _validate_span dropped here — records duration_ms on span + histogram.
    drop(_validate_span);

    // Build ExecutedBlock with deferred trie data — sorting happens in background
    let deferred = if let Some(executed) = payload.executed_block() {
        let (hashed_state, trie_updates) = match (&executed.hashed_state, &executed.trie_updates) {
            (either::Left(hs), either::Left(tu)) => (hs.clone(), tu.clone()),
            _ => unreachable!("payload builder always produces unsorted (Left) variants"),
        };

        let ancestors = coordinator.inner.read().ancestor_handles.clone();

        let deferred =
            DeferredTrieData::pending(hashed_state, trie_updates, anchor_hash, ancestors);

        let block = ExecutedBlock::with_deferred_trie_data(
            executed.recovered_block.clone(),
            executed.execution_output.clone(),
            deferred.clone(),
        );

        pending_block.send_replace(Some(block));

        Some(deferred)
    } else {
        None
    };

    // --- Brief write lock: update state, then release database permit ---
    {
        let mut inner = coordinator.inner.write();
        inner.latest_payload = Some((payload.clone(), index));
        inner.flashblocks.push(flashblock)?;
        if let Some(ref deferred) = deferred {
            inner.ancestor_handles.push(deferred.clone());
        }
    }
    // Release database permit immediately after state update.
    // Everything after this point (trie sort, broadcast) can run concurrently.
    drop(database_permit);

    // Spawn background trie sort after releasing the permit.
    // Link the rayon span back to the processing span for trace correlation.
    if let Some(deferred) = deferred {
        let trie_span = tracing::trace_span!(
            target: "flashblocks::coordinator",
            "trie_sort",
            id = %payload.id(),
            index,
        );
        trie_span.follows_from(tracing::Span::current());

        rayon::spawn(move || {
            let _enter = trie_span.enter();
            deferred.wait_cloned();
        });
    }

    trace!(
        target: "flashblocks::state_executor",
        id = %payload.id(),
        index = %index,
        block_hash = %payload.block().hash(),
        "built payload from flashblock"
    );

    let payload_events = coordinator.inner.read().payload_events.clone();

    coordinator.broadcast_payload(Events::BuiltPayload(payload), payload_events)?;

    Ok(())
}
