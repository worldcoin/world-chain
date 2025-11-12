use std::{
    future::Future,
    pin::{pin, Pin},
    task::{Context, Poll},
    time::Duration,
};

use alloy_primitives::{keccak256, ruint::aliases::U256, B256};
use flashblocks_builder::{
    coordinator::FlashblocksExecutionCoordinator, traits::payload_builder::FlashblockPayloadBuilder,
};
use flashblocks_p2p::protocol::{error::FlashblocksP2PError, handler::FlashblocksHandle};
use flashblocks_primitives::{
    access_list::{FlashblockAccessList, FlashblockAccessListData},
    flashblocks::Flashblock,
    p2p::{Authorization, AuthorizedPayload},
    primitives::FlashblocksPayloadV1,
};
use std::task::ready;

use futures::FutureExt;
use op_alloy_consensus::OpTxEnvelope;
use reth::{
    api::{BuiltPayload, PayloadBuilderError, PayloadKind},
    network::types::Encodable2718,
    payload::{KeepPayloadJobAlive, PayloadJob},
    revm::{cached::CachedReads, cancelled::CancelOnDrop},
    tasks::TaskSpawner,
};
use reth_basic_payload_builder::{
    BuildArguments, BuildOutcome, HeaderForPayload, MissingPayloadBehaviour, PayloadBuilder,
    PayloadConfig, PayloadState, PayloadTaskGuard, PendingPayload, ResolveBestPayload,
};
use reth_optimism_node::OpPayloadBuilderAttributes;
use reth_optimism_payload_builder::OpBuiltPayload;
use reth_optimism_primitives::OpPrimitives;
use tokio::{
    sync::oneshot,
    time::{Interval, Sleep},
};
use tracing::{debug, error, info, span, trace};

use crate::metrics::PayloadBuilderMetrics;

/// A future that resolves to the result of the block building job.
#[derive(Debug)]
pub struct FlashblocksPendingPayload<P> {
    /// The marker to cancel the job on drop
    _cancel: CancelOnDrop,
    /// The channel to send the result to.
    payload:
        oneshot::Receiver<Result<(BuildOutcome<P>, FlashblockAccessList), PayloadBuilderError>>,
}

// FIXME: this conversion is sad
impl<P: Send + Sync + 'static> From<FlashblocksPendingPayload<P>> for PendingPayload<P> {
    fn from(value: FlashblocksPendingPayload<P>) -> Self {
        let FlashblocksPendingPayload { _cancel, payload } = value;

        let payload = async move {
            match payload.await {
                Ok(Ok((outcome, _access_list))) => Ok(outcome),
                Ok(Err(e)) => Err(e),
                Err(recv_err) => Err(PayloadBuilderError::from(recv_err)), // adjust if needed
            }
        };

        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = tx.send(payload.await);
        });

        PendingPayload::new(_cancel, rx)
    }
}

impl<P> FlashblocksPendingPayload<P> {
    /// Constructs a [`FlashblocksPendingPayload`] future.
    pub const fn new(
        cancel: CancelOnDrop,
        payload: oneshot::Receiver<
            Result<(BuildOutcome<P>, FlashblockAccessList), PayloadBuilderError>,
        >,
    ) -> Self {
        Self {
            _cancel: cancel,
            payload,
        }
    }
}

impl<P> Future for FlashblocksPendingPayload<P> {
    type Output = Result<(BuildOutcome<P>, FlashblockAccessList), PayloadBuilderError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let res = ready!(self.payload.poll_unpin(cx));
        Poll::Ready(res.map_err(Into::into).and_then(|res| res))
    }
}

pub enum CommittedPayloadState<P> {
    Frozen {
        payload: P,
        access_list: FlashblockAccessListData,
    },
    Best {
        payload: P,
        access_list: FlashblockAccessListData,
    },
    Empty,
}

impl<P: BuiltPayload> CommittedPayloadState<P> {
    pub fn payload(&self) -> Option<&P> {
        match self {
            CommittedPayloadState::Frozen { payload, .. } => Some(payload),
            CommittedPayloadState::Best { payload, .. } => Some(payload),
            CommittedPayloadState::Empty => None,
        }
    }

    pub fn block_hash(&self) -> Option<B256> {
        match self {
            CommittedPayloadState::Frozen { payload, .. } => Some(payload.block().hash()),
            CommittedPayloadState::Best { payload, .. } => Some(payload.block().hash()),
            CommittedPayloadState::Empty => None,
        }
    }

