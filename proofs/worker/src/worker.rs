//! Service future leasing proof jobs from the `prover-service` and submitting results.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use backon::{ConstantBuilder, Retryable};
use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use pin_project::pin_project;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
    time::Sleep,
};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};
use tracing::{Instrument, info, info_span, warn};
use world_chain_prover_service::{
    BackendUpdate, LockId, LockedBackendProofWork, LockedProofRequest, ProofBackend, ProofJobQueue,
    ProofJobQueueError, ProofRequest, ProofResponse, ProofSubmissionLock,
};

use crate::backend::ProofJobBackend;

/// Default number of jobs proving concurrently. One is right for a local CPU prover, which a
/// single job already saturates; raise it for backends that parallelize externally (the
/// Succinct proving network) or that are cheap and local (TEE attestation).
pub const DEFAULT_MAX_CONCURRENT_JOBS: usize = 1;

/// Default sleep between lock attempts when no work is available.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(10);

const REPORT_UPDATE_MAX_RETRIES: usize = 2;
const REPORT_UPDATE_RETRY_DELAY: Duration = Duration::from_millis(100);

type Job<T> = Pin<Box<dyn Future<Output = Result<T, ProofJobQueueError>> + Send>>;

/// A self-contained report of one job result: performs the `prover-service` round-trip and
/// logs its own outcome.
type ReportFuture = BoxFuture<'static, ()>;

/// Work leased from either the user-facing proof queue or the durable backend-job queue.
enum LeasedWork {
    Start(LockedProofRequest),
    Backend(LockedBackendProofWork),
}

/// Configuration for a [`ProofWorker`].
#[derive(Clone, Debug)]
pub struct ProofWorkerConfig {
    /// The worker id.
    pub worker_id: String,
    /// Sleep between lock attempts when no work is available.
    pub poll_interval: Duration,
    /// Maximum number of jobs this worker proves concurrently. Per-worker, not global, so an
    /// SP1 worker and a TEE worker in the same process throttle independently.
    pub max_concurrent_jobs: usize,
}

impl ProofWorkerConfig {
    /// Create a new `ProofWorkerConfig` with the provided `worker_id`
    /// `poll_interval` and `max_concurrent_jobs`.
    pub fn new(worker_id: String, poll_interval: Duration, max_concurrent_jobs: usize) -> Self {
        Self {
            worker_id,
            poll_interval,
            max_concurrent_jobs,
        }
    }
}

/// A backend update paired with the lock it answers.
struct FinishedJob {
    lock: JobLock,
    result: anyhow::Result<BackendUpdate>,
}

/// The locked row that must be used when reporting a finished task.
enum JobLock {
    Start {
        request: ProofRequest,
        lock_id: LockId,
    },
    Backend {
        backend_job_id: i64,
        request: ProofRequest,
        lock_id: LockId,
    },
}

/// Locked sub-state: at most one `getNextProof` request is in flight at a time, and a lock is
/// only started once a concurrency permit is held for the job it may return.
#[pin_project(project = LeaseStateProj)]
enum WorkerState {
    /// Acquire a permit and start a lock on the next poll.
    Ready,
    /// Sleeping out the poll interval after an empty or failed lock attempt.
    Idle(Pin<Box<Sleep>>),
    /// Waiting on `getNextProof`, holding the permit for the prospective job.
    Leasing {
        /// A future yielding the next proof job.
        future: Job<Option<LeasedWork>>,
        /// The job's concurrency permit, taken when the job spawns.
        permit: Option<OwnedSemaphorePermit>,
    },
}

impl WorkerState {
    /// Transitions to sleeping out the poll interval before the next lock attempt.
    fn idle(poll_interval: Duration) -> Self {
        Self::Idle(Box::pin(tokio::time::sleep(poll_interval)))
    }

    /// Transitions to leasing the next queued job for `lane`.
    fn leasing<Q>(
        queue: &Arc<Q>,
        lane: ProofBackend,
        worker_id: String,
        permit: OwnedSemaphorePermit,
    ) -> Self
    where
        Q: ProofJobQueue + Send + Sync + 'static,
    {
        let queue = Arc::clone(queue);
        Self::Leasing {
            future: Box::pin(async move {
                Ok(queue
                    .get_next_proof(lane, worker_id)
                    .await?
                    .map(LeasedWork::Start))
            }),
            permit: Some(permit),
        }
    }
}

