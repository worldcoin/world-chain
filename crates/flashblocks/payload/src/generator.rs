use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alloy_primitives::B256;
use eyre::eyre::eyre;
use flashblocks_p2p::protocol::handler::FlashblocksHandle;
use op_alloy_consensus::OpTxEnvelope;
use reth::{
    api::{PayloadBuilderAttributes, PayloadBuilderError},
    payload::{PayloadJob, PayloadJobGenerator},
    revm::cached::CachedReads,
    tasks::TaskSpawner,
};
use reth_basic_payload_builder::{
    HeaderForPayload, PayloadBuilder, PayloadConfig, PayloadState, PayloadTaskGuard, PrecachedState,
};

use flashblocks_primitives::p2p::Authorization;
use reth_optimism_node::{OpBuiltPayload, OpPayloadBuilderAttributes};
use reth_optimism_primitives::OpPrimitives;
use reth_primitives::{Block, NodePrimitives, RecoveredBlock};
use reth_provider::{BlockReaderIdExt, CanonStateNotification, StateProviderFactory};
use tokio::runtime::Handle;
use tracing::debug;

use crate::job::FlashblocksPayloadJob;
use crate::metrics::PayloadBuilderMetrics;
use flashblocks_builder::{
    executor::FlashblocksStateExecutor, traits::payload_builder::FlashblockPayloadBuilder,
};
use flashblocks_primitives::flashblocks::Flashblock;

/// A type that initiates payload building jobs on the [`crate::builder::FlashblocksPayloadBuilder`].
pub struct FlashblocksPayloadJobGenerator<Client, Tasks, Builder> {
    /// The client that can interact with the chain.
    client: Client,
    /// The task executor to spawn payload building tasks on.
    executor: Tasks,
    /// The configuration for the job generator.
    config: FlashblocksJobGeneratorConfig,
    /// The type responsible for building payloads.
    ///
    /// See [`PayloadBuilder`]
    builder: Builder,
    /// Stored `cached_reads` for new payload jobs.
    pre_cached: Option<PrecachedState>,
    /// The cached authorizations for payload ids.
    authorizations: tokio::sync::watch::Receiver<Option<Authorization>>,
    /// The P2P handler for flashblocks.
    p2p_handler: FlashblocksHandle,
    /// The current flashblocks state
    flashblocks_state: FlashblocksStateExecutor,
    /// Metrics for tracking job generator operations and errors
    metrics: PayloadBuilderMetrics,
}

impl<Client, Tasks: TaskSpawner, Builder> FlashblocksPayloadJobGenerator<Client, Tasks, Builder> {
    /// Creates a new [`WorldChainPayloadJobGenerator`] with the given config and custom
    /// [`PayloadBuilder`]
    #[allow(clippy::too_many_arguments)]
    pub fn with_builder(
        client: Client,
        executor: Tasks,
        config: FlashblocksJobGeneratorConfig,
        builder: Builder,
        p2p_handler: FlashblocksHandle,
        auth_rx: tokio::sync::watch::Receiver<Option<Authorization>>,
        flashblocks_state: FlashblocksStateExecutor,
        metrics: PayloadBuilderMetrics,
    ) -> Self {
        Self {
            client,
            executor,
            config,
            builder,
            flashblocks_state,
            pre_cached: None,
            p2p_handler,
            authorizations: auth_rx,
            metrics,
        }
    }

    /// Returns the maximum duration a job should be allowed to run.
    ///
    /// This adheres to the following specification:
    /// > Client software SHOULD stop the updating process when either a call to engine_getPayload
    /// > with the build process's payloadId is made or SECONDS_PER_SLOT (12s in the Mainnet
    /// > configuration) have passed since the point in time identified by the timestamp parameter.
    ///
    /// See also <https://github.com/ethereum/execution-apis/blob/431cf72fd3403d946ca3e3afc36b973fc87e0e89/src/engine/paris.md?plain=1#L137>
    #[inline]
    fn max_job_duration(&self, unix_timestamp: u64) -> Duration {
        let duration_until_timestamp = duration_until(unix_timestamp);

        // safety in case clocks are bad
        let duration_until_timestamp = duration_until_timestamp.min(self.config.deadline * 3);

        self.config.deadline + duration_until_timestamp
    }

    /// Returns the [Instant](tokio::time::Instant) at which the job should be terminated because it
    /// is considered timed out.
    #[inline]
    fn job_deadline(&self, unix_timestamp: u64) -> tokio::time::Instant {
        tokio::time::Instant::now() + self.max_job_duration(unix_timestamp)
    }