    pub fn is_frozen(&self) -> bool {
        matches!(self, CommittedPayloadState::Frozen { .. })
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, CommittedPayloadState::Empty)
    }

    pub fn fees(&self) -> Option<U256> {
        match self {
            CommittedPayloadState::Frozen { payload, .. } => Some(payload.fees()),
            CommittedPayloadState::Best { payload, .. } => Some(payload.fees()),
            CommittedPayloadState::Empty => None,
        }
    }

    pub fn access_list(&self) -> Option<&FlashblockAccessListData> {
        match self {
            CommittedPayloadState::Frozen { access_list, .. } => Some(access_list),
            CommittedPayloadState::Best { access_list, .. } => Some(access_list),
            CommittedPayloadState::Empty => None,
        }
    }

    pub fn take_access_list(&mut self) -> Option<FlashblockAccessListData> {
        match self {
            CommittedPayloadState::Frozen { access_list, .. } => Some(FlashblockAccessListData {
                access_list_hash: access_list.access_list_hash,
                access_list: core::mem::take(&mut access_list.access_list),
            }),
            CommittedPayloadState::Best { access_list, .. } => Some(FlashblockAccessListData {
                access_list_hash: access_list.access_list_hash,
                access_list: core::mem::take(&mut access_list.access_list),
            }),
            CommittedPayloadState::Empty => None,
        }
    }

    pub fn take_payload(&self) -> Option<P>
    where
        P: Clone,
    {
        match self {
            CommittedPayloadState::Frozen { payload, .. }
            | CommittedPayloadState::Best { payload, .. } => Some(payload.clone()),
            CommittedPayloadState::Empty => None,
        }
    }
}

impl<P: Clone> Clone for CommittedPayloadState<P> {
    fn clone(&self) -> Self {
        match self {
            CommittedPayloadState::Frozen {
                payload,
                access_list,
            } => CommittedPayloadState::Frozen {
                payload: payload.clone(),
                access_list: FlashblockAccessListData {
                    access_list_hash: access_list.access_list_hash,
                    access_list: access_list.access_list.clone(),
                },
            },
            CommittedPayloadState::Best {
                payload,
                access_list,
            } => CommittedPayloadState::Best {
                payload: payload.clone(),
                access_list: FlashblockAccessListData {
                    access_list_hash: access_list.access_list_hash,
                    access_list: access_list.access_list.clone(),
                },
            },
            CommittedPayloadState::Empty => CommittedPayloadState::Empty,
        }
    }
}

impl<P> From<(PayloadState<P>, FlashblockAccessList)> for CommittedPayloadState<P>
where
    P: Clone,
{
    fn from((state, access_list): (PayloadState<P>, FlashblockAccessList)) -> Self {
        let access_list_data = FlashblockAccessListData {
            access_list_hash: keccak256(alloy_rlp::encode(&access_list)),
            access_list,
        };
        match state {
            PayloadState::Frozen(payload) => CommittedPayloadState::Frozen {
                payload: payload.clone(),
                access_list: access_list_data,
            },
            PayloadState::Best(payload) => CommittedPayloadState::Best {
                payload: payload.clone(),
                access_list: access_list_data,
            },
            PayloadState::Missing => {
                panic!("Cannot convert Missing payload state to CommittedPayloadState")
            }
        }
    }
}