/// Worker that leases proof jobs for one backend lane from the `prover-service`, proves them
/// with a [`ProofJobBackend`], and submits the results back.
///
/// The worker is a [`Future`] driving three sources concurrently: a lock loop throttled by a
/// per-worker concurrency semaphore, a [`JoinSet`] of proving tasks on the blocking pool, and
/// the in-flight result reports. It is generic over the backend, so a process can run one
/// instance per lane (an SP1 worker, a TEE worker) — each its own future with independent
/// config and concurrency — and compose them with `join!`/`select!`. Individual job failures
/// are reported to the `prover-service` (which re-queues them until their attempts are
/// exhausted) and never abort the worker.
///
/// The future resolves only when [`ProofWorker::cancellation_token`] is cancelled: the worker
/// stops leasing, flushes pending reports, and abandons jobs still proving — their leases
/// expire server-side and the jobs are re-queued.
#[pin_project]
pub struct ProofWorker<Q, B> {
    queue: Arc<Q>,
    backend: Arc<B>,
    lane: ProofBackend,
    poll_interval: Duration,
    concurrency: Arc<Semaphore>,
    cancel: CancellationToken,
    cancelled: Pin<Box<WaitForCancellationFutureOwned>>,
    /// In-flight proving tasks on the blocking pool.
    jobs: JoinSet<FinishedJob>,
    /// In-flight result reports to the `prover-service`.
    reports: FuturesUnordered<ReportFuture>,
    worker_id: String,

    #[pin]
    lock: WorkerState,
}

impl<Q, B> ProofWorker<Q, B>
where
    Q: ProofJobQueue + Send + Sync + 'static,
    B: ProofJobBackend,
{
    /// Creates a worker leasing `backend`'s lane from `queue`.
    pub fn new(queue: Q, backend: B, config: ProofWorkerConfig) -> Self {
        let cancel = CancellationToken::new();
        let cancelled = Box::pin(cancel.clone().cancelled_owned());
        Self {
            lane: backend.lane(),
            queue: Arc::new(queue),
            backend: Arc::new(backend),
            poll_interval: config.poll_interval,
            concurrency: Arc::new(Semaphore::new(config.max_concurrent_jobs.max(1))),
            cancel,
            cancelled,
            jobs: JoinSet::new(),
            reports: FuturesUnordered::new(),
            worker_id: config.worker_id,
            lock: WorkerState::Ready,
        }
    }

    /// Token that gracefully shuts the worker down when cancelled.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }
}

impl<Q, B> Future for ProofWorker<Q, B>
where
    Q: ProofJobQueue + Send + Sync + 'static,
    B: ProofJobBackend,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        let shutting_down = this.cancelled.as_mut().poll(cx).is_ready();

        // Turn finished proving tasks into report futures, then drive every report so newly
        // pushed ones register their wakers in this same pass.
        while let Poll::Ready(Some(joined)) = this.jobs.poll_join_next(cx) {
            match joined {
                Ok(FinishedJob { lock, result }) => {
                    this.reports
                        .push(report(this.queue, lock, this.worker_id.clone(), result));
                }
                Err(join_error) => warn!(%join_error, "proving task failed to join"),
            }
        }
        while let Poll::Ready(Some(())) = this.reports.poll_next_unpin(cx) {}

        // Lock new work while concurrency permits are available. The loop only exits through
        // a `break` once an inner future returns `Pending`.
        if !shutting_down {
            loop {
                match this.lock.as_mut().project() {
                    LeaseStateProj::Ready => match Arc::clone(this.concurrency).try_acquire_owned()
                    {
                        Ok(permit) => {
                            this.lock.set(WorkerState::leasing(
                                this.queue,
                                *this.lane,
                                this.worker_id.clone(),
                                permit,
                            ));
                        }
                        // Every permit is proving. A finishing job wakes this task and frees
                        // its permit; the idle backoff is a safety net in case it does not.
                        Err(_) => this.lock.set(WorkerState::idle(*this.poll_interval)),
                    },
                    LeaseStateProj::Idle(sleep) => {
                        if sleep.as_mut().poll(cx).is_pending() {
                            break;
                        }
                        this.lock.set(WorkerState::Ready);
                    }
                    LeaseStateProj::Leasing { future, permit } => match future.as_mut().poll(cx) {
                        Poll::Pending => break,
                        Poll::Ready(Ok(Some(work))) => {
                            let permit = permit
                                .take()
                                .expect("leasing state holds its permit until the job spawns");
                            spawn_job(this.jobs, this.backend, work, permit);
                            this.lock.set(WorkerState::Ready);
                        }
                        Poll::Ready(Ok(None)) => {
                            this.lock.set(WorkerState::idle(*this.poll_interval));
                        }
                        Poll::Ready(Err(error)) => {
                            warn!(%error, "failed to lock next proof job");
                            this.lock.set(WorkerState::idle(*this.poll_interval));
                        }
                    },
                }
            }
        }

        if shutting_down && this.reports.is_empty() {
            if !this.jobs.is_empty() {
                warn!(
                    jobs = this.jobs.len(),
                    "abandoning in-flight proving jobs; their leases will expire and re-queue"
                );
            }
            info!("proof worker shut down");
            return Poll::Ready(());
        }
        Poll::Pending
    }
}

