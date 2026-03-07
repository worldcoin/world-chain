use alloy_eips::{Decodable2718, eip2718::WithEncoded, eip4895::Withdrawals};
use alloy_op_evm::OpBlockExecutionCtx;
use eyre::eyre::eyre;
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use flashblocks_primitives::{p2p::AuthorizedPayload, primitives::FlashblocksPayloadV1};
use futures::StreamExt as _;
use op_alloy_consensus::{OpTxEnvelope, encode_holocene_extra_data};
use parking_lot::RwLock;
use reth::{
    payload::EthPayloadBuilderAttributes,
    revm::{cancelled::CancelOnDrop, database::StateProviderDatabase},
};
use reth_basic_payload_builder::PayloadConfig;
use reth_evm::ConfigureEvm;
use reth_node_api::{BuiltPayload as _, Events, FullNodeTypes, NodeTypes};
use reth_node_builder::BuilderContext;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpNextBlockEnvAttributes, OpRethReceiptBuilder};
use reth_optimism_node::{OpBuiltPayload, OpEngineTypes, OpEvmConfig, OpPayloadBuilderAttributes};

use reth_payload_util::BestPayloadTransactions;
use reth_provider::{ChainSpecProvider, HeaderProvider, StateProviderFactory};
use reth_transaction_pool::{EthPooledTransaction, noop::NoopTransactionPool};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::broadcast;
use tracing::{error, trace, warn};

use crate::{
    bal_executor::CommittedState,
    bal_validator::{FlashblocksBlockValidator, decode_transactions_with_indices},
    event_stream::{ExecutedFlashblock, FlashblockEvent},
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
}

#[derive(Debug, Clone)]
pub struct FlashblocksExecutionCoordinatorInner {
    /// List of flashblocks for the current payload
    flashblocks: Flashblocks,
    /// The latest built payload with its associated flashblock index
    latest_payload: Option<(OpBuiltPayload, u64)>,
    /// Broadcast channel for built payload events
    payload_events: Option<broadcast::Sender<Events<OpEngineTypes>>>,
    /// Broadcast channel for executed flashblock events consumed by the
    /// [`FlashblocksEventStream`](crate::event_stream::FlashblocksEventStream).
    flashblock_events: Option<broadcast::Sender<FlashblockEvent<reth_optimism_node::OpNode>>>,
}

impl FlashblocksExecutionCoordinator {
    /// Creates a new instance of [`FlashblocksExecutionCoordinator`].
    pub fn new(p2p_handle: FlashblocksHandle) -> Self {
        let inner = Arc::new(RwLock::new(FlashblocksExecutionCoordinatorInner {
            flashblocks: Default::default(),
            latest_payload: None,
            payload_events: None,
            flashblock_events: None,
        }));

        Self { inner, p2p_handle }
    }

