use alloy_eips::{Decodable2718, eip2718::WithEncoded, eip4895::Withdrawals};
use alloy_op_evm::OpBlockExecutionCtx;
use eyre::eyre::eyre;
use flashblocks_p2p::protocol::{
    event::{ChainEvent, WorldChainEvent, WorldChainEventsStream},
    handler::FlashblocksHandle,
};
use flashblocks_primitives::{p2p::AuthorizedPayload, primitives::FlashblocksPayloadV1};
use futures::StreamExt as _;
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
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, broadcast::{self, Sender}, oneshot};
use tracing::{error, trace, warn};

/// Maximum number of concurrent flashblock processing tasks on the thread pool.
const MAX_THREAD_POOL_SIZE: usize = 4;

/// Task handle for deferred trie computation. Intentionally empty for now —
/// cancellation flows through `oneshot::Sender` drop, not explicit events.
#[derive(Clone, Debug)]
pub enum TrieTaskHandle {}

use crate::{
    bal_executor::CommittedState,
    bal_validator::{FlashblocksBlockValidator, decode_transactions_with_indices},
    payload_builder::build,
    traits::{context::OpPayloadBuilderCtxBuilder, context_builder::PayloadBuilderCtxBuilder},
};
use backon::BlockingRetryable;
use flashblocks_primitives::flashblocks::{Flashblock, Flashblocks};

/// The maximum backoff duration when waiting for the parent header to be available in the database when processing a flashblock.
const FETCH_PARENT_HEADER_MAX_DELAY: Duration = Duration::from_millis(2000);