    /// Returns a reference to the tasks type
    pub const fn tasks(&self) -> &Tasks {
        &self.executor
    }

    /// Returns the pre-cached reads for the given parent header if it matches the cached state's
    /// block.
    fn maybe_pre_cached(&self, parent: B256) -> Option<CachedReads> {
        self.pre_cached
            .as_ref()
            .filter(|pc| pc.block == parent)
            .map(|pc| pc.cached.clone())
    }
}

impl<Client, Tasks, Builder> PayloadJobGenerator
    for FlashblocksPayloadJobGenerator<Client, Tasks, Builder>
where
    Client: StateProviderFactory
        + BlockReaderIdExt<Header = HeaderForPayload<Builder::BuiltPayload>>
        + Clone
        + Unpin
        + 'static,
    Tasks: TaskSpawner + Clone + Unpin + 'static,
    Builder: PayloadBuilder<
            BuiltPayload = OpBuiltPayload<OpPrimitives>,
            Attributes = OpPayloadBuilderAttributes<OpTxEnvelope>,
        > + FlashblockPayloadBuilder
        + Unpin
        + Clone
        + 'static,
    Builder::Attributes: Unpin + Clone,
    Builder::BuiltPayload: Unpin + Clone,
{
    type Job = FlashblocksPayloadJob<Tasks, Builder>;

    fn new_payload_job(
        &self,
        attributes: <Self::Job as PayloadJob>::PayloadAttributes,
    ) -> Result<Self::Job, PayloadBuilderError> {
        let parent_header = if attributes.parent().is_zero() {
            // Use latest header for genesis block case
            self.client
                .latest_header()
                .map_err(|e| {
                    self.metrics.inc_job_creation_errors();
                    PayloadBuilderError::from(e)
                })?
                .ok_or_else(|| {
                    self.metrics.inc_job_creation_errors();
                    PayloadBuilderError::MissingParentHeader(B256::ZERO)
                })?
        } else {
            // Fetch specific header by hash
            self.client
                .sealed_header_by_hash(attributes.parent())
                .map_err(|e| {
                    self.metrics.inc_job_creation_errors();
                    PayloadBuilderError::from(e)
                })?
                .ok_or_else(|| {
                    self.metrics.inc_job_creation_errors();
                    PayloadBuilderError::MissingParentHeader(attributes.parent())
                })?
        };

        let config = PayloadConfig::new(Arc::new(parent_header.clone()), attributes);

        let until = self.job_deadline(config.attributes.timestamp());
        let deadline = Box::pin(tokio::time::sleep_until(until));
        let flashblock_deadline = Box::pin(tokio::time::sleep(self.config.interval));
        let recommit_interval = tokio::time::interval(self.config.recommitment_interval);

        let cached_reads = self.maybe_pre_cached(parent_header.hash());

        let payload_task_guard = PayloadTaskGuard::new(self.config.max_payload_tasks);

        let maybe_pre_state = self
            .check_for_pre_state(&config.attributes)
            .inspect_err(|_| {
                self.metrics.inc_job_creation_errors();
            })?;

        let payload_id = config.attributes.payload_id();
        let mut authorization = self.authorizations.clone();
        let pending = async move {
            let _ = authorization
                .wait_for(|a| a.is_some_and(|auth| auth.payload_id == payload_id))
                .await
                .is_ok();

            authorization.borrow().unwrap()
        };

        let authorization = tokio::task::block_in_place(|| {
            let handle = Handle::current();
            handle.block_on(pending)
        });

        // Notify the P2P handler to start publishing for this authorization
        self.p2p_handler
            .start_publishing(authorization)
            .map_err(PayloadBuilderError::other)?;

        // Extract pre-built payload from the p2p handler and the latest flashblock index if available
        let (pre_state, index) = maybe_pre_state
            .map(|(pre_state, index)| (Some(pre_state), index))
            .unwrap_or((None, 0));

        let mut job = FlashblocksPayloadJob {
            config,
            executor: self.executor.clone(),
            deadline,
            committed_payload: None,
            flashblock_interval: self.config.interval,
            flashblock_deadline,
            recommit_interval,
            best_payload: pre_state
                .map(|p| PayloadState::Frozen(p))
                .unwrap_or(PayloadState::Missing),
            pending_block: None,
            cached_reads,
            payload_task_guard,
            metrics: self.metrics.clone(),
            builder: self.builder.clone(),
            authorization,
            p2p_handler: self.p2p_handler.clone(),
            flashblocks_state: self.flashblocks_state.clone(),
            block_index: index,
        };

        // start the first job right away
        job.spawn_build_job();

        Ok(job)
    }

    fn on_new_state<N: NodePrimitives>(&mut self, new_state: CanonStateNotification<N>) {
        let mut cached = CachedReads::default();

        // extract the state from the notification and put it into the cache
        let committed = new_state.committed();
        let new_execution_outcome = committed.execution_outcome();
        for (addr, acc) in new_execution_outcome.bundle_accounts_iter() {
            if let Some(info) = acc.info.clone() {
                // we want pre cache existing accounts and their storage
                // this only includes changed accounts and storage but is better than nothing
                let storage = acc
                    .storage
                    .iter()
                    .map(|(key, slot)| (*key, slot.present_value))
                    .collect();
                cached.insert_account(addr, info, storage);
            }
        }

        self.pre_cached = Some(PrecachedState {
            block: committed.tip().hash(),
            cached,
        });
    }
}

