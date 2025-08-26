use std::{
    future::Future,
    pin::{pin, Pin},
    task::{ready, Context, Poll},
    time::Duration,
};

use flashblocks_p2p::protocol::{error::FlashblocksP2PError, handler::FlashblocksHandle};
use futures::FutureExt;
use op_alloy_consensus::OpTxEnvelope;
use reth::{
    api::{PayloadBuilderError, PayloadKind},
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
use rollup_boost::{
    ed25519_dalek::SigningKey, Authorization, AuthorizedPayload, FlashblocksPayloadV1,
};
use tokio::{sync::oneshot, time::Sleep};
use tracing::{debug, error, info, span, trace};

use crate::{builder::executor::FlashblocksStateExecutor, primitives::Flashblock};

/// A payload job that continuously spawns new build tasks at regular intervals, each building on top of the previous `best_payload`.
///
/// This type is a [`PayloadJob`] and [`Future`] that terminates when the deadline is reached or
/// when the job is resolved: [`PayloadJob::resolve`].
///
/// This [`WorldChainPayloadJob`] implementation spawns new payload build tasks at fixed intervals. Each new build
/// task uses the current `best_payload` as an absolute prestate, allowing for each successive build to be a pre-commitment to the next.
///
/// The spawning continues until the job is resolved, the deadline is reached, or the built payload
/// is marked as frozen: [`BuildOutcome::Freeze`]. Once a frozen payload is returned, no additional
/// payloads will be built and this future will wait to be resolved: [`PayloadJob::resolve`] or
/// terminated if the deadline is reached.
pub struct WorldChainPayloadJob<Tasks, Builder: PayloadBuilder> {
    /// The configuration for how the payload will be created.
    pub(crate) config: PayloadConfig<Builder::Attributes, HeaderForPayload<Builder::BuiltPayload>>,
    /// How to spawn building tasks
    pub(crate) executor: Tasks,
    /// The best payload so far and its state.
    pub(crate) best_payload: PayloadState<Builder::BuiltPayload>,
    /// Receiver for the block that is currently being built.
    pub(crate) pending_block: Option<PendingPayload<Builder::BuiltPayload>>,
    /// Restricts how many generator tasks can be executed at once.
    pub(crate) payload_task_guard: PayloadTaskGuard,
    /// Caches all disk reads for the state the new payloads builds on
    ///
    /// This is used to avoid reading the same state over and over again when new attempts are
    /// triggered, because during the building process we'll repeatedly execute the transactions.
    pub(crate) cached_reads: Option<CachedReads>,
    /// TODO: Add Metrics
    // /// metrics for this type
    // pub(crate) metrics: PayloadBuilderMetrics,
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
    pub(crate) interval: Duration,
    /// The p2p handler for flashblocks
    pub(crate) p2p_handler: FlashblocksHandle,
    /// The flashblocks state executor
    pub(crate) flashblocks_state: FlashblocksStateExecutor,
    /// Any pre-confirmed state on the Payload ID corresponding to this job
    pub(crate) pre_built_payload: Option<Builder::BuiltPayload>,
    /// Block index
    pub(crate) block_index: u64,
    /// The builder signing key
    pub(crate) builder_signing_key: SigningKey,
}

impl<Tasks, Builder> WorldChainPayloadJob<Tasks, Builder>
where
    Tasks: TaskSpawner + Clone + 'static,
    Builder: PayloadBuilder<
            BuiltPayload = OpBuiltPayload<OpPrimitives>,
            Attributes = OpPayloadBuilderAttributes<OpTxEnvelope>,
        > + Unpin
        + 'static,
    Builder::Attributes: Unpin + Clone,
    Builder::BuiltPayload: Unpin + Clone,
{
    /// Spawns a new payload build task that builds on top of the current `best_payload`.
    ///
    /// This method creates a new build job using the current `best_payload` as the base,
    /// allowing each successive build to improve upon the previous one.
    pub(crate) fn spawn_build_job(&mut self) {
        trace!(target: "payload_builder", id = %self.config.payload_id(), "spawn new payload build task");
        let (tx, rx) = oneshot::channel();
        let cancel = CancelOnDrop::default();
        let _cancel = cancel.clone();
        let guard = self.payload_task_guard.clone();
        let payload_config = self.config.clone();
        let best_payload = self.best_payload.payload().cloned();
        // self.metrics.inc_initiated_payload_builds();

        let cached_reads = self.cached_reads.take().unwrap_or_default();
        let builder = self.builder.clone();

        if let Some(pre_built_payload) = self.pre_built_payload.clone() {
            self.best_payload = PayloadState::Frozen(pre_built_payload);
        }

        self.executor.spawn_blocking(Box::pin(async move {
            let _permit = guard.acquire().await;
            let args = BuildArguments {
                cached_reads,
                config: payload_config,
                cancel,
                best_payload,
            };

            let result = builder.try_build(args);
            let _ = tx.send(result);
        }));

        self.pending_block = Some(PendingPayload::new(_cancel, rx));
    }

    /// Publishes a new payload to the [`FlashblocksHandle`] after every build job has resolved.
    ///
    /// An [`AuthorizedPayload<FlashblockPayloadV1>`] signed by the builder is sent to
    /// the [`FlashblocksHandle`] where the payload will be broadcasted across the network.
    /// See: [`FlashblocksHandle::publish_new`].
    pub fn publish_payload(
        &self,
        payload: &OpBuiltPayload<OpPrimitives>,
        prev: &Option<OpBuiltPayload<OpPrimitives>>,
    ) -> Result<(), FlashblocksP2PError> {
        let offset = prev
            .as_ref()
            .map_or(0, |p| p.block().body().transactions().count());

        let flashblock = Flashblock::new(payload, self.config.clone(), self.block_index, offset);
        trace!(target: "jobs_generator", id=%self.config.payload_id(), "creating authorized flashblock");

        let authorized_payload = self.authorization_for(flashblock.into_flashblock());

        self.flashblocks_state
            .publish_built_payload(authorized_payload, payload.to_owned())
            .inspect_err(|err| {
                error!(target: "jobs_generator", id=%self.config.payload_id(), %err, "failed to publish new payload");
            })
    }

    pub fn authorization_for(
        &self,
        payload: FlashblocksPayloadV1,
    ) -> AuthorizedPayload<FlashblocksPayloadV1> {
        AuthorizedPayload::new(&self.builder_signing_key, self.authorization, payload)
    }
}

impl<Tasks, Builder> Future for WorldChainPayloadJob<Tasks, Builder>
where
    Tasks: TaskSpawner + Clone + 'static,
    Builder: PayloadBuilder<
            BuiltPayload = OpBuiltPayload,
            Attributes = OpPayloadBuilderAttributes<OpTxEnvelope>,
        > + Unpin
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
    /// [`Poll::Pending`] for ongoing work or [`Poll::Ready(Ok(()))`] when complete.
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let _span = span!(target: "jobs_generator", tracing::Level::DEBUG, "poll").entered();
        let _enter = _span.enter();
        let this = self.get_mut();
        // check if the deadline is reached
        if this.deadline.as_mut().poll(cx).is_ready() {
            trace!(target: "jobs_generator", "payload building deadline reached");
            return Poll::Ready(Ok(()));
        }

        // First wait for the 200ms interval to complete
        if this.flashblock_deadline.as_mut().poll(cx).is_ready() {
            // Reset the interval for the next iteration
            this.flashblock_deadline
                .as_mut()
                .reset(tokio::time::Instant::now() + this.interval);

            // Now wait for network clearance before proceeding
            let network_handle = this.p2p_handler.clone();
            ready!(pin!(network_handle.await_clearance()).poll(cx));

            trace!(target: "jobs_generator", id=%this.config.payload_id(), "interval elapsed, and clearance granted");
        } else {
            return Poll::Pending;
        }

        // poll the pending block
        if let Some(mut fut) = this.pending_block.take() {
            match fut.poll_unpin(cx) {
                Poll::Ready(Ok(outcome)) => match outcome {
                    BuildOutcome::Better {
                        payload,
                        cached_reads,
                    } => {
                        let prev = this.best_payload.clone();
                        this.best_payload = PayloadState::Best(payload.clone());
                        this.cached_reads = Some(cached_reads);

                        trace!(target: "jobs_generator", value = %payload.fees(), "building new best payload");
                        // publish the new payload to the p2p network
                        if let Err(err) = this.publish_payload(&payload, &prev.payload().cloned()) {
                            error!(target: "jobs_generator", %err, "failed to publish new payload to p2p network");
                        } else {
                            trace!(target: "jobs_generator", id=%this.config.payload_id(), "published new best payload to p2p network");
                        }

                        // increment the pre-confirmation index
                        this.block_index += 1;
                        this.spawn_build_job();
                    }
                    BuildOutcome::Freeze(payload) => {
                        debug!(target: "jobs_generator", "payload frozen, no further building will occur");
                        this.best_payload = PayloadState::Frozen(payload);
                    }
                    BuildOutcome::Aborted { fees, cached_reads } => {
                        this.cached_reads = Some(cached_reads);
                        trace!(target: "jobs_generator", worse_fees = %fees, "skipped payload build of worse block");
                    }
                    BuildOutcome::Cancelled => {
                        unreachable!("the cancel signal never fired")
                    }
                },
                Poll::Ready(Err(error)) => {
                    // job failed, but we simply try again next interval
                    debug!(target: "jobs_generator", %error, "payload build attempt failed");
                    // this.metrics.inc_failed_payload_builds();
                }
                Poll::Pending => {
                    this.pending_block = Some(fut);
                }
            }
        }

        Poll::Pending
    }
}