/// A payload job that continuously spawns new build tasks at regular intervals, each building on top of the previous `best_payload`.
///
/// This type is a [`PayloadJob`] and [`Future`] that terminates when the deadline is reached or
/// when the job is resolved: [`PayloadJob::resolve`].
///
/// This [`FlashblocksPayloadJob`] implementation spawns new payload build tasks at fixed intervals. Each new build
/// task uses the current `best_payload` as an absolute prestate, allowing for each successive build to be a pre-commitment to the next.
///
/// The spawning continues until the job is resolved, the deadline is reached, or the built payload
/// is marked as frozen: [`BuildOutcome::Freeze`]. Once a frozen payload is returned, no additional
/// payloads will be built and this future will wait to be resolved: [`PayloadJob::resolve`] or
/// terminated if the deadline is reached.
pub struct FlashblocksPayloadJob<Tasks, Builder: PayloadBuilder> {
    /// The configuration for how the payload will be created.
    pub(crate) config: PayloadConfig<Builder::Attributes, HeaderForPayload<Builder::BuiltPayload>>,
    /// How to spawn building tasks
    pub(crate) executor: Tasks,
    /// The best payload so far and its state.
    pub(crate) best_payload: (
        PayloadState<Builder::BuiltPayload>,
        Option<FlashblockAccessList>,
    ),
    /// The best payload that has been committed, and published to the network.
    /// This payload is a pre-commitment to all future payloads.
    pub(crate) committed_payload: CommittedPayloadState<Builder::BuiltPayload>,
    /// Receiver for the block that is currently being built.
    pub(crate) pending_block: Option<FlashblocksPendingPayload<Builder::BuiltPayload>>,
    /// Restricts how many generator tasks can be executed at once.
    pub(crate) payload_task_guard: PayloadTaskGuard,
    /// Caches all disk reads for the state the new payloads builds on
    ///
    /// This is used to avoid reading the same state over and over again when new attempts are
    /// triggered, because during the building process we'll repeatedly execute the transactions.
    pub(crate) cached_reads: Option<CachedReads>,
    // /// metrics for this type
    pub(crate) metrics: PayloadBuilderMetrics,
    /// The type responsible for building payloads.
    ///
    /// See [`PayloadBuilder`]
    pub(crate) builder: Builder,
    /// The authorization information for this job
    pub(crate) authorization: Authorization,
    /// The deadline when this job should resolve.
    pub(crate) deadline: Pin<Box<Sleep>>,
    /// The interval at which we should attempt to build new payloads
    pub(crate) flashblock_deadline: Pin<Box<Sleep>>,
    /// The interval timer for spawning new build tasks
    pub(crate) flashblock_interval: Duration,
    /// The recommit interval duration
    pub(crate) recommit_interval: Interval,
    /// The p2p handler for flashblocks
    pub(crate) p2p_handler: FlashblocksHandle,
    /// The flashblocks state executor
    pub(crate) flashblocks_state: FlashblocksExecutionCoordinator,
    /// Block index
    pub(crate) block_index: u64,
}

impl<Tasks, Builder> FlashblocksPayloadJob<Tasks, Builder>
where
    Tasks: TaskSpawner + Clone + 'static,
    Builder: PayloadBuilder<
            BuiltPayload = OpBuiltPayload<OpPrimitives>,
            Attributes = OpPayloadBuilderAttributes<OpTxEnvelope>,
        > + FlashblockPayloadBuilder
        + Unpin
        + 'static,
    Builder::Attributes: Unpin + Clone,
    Builder::BuiltPayload: Unpin + Clone,
{
    /// Spawns a new payload build task that builds on top of the current `best_payload`.
    ///
    /// This method creates a new build job using the current `best_payload` as the base,
    /// allowing each successive build to improve upon the previous one.
    pub(crate) fn spawn_build_job(&mut self) {
        trace!(target: "flashblocks::payload_builder", id = %self.config.payload_id(), "spawn new payload build task");
        let (tx, rx) = oneshot::channel();
        let cancel = CancelOnDrop::default();
        let _cancel = cancel.clone();
        let guard = self.payload_task_guard.clone();
        let payload_config = self.config.clone();
        let best_payload = self.best_payload.0.payload().cloned();
        let committed_payload = self.committed_payload.clone();
        self.metrics.inc_initiated_payload_builds();

        let cached_reads = self.cached_reads.take().unwrap_or_default();
        let builder = self.builder.clone();

        self.executor.spawn_blocking(Box::pin(async move {
            let _permit = guard.acquire().await;
            let args = BuildArguments {
                cached_reads,
                config: payload_config,
                cancel,
                best_payload,
            };

            let result = builder.try_build_with_precommit(args, committed_payload.payload());
            let _ = tx.send(result);
        }));

        self.pending_block = Some(FlashblocksPendingPayload::new(_cancel, rx));
    }

    /// Publishes a new payload to the [`FlashblocksHandle`] after every build job has resolved.
    ///
    /// An [`AuthorizedPayload<FlashblockPayloadV1>`] signed by the builder is sent to
    /// the [`FlashblocksHandle`] where the payload will be broadcasted across the network.
    ///
    /// See: [`FlashblocksHandle::publish_new`].
    pub(crate) fn publish_payload(
        &self,
        payload: &OpBuiltPayload<OpPrimitives>,
        access_list: FlashblockAccessList,
        prev: Option<&OpBuiltPayload<OpPrimitives>>,
    ) -> eyre::Result<()> {
        let offset = prev
            .as_ref()
            .map_or(0, |p| p.block().body().transactions().count());

        let flashblock = Flashblock::new(
            payload,
            &self.config,
            self.block_index,
            offset,
            Some(access_list),
        );

        trace!(target: "flashblocks::payload_builder", id=%self.config.payload_id(), "creating authorized flashblock");

        let authorized_payload = self.authorization_for(flashblock.into_flashblock())?;

        self.flashblocks_state
            .publish_built_payload(authorized_payload, payload.to_owned())
            .inspect_err(|err| {
                error!(target: "flashblocks::payload_builder", id=%self.config.payload_id(), %err, "failed to publish new payload");
            })
    }

    pub(crate) fn authorization_for(
        &self,
        payload: FlashblocksPayloadV1,
    ) -> Result<AuthorizedPayload<FlashblocksPayloadV1>, FlashblocksP2PError> {
        Ok(AuthorizedPayload::new(
            self.p2p_handler.builder_sk()?,
            self.authorization,
            payload,
        ))
    }

    pub(crate) fn record_payload_metrics(&self, payload: &OpBuiltPayload<OpPrimitives>) {
        let block = payload.block();
        let payload_bytes: usize = block
            .body()
            .transactions()
            .map(|tx| tx.encoded_2718().len())
            .sum();
        let gas_used = block.header().gas_used;
        let tx_count = block.body().transactions().count();

        self.metrics
            .record_payload_metrics(payload_bytes as u64, gas_used, tx_count);
    }
}