/// The minimum backoff duration when waiting for the parent header to be available in the database when processing a flashblock.
const FETCH_PARENT_HEADER_MIN_DELAY: Duration = Duration::from_millis(100);
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
        let mut stream: WorldChainEventsStream<TrieTaskHandle> =
            self.p2p_handle.event_stream(provider.clone());

        let this = self.clone();
        let chain_spec = ctx.chain_spec().clone();

        let pending_block = self.pending_block.clone();

        let workload = WorkloadExecutor::default();
        let semaphore = Arc::new(Semaphore::new(MAX_THREAD_POOL_SIZE));

        ctx.task_executor()
            .spawn_critical("flashblocks executor", async move {
                let mut shutdown_tx: Option<oneshot::Sender<()>> = None;

                while let Some(event) = stream.next().await {
                    match event {
                        WorldChainEvent::Chain(ChainEvent::Pending(flashblock)) => {
                            this.on_flashblock(
                                flashblock,
                                &mut shutdown_tx,
                                &semaphore,
                                &workload,
                                &provider,
                                &evm_config,
                                &chain_spec,
                                &pending_block,
                            )
                            .await;
                        }
                        WorldChainEvent::Chain(ChainEvent::Canon(tip)) => {
                            this.on_canon(tip, &mut shutdown_tx, &pending_block);
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
        semaphore: &Arc<Semaphore>,
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
        // Cancel any previous in-flight task
        shutdown_tx.take();

        let (tx, rx) = oneshot::channel::<()>();
        *shutdown_tx = Some(tx);

        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore closed");

        let provider = provider.clone();
        let evm_config = evm_config.clone();
        let this = self.clone();
        let chain_spec = chain_spec.clone();
        let pending_block = pending_block.clone();

        spawn_blocking_with_shutdown_signal(workload, rx, move || {
            let result = process_flashblock(
                provider,
                &evm_config,
                &this,
                chain_spec,
                flashblock,
                pending_block,
            );
            drop(permit);
            if let Err(e) = &result {
                error!("error processing flashblock: {e:#?}");
            }
            result
        });
    }

    /// Handles a canonical chain tip update. Cancels any in-flight task,
    /// clears stale ancestor trie handles, and clears the pending block if
    /// it was built on the now-canonical tip.
    fn on_canon(
        &self,
        tip: BlockNumHash,
        shutdown_tx: &mut Option<oneshot::Sender<()>>,
        pending_block: &tokio::sync::watch::Sender<Option<ExecutedBlock<OpPrimitives>>>,
    ) {
        // Drop the shutdown sender — cancels in-flight task
        shutdown_tx.take();

        // Clear ancestor handles if the epoch matches the canonical tip or is stale.
        {
            let mut inner = self.inner.write();
            let should_clear = inner.latest_payload.as_ref().is_some_and(|(p, _)| {
                p.block().hash() == tip.hash || p.block().header().number < tip.number
            });
            if should_clear {
                inner.ancestor_handles.clear();
            }
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

/// Spawns a blocking task on the [`WorkloadExecutor`] thread pool, racing it
/// against a shutdown signal. If the shutdown receiver resolves first (sender
/// dropped), the task result is discarded.
fn spawn_blocking_with_shutdown_signal<F, R>(
    executor: &WorkloadExecutor,
    shutdown_rx: oneshot::Receiver<()>,
    f: F,
) where
    F: FnOnce() -> R + Send + 'static,
    R: std::fmt::Debug + Send + 'static,
{
    let task = executor.spawn_blocking(f);

    tokio::spawn(async move {
        match futures::future::select(task, shutdown_rx).await {
            futures::future::Either::Left((result, _)) => {
                if let Err(e) = result {
                    error!("flashblock thread pool task panicked: {e:#?}");
                }
            }
            futures::future::Either::Right(_) => {
                trace!("flashblock processing cancelled by shutdown signal");
            }
        }
    });
}

fn process_flashblock<Provider>(
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
    let FlashblocksExecutionCoordinatorInner {
        ref mut flashblocks,
        ref mut latest_payload,
        ref mut payload_events,
        ..
    } = *coordinator.inner.write();

    let flashblock = Flashblock { flashblock };

    if let Some(latest_payload) = latest_payload
        && latest_payload.0.id() == flashblock.flashblock.payload_id
        && latest_payload.1 >= flashblock.flashblock.index
    {
        // Already processed this flashblock. This happens when set directly
        // from publish_built_payload. Since we already built the payload, no need
        // to do it again.
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

    let diff = flashblock.diff().clone();
    let index = flashblock.flashblock.index;

    // If for whatever reason we are not processing flashblocks in order
    // we will error and return here.
    let base = if flashblocks.is_new_payload(&flashblock)? {
        *latest_payload = None;
        coordinator.inner.write().ancestor_handles.clear();
        // safe unwrap from check in is_new_payload
        flashblock.base().unwrap()
    } else {
        flashblocks.base()
    };

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

    trace!(
        target: "flashblocks::coordinator",
        id = %flashblock.flashblock().payload_id,
        index = %flashblock.flashblock().index,
        min_tx_index = %flashblock.flashblock().diff.access_list_data.as_ref().map_or("None".to_string(), |d| d.access_list.min_tx_index.to_string()),
        max_tx_index = %flashblock.flashblock().diff.access_list_data.as_ref().map_or("None".to_string(), |d| d.access_list.max_tx_index.to_string()),
        execution_context = ?execution_context,
        next_block_context = ?next_block_context,
        "processing flashblock"
    );

    let evm_env = evm_config.next_evm_env(sealed_header.header(), &next_block_context)?;

    let committed_state =
        CommittedState::<OpRethReceiptBuilder>::try_from(latest_payload.as_ref().map(|(p, _)| p))
            .map_err(|e| eyre!("Failed to construct committed state {:#?}", e))?;

    let transactions_offset = committed_state.transactions.len() + 1;
    let start = Instant::now();

    let payload = if flashblock.diff().access_list_data.is_some() {
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

        let builder_ctx = OpPayloadBuilderCtxBuilder.build(
            provider.clone(),
            evm_config.clone(),
            Default::default(),
            config,
            &cancel,
            latest_payload.as_ref().map(|p| p.0.clone()),
        );

        let best = |_| BestPayloadTransactions::new(vec![].into_iter());
        let db = StateProviderDatabase::new(state_provider);

        let outcome = build(
            provider,
            best,
            Option::<NoopTransactionPool<EthPooledTransaction>>::None,
            db,
            &builder_ctx,
            latest_payload.as_ref().map(|p| &p.0),
            false,
        )?;

        match outcome.0 {
            reth_basic_payload_builder::BuildOutcomeKind::Better { payload } => payload,
            reth_basic_payload_builder::BuildOutcomeKind::Freeze(payload) => payload,
            _ => return Err(eyre::eyre::eyre!("unexpected build outcome")),
        }
    };

    let duration = Instant::now().duration_since(start);
    metrics::histogram!("flashblocks.validate", "access_list" => flashblock.diff().access_list_data.is_some().to_string())
        .record(duration.as_nanos() as f64 / 1_000_000_000.0);

    // construct the full payload
    *latest_payload = Some((payload.clone(), index));

    flashblocks.push(flashblock)?;

    // Build ExecutedBlock with deferred trie data — sorting happens in background
    if let Some(executed) = payload.executed_block() {
        let (hashed_state, trie_updates) = match (&executed.hashed_state, &executed.trie_updates) {
            (either::Left(hs), either::Left(tu)) => (hs.clone(), tu.clone()),
            _ => unreachable!("payload builder always produces unsorted (Left) variants"),
        };

        let ancestors = coordinator.inner.read().ancestor_handles.clone();

        let deferred = DeferredTrieData::pending(
            hashed_state,
            trie_updates,
            anchor_hash,
            ancestors,
        );

        // Spawn background sort so wait_cloned() finds it ready
        let deferred_clone = deferred.clone();
        rayon::spawn(move || {
            deferred_clone.wait_cloned();
        });

        let block = ExecutedBlock::with_deferred_trie_data(
            executed.recovered_block.clone(),
            executed.execution_output.clone(),
            deferred.clone(),
        );

        pending_block.send_replace(Some(block));

        // Track this handle for future ancestors
        coordinator.inner.write().ancestor_handles.push(deferred);
    }

    trace!(
        target: "flashblocks::state_executor",
        id = %payload.id(),
        index = %index,
        block_hash = %payload.block().hash(),
        "built payload from flashblock"
    );

    coordinator.broadcast_payload(Events::BuiltPayload(payload), payload_events.clone())?;

    Ok(())
}