impl<Tasks, Builder> PayloadJob for WorldChainPayloadJob<Tasks, Builder>
where
    Tasks: TaskSpawner + Clone + 'static,
    Builder: PayloadBuilder<
            BuiltPayload = OpBuiltPayload,
            Attributes = OpPayloadBuilderAttributes<OpTxEnvelope>,
        > + Unpin
        + 'static,
    Builder::Attributes: Unpin + Clone,
    Builder::BuiltPayload: Unpin + Clone,
{
    type PayloadAttributes = Builder::Attributes;
    type ResolvePayloadFuture = ResolveBestPayload<Self::BuiltPayload>;
    type BuiltPayload = Builder::BuiltPayload;

    fn best_payload(&self) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        if let Some(payload) = self.best_payload.payload() {
            Ok(payload.clone())
        } else {
            info!(target: "payload_builder", id=%self.config.payload_id(), "no best payload available, building empty payload");
            // No payload has been built yet, but we need to return something that the CL then
            // can deliver, so we need to return an empty payload.
            //
            // Note: it is assumed that this is unlikely to happen, as the payload job is
            // started right away and the first full block should have been
            // built by the time CL is requesting the payload.
            // self.metrics.inc_requested_empty_payload();
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
        let best_payload = self.best_payload.payload().cloned();
        if best_payload.is_none() && self.pending_block.is_none() {
            // ensure we have a job scheduled if we don't have a best payload yet and none is active
            self.spawn_build_job();
        }

        let maybe_better = self.pending_block.take();
        let mut empty_payload = None;

        if best_payload.is_none() {
            debug!(target: "payload_builder", id=%self.config.payload_id(), "no best payload yet to resolve, building empty payload");

            let args = BuildArguments {
                cached_reads: self.cached_reads.take().unwrap_or_default(),
                config: self.config.clone(),
                cancel: CancelOnDrop::default(),
                best_payload: None,
            };

            match self.builder.on_missing_payload(args) {
                MissingPayloadBehaviour::AwaitInProgress => {
                    debug!(target: "payload_builder", id=%self.config.payload_id(), "awaiting in progress payload build job");
                }
                MissingPayloadBehaviour::RaceEmptyPayload => {
                    debug!(target: "payload_builder", id=%self.config.payload_id(), "racing empty payload");

                    // if no payload has been built yet
                    // self.metrics.inc_requested_empty_payload();
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
                    debug!(target: "payload_builder", id=%self.config.payload_id(), "racing fallback payload");
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
            best_payload,
            maybe_better,
            empty_payload: empty_payload.filter(|_| kind != PayloadKind::WaitForPending),
        };

        (fut, KeepPayloadJobAlive::No)
    }
}