impl<Tasks, Builder> Future for FlashblocksPayloadJob<Tasks, Builder>
where
    Tasks: TaskSpawner + Clone + 'static,
    Builder: PayloadBuilder<
            BuiltPayload = OpBuiltPayload<OpPrimitives>,
            Attributes = OpPayloadBuilderAttributes<OpTxEnvelope>,
        > + FlashblockPayloadBuilder
        + Unpin
        + 'static,
    Builder::Attributes: Unpin + Clone,
    Builder::BuiltPayload: Unpin + Clone,
{
    type Output = Result<(), PayloadBuilderError>;

    /// Polls the payload builder job to drive payload construction and management.
    ///
    /// This implementation follows a state-driven approach with the following logic:
    ///
    /// # Flow
    ///
    /// 1. **Deadline Check**: First checks if the payload building deadline has been reached.
    ///    If so, returns [`Poll::Ready(Ok(()))`] to complete the job.
    ///
    /// 2. **Interval Tick**: Polls the interval timer to determine when to spawn new build jobs.
    ///    - If the interval is ready, continues to spawn a new build job
    ///    - If pending, schedules a wake-up and returns [`Poll::Pending`]
    ///
    /// 3. **Pending Block Processing**: If there's a pending block being built:
    ///    - Processes build outcomes ([`BuildOutcome::Better`], [`BuildOutcome::Freeze`], [`BuildOutcome::Aborted`])
    ///    - [`BuildOutcome::Better`]:
    ///         - Each successive payload is a pre-commitment to the next payload.
    ///         - We will continuously build on top of the best payload until the job is resolved.
    ///    - [`BuildOutcome::Freeze`]: marks payload as frozen and stops further building
    ///    - [`BuildOutcome::Aborted`]: handles cancellation and potential respawning
    ///    - On errors: logs and continues operation
    ///    - If still pending: restores the future and returns [`Poll::Pending`]
    ///
    /// 4. **New Job Spawning**: If no pending block exists, spawns a new build job
    ///    and continues polling.
    ///
    /// # Pre-confirmation Behavior
    ///
    /// Each time we hit [`BuildOutcome::Better`], we increment `block_index` and create a new
    /// pre-confirmation on top of the current best payload. This allows for iterative improvement
    /// where each successive build acts as a pre-commitment to the next block state.
    ///
    /// # Returns
    ///
    /// The polling continues until either the deadline is reached or an error occurs, returning
    /// [`Poll::Pending`] for ongoing work or [`Poll::Ready`] when complete.
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let _span =
            span!(target: "flashblocks::payload_builder", tracing::Level::TRACE, "poll").entered();
        let _enter = _span.enter();
        let this = self.get_mut();

        // check if the deadline is reached
        if this.deadline.as_mut().poll(cx).is_ready() {
            trace!(target: "flashblocks::payload_builder", "payload building deadline reached");
            return Poll::Ready(Ok(()));
        }

        if this.recommit_interval.poll_tick(cx).is_ready() && !this.best_payload.0.is_frozen() {
            trace!(target: "flashblocks::payload_builder", "recommit interval reached, spawning new build job");
            this.spawn_build_job();
        }

        let network_handle = this.p2p_handler.clone();
        let fut = pin!(network_handle.await_clearance());

        let mut joined_fut = pin!(futures::future::join(
            fut,
            this.flashblock_deadline.as_mut()
        ));

        // flashblocks interval reached, and clearance received to publish.
        // commit to the best payload, reset the interval, and publish the payload
        if joined_fut.poll_unpin(cx).is_ready() && !this.best_payload.0.is_frozen() {
            if let (Some(payload), Some(access_list)) = (
                this.best_payload.0.payload().cloned(),
                this.best_payload.1.clone(),
            ) {
                // record metrics
                this.record_payload_metrics(&payload);

                trace!(target: "flashblocks::payload_builder", current_value = %payload.fees(), "committing to best payload");

                if this
                    .committed_payload
                    .payload()
                    .is_none_or(|p| p.block().hash() != payload.block().hash())
                {
                    trace!(target: "flashblocks::payload_builder", id=%this.config.payload_id(), "best payload already committed, skipping publish");

                    // publish the new payload to the p2p network
                    if let Err(err) = this.publish_payload(
                        &payload,
                        access_list.clone(),
                        this.committed_payload.payload(),
                    ) {
                        this.metrics.inc_p2p_publishing_errors();
                        error!(target: "flashblocks::payload_builder", %err, "failed to publish new payload to p2p network");
                    } else {
                        trace!(target: "flashblocks::payload_builder", id=%this.config.payload_id(), "published new best payload to p2p network");
                    }

                    // commit to the best payload
                    this.committed_payload =
                        CommittedPayloadState::from((this.best_payload.0.clone(), access_list));

                    // increment the pre-confirmation index
                    this.block_index += 1;
                }

                this.spawn_build_job();
                this.recommit_interval.reset();

                this.flashblock_deadline
                    .as_mut()
                    .reset(tokio::time::Instant::now() + this.flashblock_interval);
            }
        }

        // poll the pending block
        if let Some(mut fut) = this.pending_block.take() {
            match fut.poll_unpin(cx) {
                Poll::Ready(Ok((outcome, access_list))) => match outcome {
                    BuildOutcome::Better {
                        payload,
                        cached_reads,
                    } => {
                        if this
                            .committed_payload
                            .payload()
                            .is_none_or(|p| p.block().hash() != payload.block().hash())
                        {
                            this.cached_reads = Some(cached_reads);
                            this.best_payload = (PayloadState::Best(payload), Some(access_list));
                        }
                    }
                    BuildOutcome::Freeze(payload) => {
                        trace!(target: "flashblocks::payload_builder", "payload frozen, no further building will occur");
                        this.best_payload = (PayloadState::Frozen(payload), Some(access_list));
                    }
                    BuildOutcome::Aborted { fees, cached_reads } => {
                        this.cached_reads = Some(cached_reads);
                        trace!(target: "flashblocks::payload_builder", worse_fees = %fees, "skipped payload build of worse block");
                    }
                    BuildOutcome::Cancelled => {
                        unreachable!("the cancel signal never fired")
                    }
                },
                Poll::Ready(Err(error)) => {
                    // job failed, but we simply try again next interval
                    debug!(target: "flashblocks::payload_builder", %error, "payload build attempt failed");
                    match &error {
                        PayloadBuilderError::EvmExecutionError(_) => {
                            this.metrics.inc_evm_execution_errors();
                        }
                        PayloadBuilderError::MissingPayload => {
                            this.metrics.inc_database_errors();
                        }
                        PayloadBuilderError::MissingParentHeader(_) => {
                            this.metrics.inc_database_errors();
                        }
                        PayloadBuilderError::MissingParentBlock(_) => {
                            this.metrics.inc_database_errors();
                        }
                        PayloadBuilderError::Internal(_) => {
                            // RethError from provider/database operations
                            this.metrics.inc_database_errors();
                        }
                        PayloadBuilderError::ChannelClosed => {
                            // Communication failure between components
                            this.metrics.inc_payload_build_errors();
                        }
                        PayloadBuilderError::Other(_) => {
                            // Catch-all for unknown errors
                            this.metrics.inc_payload_build_errors();
                        }
                    }

                    this.metrics.inc_failed_payload_builds();
                }
                Poll::Pending => {
                    this.pending_block = Some(fut);
                }
            }
        }

        Poll::Pending
    }
}