/// Dispatches a leased job to the blocking pool under a per-job tracing span, holding its
/// concurrency permit until the proof completes.
///
/// Panics are contained so a crashed job is failed promptly instead of waiting out its lock.
/// The task is not interruptible once started: on worker shutdown it is abandoned and its
/// result discarded.
fn spawn_job<B>(
    jobs: &mut JoinSet<FinishedJob>,
    backend: &Arc<B>,
    work: LeasedWork,
    permit: OwnedSemaphorePermit,
) where
    B: ProofJobBackend,
{
    let (request, backend_state) = match &work {
        LeasedWork::Start(leased) => (&leased.request, None),
        LeasedWork::Backend(leased) => (&leased.work.proof_request, Some(leased.work.state)),
    };
    let span = info_span!("proof_job", id = %request.id(), lane = %request.backend);
    span.in_scope(|| {
        info!(
            game = %request.game,
            l2_block_number = request.l2_block_number,
            "processing proof request"
        );
    });

    let backend = Arc::clone(backend);
    jobs.spawn_blocking(move || {
        let _guard = span.enter();
        let _permit = permit;
        let lock = match work {
            LeasedWork::Start(leased) => JobLock::Start {
                request: leased.request,
                lock_id: leased.lock_id,
            },
            LeasedWork::Backend(leased) => JobLock::Backend {
                backend_job_id: leased.backend_job_id,
                request: leased.work.proof_request,
                lock_id: leased.lock_id,
            },
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &lock {
            JobLock::Start { request, .. } => backend.start(request),
            JobLock::Backend { request, .. } => {
                let state = backend_state.expect("backend work has state");
                backend.advance(request, state)
            }
        }))
        .unwrap_or_else(|panic| {
            Err(anyhow::anyhow!(
                "proving panicked: {}",
                panic_message(&*panic)
            ))
        });
        FinishedJob { lock, result }
    });
}

/// Builds the future that reports one finished job (submit or fail) and logs the outcome.
fn report<Q>(
    queue: &Arc<Q>,
    lock: JobLock,
    worker_id: String,
    result: anyhow::Result<BackendUpdate>,
) -> ReportFuture
where
    Q: ProofJobQueue + Send + Sync + 'static,
{
    let (id, lane) = match &lock {
        JobLock::Start { request, .. } | JobLock::Backend { request, .. } => {
            (request.id(), request.backend)
        }
    };
    let span = info_span!("proof_job", %id, lane = %lane);
    let queue = Arc::clone(queue);

    // `{:#}` renders the full anyhow context chain into the failure reason.
    match result.map_err(|error| format!("{error:#}")) {
        Ok(update) => report_update(queue, lock, worker_id, update)
            .instrument(span)
            .boxed(),
        Err(reason) => Box::pin(
            async move {
                warn!(%reason, "proving failed");
                report_failure(queue, lock, worker_id, reason).await;
            }
            .instrument(span),
        ),
    }
}

async fn report_update<Q>(queue: Arc<Q>, lock: JobLock, worker_id: String, update: BackendUpdate)
where
    Q: ProofJobQueue + Send + Sync + 'static,
{
    match lock {
        JobLock::Start { request, lock_id } => {
            report_start_update(queue, request, lock_id, worker_id, update).await
        }
        JobLock::Backend {
            backend_job_id,
            lock_id,
            ..
        } => report_backend_update(queue, backend_job_id, lock_id, update).await,
    }
}

async fn report_start_update<Q>(
    queue: Arc<Q>,
    request: ProofRequest,
    lock_id: LockId,
    worker_id: String,
    update: BackendUpdate,
) where
    Q: ProofJobQueue + Send + Sync + 'static,
{
    let id = request.id();
    let result = match update {
        BackendUpdate::Pending { state } => {
            queue
                .submit_backend_proof_state(id, state, lock_id, worker_id)
                .await
        }
        BackendUpdate::Complete(proof) => {
            queue
                .submit_proof(
                    ProofResponse { id, proof },
                    ProofSubmissionLock::ProofJob { lock_id },
                )
                .await
        }
        BackendUpdate::Failed(reason) => queue.fail_proof(id, reason, lock_id, worker_id).await,
        BackendUpdate::Noop => {
            queue
                .fail_proof(
                    id,
                    "backend start returned no state update".to_string(),
                    lock_id,
                    worker_id,
                )
                .await
        }
    };

    match result {
        Ok(()) => info!("proof job update submitted"),
        Err(error) => warn!(%error, "failed to submit proof job update"),
    }
}

/// Best-effort extraction of a panic payload's message.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}