    /// Launches the executor to listen for new flashblocks and build payloads.
    pub fn launch<Node>(&self, ctx: &BuilderContext<Node>, evm_config: OpEvmConfig)
    where
        Node: FullNodeTypes,
        Node::Provider: StateProviderFactory + HeaderProvider<Header = alloy_consensus::Header>,
        Node::Types: NodeTypes<ChainSpec = OpChainSpec>,
    {
        let mut stream = self.p2p_handle.flashblock_stream();
        let this = self.clone();
        let provider = ctx.provider().clone();
        let chain_spec = ctx.chain_spec().clone();

        ctx.task_executor()
            .spawn_critical("flashblocks executor", async move {
                while let Some(flashblock) = stream.next().await {
                    let provider = provider.clone();
                    if let Err(e) =
                        process_flashblock(provider, &evm_config, &this, &chain_spec, flashblock)
                    {
                        error!("error processing flashblock: {e:#?}")
                    }
                }
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

    /// Registers a new broadcast channel for built payloads.
    pub fn register_payload_events(&self, tx: broadcast::Sender<Events<OpEngineTypes>>) {
        self.inner.write().payload_events = Some(tx);
    }

    /// Registers a broadcast channel for executed flashblock events.
    ///
    /// The [`FlashblocksEventStream`](crate::event_stream::FlashblocksEventStream)
    /// subscribes to this channel to receive executed flashblocks.
    pub fn register_flashblock_events(
        &self,
        tx: broadcast::Sender<FlashblockEvent<reth_optimism_node::OpNode>>,
    ) {
        self.inner.write().flashblock_events = Some(tx);
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

/// Execute a flashblock payload against the current state, returning the
/// built payload and its flashblock index.
///
/// This is a pure function -- it does not mutate any shared state beyond the
/// `flashblocks` and `latest_payload` arguments. The caller is responsible
/// for publishing or broadcasting the result.
pub fn execute_flashblock<Provider>(
    provider: Provider,
    evm_config: &OpEvmConfig,
    chain_spec: &Arc<OpChainSpec>,
    flashblocks: &mut Flashblocks,
    latest_payload: &mut Option<(OpBuiltPayload, u64)>,
    flashblock: FlashblocksPayloadV1,
) -> eyre::Result<(OpBuiltPayload, u64)>
where
    Provider: StateProviderFactory
        + HeaderProvider<Header = alloy_consensus::Header>
        + ChainSpecProvider<ChainSpec = OpChainSpec>
        + Clone
        + 'static,
{
    let flashblock = Flashblock { flashblock };

    if let Some(latest_payload) = latest_payload.as_ref()
        && latest_payload.0.id() == flashblock.flashblock.payload_id
        && latest_payload.1 >= flashblock.flashblock.index
    {
        // Already processed this flashblock. Return the cached payload.
        return Ok(latest_payload.clone());
    }

    let index = flashblock.flashblock.index;
    // Cache the branch predicate before we potentially move the flashblock.
    let has_access_list = flashblock.diff().access_list_data.is_some();
    let payload_id_copy = *flashblock.payload_id();

    // If for whatever reason we are not processing flashblocks in order
    // we will error and return here.
    let base = if flashblocks.is_new_payload(&flashblock)? {
        *latest_payload = None;
        // safe unwrap from check in is_new_payload
        flashblock.base().unwrap()
    } else {
        flashblocks.base()
    };

    // Copy all base fields we need into owned locals so we can release the
    // borrow on `flashblock` before moving it into the collection. All of
    // these types are `Copy` (B256, Address, u64) except `extra_data` which
    // is cloned once here instead of twice (execution_context + next_block_context).
    let base_parent_hash = base.parent_hash;
    let base_parent_beacon_block_root = base.parent_beacon_block_root;
    let base_timestamp = base.timestamp;
    let base_fee_recipient = base.fee_recipient;
    let base_prev_randao = base.prev_randao;
    let base_gas_limit = base.gas_limit;
    let base_extra_data = base.extra_data.clone();

    let f = || {
        provider
            .sealed_header_by_hash(base_parent_hash)?
            .ok_or(eyre!("failed to fetch sealed header {}", base_parent_hash))
    };

    let sealed_header = f
        .retry(
            backon::ExponentialBuilder::default()
                .with_min_delay(FETCH_PARENT_HEADER_MIN_DELAY)
                .with_max_delay(FETCH_PARENT_HEADER_MAX_DELAY)
                .with_max_times(10),
        )
        .notify(|e, duration| {
            warn!(
                "waiting for parent header {}: {e:#?}. waited {:#?} so far",
                base_parent_hash, duration
            )
        })
        .call()
        .inspect_err(|e| {
            error!(
                flashblock_index = index,
                parent_hash = %base_parent_hash,
                error = %e,
                "failed to fetch parent header after multiple attempts"
            )
        })?;

    let execution_context = OpBlockExecutionCtx {
        parent_hash: base_parent_hash,
        parent_beacon_block_root: Some(base_parent_beacon_block_root),
        extra_data: base_extra_data.clone(),
    };

    let next_block_context = OpNextBlockEnvAttributes {
        timestamp: base_timestamp,
        suggested_fee_recipient: base_fee_recipient,
        prev_randao: base_prev_randao,
        gas_limit: base_gas_limit,
        parent_beacon_block_root: Some(base_parent_beacon_block_root),
        extra_data: base_extra_data,
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

    let payload = if has_access_list {
        // BAL path -- clone the diff for the validator (untouched by this optimization).
        let diff = flashblock.diff().clone();
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

        let payload = block_validator.validate(
            provider,
            diff.clone(),
            &sealed_header,
            *flashblock.payload_id(),
        )?;

        // Push the flashblock into the collection (BAL path keeps ownership).
        flashblocks.push(flashblock)?;

        payload
    } else {
        // Simple path -- push the flashblock first, then borrow the stored diff
        // by reference to decode transactions. This avoids cloning the entire
        // `ExecutionPayloadFlashblockDeltaV1` (which contains `Vec<Bytes>`
        // transactions and `Vec<Withdrawal>` withdrawals).
        flashblocks.push(flashblock)?;

        // Borrow the diff from the just-pushed flashblock. The reference is
        // valid for the duration of this scope.
        let last_diff = flashblocks.last().diff();

        let transactions = last_diff
            .transactions
            .iter()
            .map(|b| {
                let tx: OpTxEnvelope = Decodable2718::decode_2718_exact(b)?;
                eyre::Result::Ok(WithEncoded::new(b.clone(), tx))
            })
            .collect::<eyre::Result<Vec<_>>>()?;

        let eth_attrs = EthPayloadBuilderAttributes {
            id: payload_id_copy,
            parent: base_parent_hash,
            timestamp: base_timestamp,
            suggested_fee_recipient: base_fee_recipient,
            prev_randao: base_prev_randao,
            withdrawals: Withdrawals(last_diff.withdrawals.clone()),
            parent_beacon_block_root: Some(base_parent_beacon_block_root),
        };

        let eip1559 = encode_holocene_extra_data(
            Default::default(),
            chain_spec.base_fee_params_at_timestamp(base_timestamp),
        )?;

        let attributes = OpPayloadBuilderAttributes {
            payload_attributes: eth_attrs,
            no_tx_pool: true,
            // Move the decoded transactions directly -- no clone needed.
            transactions,
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
    metrics::histogram!("flashblocks.validate", "access_list" => has_access_list.to_string())
        .record(duration.as_nanos() as f64 / 1_000_000_000.0);

    *latest_payload = Some((payload.clone(), index));

    Ok((payload, index))
}

fn process_flashblock<Provider>(
    provider: Provider,
    evm_config: &OpEvmConfig,
    coordinator: &FlashblocksExecutionCoordinator,
    chain_spec: &Arc<OpChainSpec>,
    flashblock: FlashblocksPayloadV1,
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
        ref mut flashblock_events,
    } = *coordinator.inner.write();

    let (payload, index) = execute_flashblock(
        provider,
        evm_config,
        chain_spec,
        flashblocks,
        latest_payload,
        flashblock,
    )?;

    let payload_id = payload.id();
    let block_hash = payload.block().hash();
    let executed_block = payload.executed_block();

    // Broadcast the executed flashblock to the event stream if a channel is
    // registered and we have an executed block.
    if let Some(fb_tx) = flashblock_events.as_ref()
        && let Some(executed) = executed_block
    {
        let event = FlashblockEvent::ExecutedFlashblock(ExecutedFlashblock {
            block: executed.into_executed_payload(),
            index,
        });
        if let Err(e) = fb_tx.send(event) {
            error!("error broadcasting flashblock event: {e:#?}");
        }
    }

    trace!(
        target: "flashblocks::state_executor",
        id = %payload_id,
        index = %index,
        block_hash = %block_hash,
        "built payload from flashblock"
    );

    coordinator.broadcast_payload(Events::BuiltPayload(payload), payload_events.clone())?;

    Ok(())
}