impl<Tasks, Builder> PayloadJob for FlashblocksPayloadJob<Tasks, Builder>
where
    Tasks: TaskSpawner + Clone + 'static,
    Builder: PayloadBuilder<
            BuiltPayload = OpBuiltPayload<OpPrimitives>,
            Attributes = OpPayloadBuilderAttributes<OpTxEnvelope>,
        > + FlashblockPayloadBuilder
        + Unpin
        + 'static,
    Builder::Attributes: Unpin + Clone,
    Builder::BuiltPayload: Unpin + Clone,
{
    type PayloadAttributes = Builder::Attributes;
    type ResolvePayloadFuture = ResolveBestPayload<Self::BuiltPayload>;
    type BuiltPayload = Builder::BuiltPayload;

    fn best_payload(&self) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        if let Some(payload) = self.committed_payload.payload() {
            trace!(target: "flashblocks::payload_builder", id=%self.config.payload_id(), value = %payload.fees(), "returning best payload");
            Ok(payload.clone())
        } else {
            info!(target: "flashblocks::payload_builder", id=%self.config.payload_id(), "no best payload available, building empty payload");
            // No payload has been built yet, but we need to return something that the CL then
            // can deliver, so we need to return an empty payload.
            //
            // Note: it is assumed that this is unlikely to happen, as the payload job is
            // started right away and the first full block should have been
            // built by the time CL is requesting the payload.
            self.metrics.inc_requested_empty_payload();
            self.builder.build_empty_payload(self.config.clone())
        }
    }

    fn payload_attributes(&self) -> Result<Self::PayloadAttributes, PayloadBuilderError> {
        Ok(self.config.attributes.clone())
    }

    fn resolve_kind(
        &mut self,
        kind: PayloadKind,
    ) -> (Self::ResolvePayloadFuture, KeepPayloadJobAlive) {
        if self.committed_payload.is_empty() && self.pending_block.is_none() {
            trace!(target: "flashblocks::payload_builder", id=%self.config.payload_id(), "no best payload yet and no active build job, spawning new build job");
            // ensure we have a job scheduled if we don't have a best payload yet and none is active
            self.spawn_build_job();
        }

        let maybe_better: Option<FlashblocksPendingPayload<OpBuiltPayload>> =
            self.pending_block.take();
        let mut empty_payload = None;

        if self.committed_payload.is_empty() {
            debug!(target: "flashblocks::payload_builder", id=%self.config.payload_id(), "no best payload yet to resolve, building empty payload");

            let args = BuildArguments {
                cached_reads: self.cached_reads.take().unwrap_or_default(),
                config: self.config.clone(),
                cancel: CancelOnDrop::default(),
                best_payload: None,
            };

            match self.builder.on_missing_payload(args) {
                MissingPayloadBehaviour::AwaitInProgress => {
                    debug!(target: "flashblocks::payload_builder", id=%self.config.payload_id(), "awaiting in progress payload build job");
                }
                MissingPayloadBehaviour::RaceEmptyPayload => {
                    debug!(target: "flashblocks::payload_builder", id=%self.config.payload_id(), "racing empty payload");

                    // if no payload has been built yet
                    self.metrics.inc_requested_empty_payload();

                    // no payload built yet, so we need to return an empty payload
                    let (tx, rx) = oneshot::channel();
                    let config = self.config.clone();
                    let builder = self.builder.clone();
                    self.executor.spawn_blocking(Box::pin(async move {
                        let res = builder.build_empty_payload(config);
                        let _ = tx.send(res);
                    }));

                    empty_payload = Some(rx);
                }
                MissingPayloadBehaviour::RacePayload(job) => {
                    debug!(target: "flashblocks::payload_builder", id=%self.config.payload_id(), "racing fallback payload");
                    // race the in progress job with this job
                    let (tx, rx) = oneshot::channel();
                    self.executor.spawn_blocking(Box::pin(async move {
                        let _ = tx.send(job());
                    }));
                    empty_payload = Some(rx);
                }
            };
        }

        let fut = ResolveBestPayload {
            best_payload: self.committed_payload.take_payload(),
            maybe_better: maybe_better.map(Into::into),
            empty_payload: empty_payload.filter(|_| kind != PayloadKind::WaitForPending),
        };

        (fut, KeepPayloadJobAlive::No)
    }
}