impl<Builder, Client, Tasks> FlashblocksPayloadJobGenerator<Client, Tasks, Builder>
where
    Builder: PayloadBuilder<BuiltPayload = OpBuiltPayload>,
{
    fn check_for_pre_state(
        &self,
        attributes: &<Builder as PayloadBuilder>::Attributes,
    ) -> Result<Option<(Builder::BuiltPayload, u64)>, PayloadBuilderError> {
        // check for any pending pre state received over p2p
        let flashblocks = self.flashblocks_state.flashblocks();

        let block = Flashblock::reduce(flashblocks);
        if let Some(flashblock) = block {
            if *flashblock.payload_id() == attributes.payload_id().0 {
                // If we have a pre-confirmed state, we can use it to build the payload
                debug!(target: "flashblocks::payload_builder", payload_id = %attributes.payload_id(), "Using pre-confirmed state for payload");

                let block: RecoveredBlock<Block<OpTxEnvelope>> =
                    flashblock.clone().try_into().map_err(|_| {
                        PayloadBuilderError::Other(
                            eyre!("Failed to convert flashblock to recovered block").into(),
                        )
                    })?;

                let sealed = block.into_sealed_block();

                let payload = OpBuiltPayload::new(
                    attributes.payload_id(),
                    Arc::new(sealed),
                    flashblock.flashblock().metadata.fees,
                    None,
                );

                return Ok(Some((payload, flashblock.flashblock().index)));
            }
        }

        Ok(None)
    }
}

/// Settings for the [`FlashblockJobGenerator`]
#[derive(Debug, Clone)]
pub struct FlashblocksJobGeneratorConfig {
    /// The interval at which the job should build a new payload after the last.
    interval: Duration,
    /// The interval at which the job should recommit to the transaction pool.
    recommitment_interval: Duration,
    /// The maximum number of concurrent payload build tasks.
    max_payload_tasks: usize,
    /// The deadline for when the payload builder job should resolve.
    ///
    /// By default this is [`SLOT_DURATION`]: 12s
    deadline: Duration,
    /// Whether to enable Authorization's for payloads.
    enable_authorization: bool,
}

// === impl Flashblocks ===

impl FlashblocksJobGeneratorConfig {
    /// Sets the interval at which the job should build a new payload after the last.
    pub const fn interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// Sets the deadline when this job should resolve.
    pub const fn deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// Sets the flag to enable or disable Authorization's for payloads.
    pub const fn authorization(mut self, enable: bool) -> Self {
        self.enable_authorization = enable;
        self
    }

    /// Sets the recommitment interval at which the job should re-commit to the transaction pool.
    pub const fn recommitment_interval(mut self, interval: Duration) -> Self {
        self.recommitment_interval = interval;
        self
    }

    /// Sets the maximum number of concurrent payload build tasks.
    pub const fn max_payload_tasks(mut self, max: usize) -> Self {
        self.max_payload_tasks = max;
        self
    }
}

impl Default for FlashblocksJobGeneratorConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(200),
            recommitment_interval: Duration::from_millis(50),
            deadline: Duration::from_secs(2),
            enable_authorization: true,
            max_payload_tasks: 4,
        }
    }
}

/// Returns the duration until the given unix timestamp in seconds.
///
/// Returns `Duration::ZERO` if the given timestamp is in the past.
fn duration_until(unix_timestamp_secs: u64) -> Duration {
    let unix_now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let timestamp = Duration::from_secs(unix_timestamp_secs);
    timestamp.saturating_sub(unix_now)
}
