//! Service future leasing proof jobs from the `prover-service` and submitting results.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

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
    BackendUpdate, LeaseToken, LeasedBackendProofWork, LeasedProofRequest, ProofBackend,
    ProofJobQueue, ProofJobQueueError, ProofRequest, ProofResponse, ProofSubmissionLease,
};

use crate::backend::ProofJobBackend;

/// Default number of jobs proving concurrently. One is right for a local CPU prover, which a
/// single job already saturates; raise it for backends that parallelize externally (the
/// Succinct proving network) or that are cheap and local (TEE attestation).
pub const DEFAULT_MAX_CONCURRENT_JOBS: usize = 1;

/// Default sleep between lease attempts when no work is available.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(10);

type Job<T> = Pin<Box<dyn Future<Output = Result<T, ProofJobQueueError>> + Send>>;

/// A self-contained report of one job result: performs the `prover-service` round-trip and
/// logs its own outcome.
type ReportFuture = BoxFuture<'static, ()>;

/// Work leased from either the user-facing proof queue or the durable backend-job queue.
enum LeasedWork {
    Start(LeasedProofRequest),
    Backend(LeasedBackendProofWork),
}

/// Configuration for a [`ProofWorker`].
#[derive(Clone, Copy, Debug)]
pub struct ProofWorkerConfig {
    /// Sleep between lease attempts when no work is available.
    pub poll_interval: Duration,
    /// Maximum number of jobs this worker proves concurrently. Per-worker, not global, so an
    /// SP1 worker and a TEE worker in the same process throttle independently.
    pub max_concurrent_jobs: usize,
}

impl Default for ProofWorkerConfig {
    fn default() -> Self {
        Self {
            poll_interval: DEFAULT_POLL_INTERVAL,
            max_concurrent_jobs: DEFAULT_MAX_CONCURRENT_JOBS,
        }
    }
}

/// A backend update paired with the lease it answers.
struct FinishedJob {
    lease: JobLease,
    result: anyhow::Result<BackendUpdate>,
}

/// The leased row that must be used when reporting a finished task.
enum JobLease {
    Start {
        request: ProofRequest,
        lease_token: LeaseToken,
    },
    Backend {
        backend_job_id: i64,
        request: ProofRequest,
        lease_token: LeaseToken,
    },
}

/// Lease sub-state: at most one `getNextProof` request is in flight at a time, and a lease is
/// only started once a concurrency permit is held for the job it may return.
#[pin_project(project = LeaseStateProj)]
enum WorkerState {
    /// Acquire a permit and start a lease on the next poll.
    Ready,
    /// Sleeping out the poll interval after an empty or failed lease attempt.
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
    /// Transitions to sleeping out the poll interval before the next lease attempt.
    fn idle(poll_interval: Duration) -> Self {
        Self::Idle(Box::pin(tokio::time::sleep(poll_interval)))
    }

    /// Transitions to leasing the next queued job for `lane`.
    fn leasing<Q>(queue: &Arc<Q>, lane: ProofBackend, permit: OwnedSemaphorePermit) -> Self
    where
        Q: ProofJobQueue + Send + Sync + 'static,
    {
        let queue = Arc::clone(queue);
        Self::Leasing {
            future: Box::pin(async move {
                if let Some(work) = queue.get_next_backend_proof(lane).await? {
                    return Ok(Some(LeasedWork::Backend(work)));
                }
                Ok(queue.get_next_proof(lane).await?.map(LeasedWork::Start))
            }),
            permit: Some(permit),
        }
    }
}

/// Worker that leases proof jobs for one backend lane from the `prover-service`, proves them
/// with a [`ProofJobBackend`], and submits the results back.
///
/// The worker is a [`Future`] driving three sources concurrently: a lease loop throttled by a
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

    #[pin]
    lease: WorkerState,
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
            lease: WorkerState::Ready,
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
                Ok(FinishedJob { lease, result }) => {
                    this.reports.push(report(this.queue, lease, result));
                }
                Err(join_error) => warn!(%join_error, "proving task failed to join"),
            }
        }
        while let Poll::Ready(Some(())) = this.reports.poll_next_unpin(cx) {}

        // Lease new work while concurrency permits are available. The loop only exits through
        // a `break` once an inner future returns `Pending`.
        if !shutting_down {
            loop {
                match this.lease.as_mut().project() {
                    LeaseStateProj::Ready => match Arc::clone(this.concurrency).try_acquire_owned()
                    {
                        Ok(permit) => {
                            this.lease
                                .set(WorkerState::leasing(this.queue, *this.lane, permit));
                        }
                        // Every permit is proving. A finishing job wakes this task and frees
                        // its permit; the idle backoff is a safety net in case it does not.
                        Err(_) => this.lease.set(WorkerState::idle(*this.poll_interval)),
                    },
                    LeaseStateProj::Idle(sleep) => {
                        if sleep.as_mut().poll(cx).is_pending() {
                            break;
                        }
                        this.lease.set(WorkerState::Ready);
                    }
                    LeaseStateProj::Leasing { future, permit } => match future.as_mut().poll(cx) {
                        Poll::Pending => break,
                        Poll::Ready(Ok(Some(work))) => {
                            let permit = permit
                                .take()
                                .expect("leasing state holds its permit until the job spawns");
                            spawn_job(this.jobs, this.backend, work, permit);
                            this.lease.set(WorkerState::Ready);
                        }
                        Poll::Ready(Ok(None)) => {
                            this.lease.set(WorkerState::idle(*this.poll_interval));
                        }
                        Poll::Ready(Err(error)) => {
                            warn!(%error, "failed to lease next proof job");
                            this.lease.set(WorkerState::idle(*this.poll_interval));
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
/// Panics are contained so a crashed job is failed promptly instead of waiting out its lease.
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
        let lease = match work {
            LeasedWork::Start(leased) => JobLease::Start {
                request: leased.request,
                lease_token: leased.lease_token,
            },
            LeasedWork::Backend(leased) => JobLease::Backend {
                backend_job_id: leased.backend_job_id,
                request: leased.work.proof_request,
                lease_token: leased.lease_token,
            },
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &lease {
            JobLease::Start { request, .. } => backend.start(request),
            JobLease::Backend { request, .. } => {
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
        FinishedJob { lease, result }
    });
}

/// Builds the future that reports one finished job (submit or fail) and logs the outcome.
fn report<Q>(queue: &Arc<Q>, lease: JobLease, result: anyhow::Result<BackendUpdate>) -> ReportFuture
where
    Q: ProofJobQueue + Send + Sync + 'static,
{
    let (id, lane) = match &lease {
        JobLease::Start { request, .. } | JobLease::Backend { request, .. } => {
            (request.id(), request.backend)
        }
    };
    let span = info_span!("proof_job", %id, lane = %lane);
    let queue = Arc::clone(queue);

    // `{:#}` renders the full anyhow context chain into the failure reason.
    match result.map_err(|error| format!("{error:#}")) {
        Ok(update) => report_update(queue, lease, update).instrument(span).boxed(),
        Err(reason) => Box::pin(
            async move {
                warn!(%reason, "proving failed");
                report_failure(queue, lease, reason).await;
            }
            .instrument(span),
        ),
    }
}

async fn report_update<Q>(queue: Arc<Q>, lease: JobLease, update: BackendUpdate)
where
    Q: ProofJobQueue + Send + Sync + 'static,
{
    match lease {
        JobLease::Start {
            request,
            lease_token,
        } => report_start_update(queue, request, lease_token, update).await,
        JobLease::Backend {
            backend_job_id,
            lease_token,
            ..
        } => report_backend_update(queue, backend_job_id, lease_token, update).await,
    }
}

async fn report_start_update<Q>(
    queue: Arc<Q>,
    request: ProofRequest,
    lease_token: LeaseToken,
    update: BackendUpdate,
) where
    Q: ProofJobQueue + Send + Sync + 'static,
{
    let id = request.id();
    let result = match update {
        BackendUpdate::Pending { state } => {
            queue
                .submit_backend_proof_state(id, state, lease_token)
                .await
        }
        BackendUpdate::Complete(proof) => {
            queue
                .submit_proof(
                    ProofResponse { id, proof },
                    ProofSubmissionLease::ProofJob { lease_token },
                )
                .await
        }
        BackendUpdate::Failed(reason) => queue.fail_proof(id, reason, lease_token).await,
        BackendUpdate::Noop => {
            queue
                .fail_proof(
                    id,
                    "backend start returned no state update".to_string(),
                    lease_token,
                )
                .await
        }
    };

    match result {
        Ok(()) => info!("proof job update submitted"),
        Err(error) => warn!(%error, "failed to submit proof job update"),
    }
}

async fn report_backend_update<Q>(
    queue: Arc<Q>,
    backend_job_id: i64,
    lease_token: LeaseToken,
    update: BackendUpdate,
) where
    Q: ProofJobQueue + Send + Sync + 'static,
{
    let result = queue
        .complete_backend_proof_job(backend_job_id, lease_token, update)
        .await;

    match result {
        Ok(()) => info!("backend proof job update submitted"),
        Err(error) => warn!(%error, backend_job_id, "failed to submit backend proof job update"),
    }
}

async fn report_failure<Q>(queue: Arc<Q>, lease: JobLease, reason: String)
where
    Q: ProofJobQueue + Send + Sync + 'static,
{
    let result = match lease {
        JobLease::Start {
            request,
            lease_token,
        } => queue.fail_proof(request.id(), reason, lease_token).await,
        JobLease::Backend {
            backend_job_id,
            lease_token,
            ..
        } => {
            queue
                .complete_backend_proof_job(
                    backend_job_id,
                    lease_token,
                    BackendUpdate::Failed(reason),
                )
                .await
        }
    };

    if let Err(error) = result {
        warn!(%error, "failed to report proving failure");
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

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Barrier, Mutex},
    };

    use alloy_primitives::{Address, B256, Bytes};
    use anyhow::Context as _;
    use async_trait::async_trait;
    use world_chain_prover_service::{BackendProofState, ProofData, ProofRequestId};

    use super::*;

    /// In-memory queue with shared interior so tests keep a handle for assertions.
    #[derive(Clone, Default)]
    struct MockQueue {
        jobs: Arc<Mutex<VecDeque<LeasedProofRequest>>>,
        submitted: Arc<Mutex<Vec<ProofResponse>>>,
        failed: Arc<Mutex<Vec<(ProofRequestId, String)>>>,
    }

    impl MockQueue {
        fn with_jobs(jobs: impl IntoIterator<Item = ProofRequest>) -> Self {
            Self {
                jobs: Arc::new(Mutex::new(
                    jobs.into_iter()
                        .map(|request| LeasedProofRequest {
                            request,
                            lease_token: LeaseToken::new(),
                        })
                        .collect(),
                )),
                ..Self::default()
            }
        }

        fn submitted(&self) -> Vec<ProofResponse> {
            self.submitted.lock().expect("submitted poisoned").clone()
        }

        fn failed(&self) -> Vec<(ProofRequestId, String)> {
            self.failed.lock().expect("failed poisoned").clone()
        }
    }

    #[async_trait]
    impl ProofJobQueue for MockQueue {
        async fn get_next_proof(
            &self,
            _backend: ProofBackend,
        ) -> Result<Option<LeasedProofRequest>, ProofJobQueueError> {
            Ok(self.jobs.lock().expect("jobs poisoned").pop_front())
        }

        async fn submit_backend_proof_state(
            &self,
            _proof_id: ProofRequestId,
            _backend_proof_state: BackendProofState,
            _lease_token: LeaseToken,
        ) -> Result<(), ProofJobQueueError> {
            Ok(())
        }

        async fn get_next_backend_proof(
            &self,
            _backend: ProofBackend,
        ) -> Result<Option<LeasedBackendProofWork>, ProofJobQueueError> {
            Ok(None)
        }

        async fn complete_backend_proof_job(
            &self,
            _backend_job_id: i64,
            _lease_token: LeaseToken,
            _next_update: BackendUpdate,
        ) -> Result<(), ProofJobQueueError> {
            Ok(())
        }

        async fn submit_proof(
            &self,
            proof: ProofResponse,
            _lease: ProofSubmissionLease,
        ) -> Result<(), ProofJobQueueError> {
            self.submitted
                .lock()
                .expect("submitted poisoned")
                .push(proof);
            Ok(())
        }

        async fn fail_proof(
            &self,
            proof_id: ProofRequestId,
            reason: String,
            _lease_token: LeaseToken,
        ) -> Result<(), ProofJobQueueError> {
            self.failed
                .lock()
                .expect("failed poisoned")
                .push((proof_id, reason));
            Ok(())
        }
    }

    /// The canned proof returned by the succeeding mock backend.
    fn proof_data() -> ProofData {
        ProofData::Sp1 {
            proof: Bytes::from_static(&[0xaa, 0xbb]),
            public_values: Bytes::from_static(&[0x01]),
        }
    }

    /// Backend returning canned results without touching RPC or a prover.
    enum MockBackend {
        Ok,
        Fails,
        Panics,
        /// Completes only once `n` jobs rendezvous, proving they ran concurrently.
        Rendezvous(Arc<Barrier>),
    }

    impl ProofJobBackend for MockBackend {
        fn lane(&self) -> ProofBackend {
            ProofBackend::Sp1
        }

        fn start(&self, _request: &ProofRequest) -> anyhow::Result<BackendUpdate> {
            match self {
                Self::Ok => Ok(BackendUpdate::Complete(proof_data())),
                Self::Fails => Err(anyhow::anyhow!("witness generation failed"))
                    .context("building range witness"),
                Self::Panics => panic!("prover exploded"),
                Self::Rendezvous(barrier) => {
                    barrier.wait();
                    Ok(BackendUpdate::Complete(proof_data()))
                }
            }
        }

        fn advance(
            &self,
            _request: &ProofRequest,
            _state: BackendProofState,
        ) -> anyhow::Result<BackendUpdate> {
            anyhow::bail!("mock backend has no durable backend jobs")
        }
    }

    fn request_at(l2_block_number: u64) -> ProofRequest {
        ProofRequest {
            backend: ProofBackend::Sp1,
            game: Address::repeat_byte(0x42),
            root_claim: B256::repeat_byte(0x07),
            l2_block_number,
            l1_head: B256::repeat_byte(0x11),
        }
    }

    fn request() -> ProofRequest {
        request_at(1_200)
    }

    fn config() -> ProofWorkerConfig {
        ProofWorkerConfig {
            poll_interval: Duration::from_millis(10),
            max_concurrent_jobs: 1,
        }
    }

    /// Polls `condition` until it returns true or ~2s elapse.
    async fn wait_for(condition: impl Fn() -> bool) -> bool {
        for _ in 0..200 {
            if condition() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        condition()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn submits_backend_proof() {
        let request = request();
        let queue = MockQueue::with_jobs([request.clone()]);
        let worker = ProofWorker::new(queue.clone(), MockBackend::Ok, config());
        let handle = tokio::spawn(worker);

        assert!(wait_for(|| !queue.submitted().is_empty()).await);
        handle.abort();

        let submitted = queue.submitted();
        assert_eq!(submitted.len(), 1);
        assert_eq!(submitted[0].id, request.id());
        assert_eq!(submitted[0].proof, proof_data());
        assert!(queue.failed().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fails_job_with_error_context_chain() {
        let request = request();
        let queue = MockQueue::with_jobs([request.clone()]);
        let worker = ProofWorker::new(queue.clone(), MockBackend::Fails, config());
        let handle = tokio::spawn(worker);

        assert!(wait_for(|| !queue.failed().is_empty()).await);
        handle.abort();

        let failed = queue.failed();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, request.id());
        assert!(
            failed[0].1.contains("building range witness"),
            "reason: {}",
            failed[0].1
        );
        assert!(
            failed[0].1.contains("witness generation failed"),
            "reason: {}",
            failed[0].1
        );
        assert!(queue.submitted().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fails_job_when_proving_panics() {
        let request = request();
        let queue = MockQueue::with_jobs([request.clone()]);
        let worker = ProofWorker::new(queue.clone(), MockBackend::Panics, config());
        let handle = tokio::spawn(worker);

        assert!(wait_for(|| !queue.failed().is_empty()).await);
        handle.abort();

        let reason = &queue.failed()[0].1;
        assert!(reason.contains("prover exploded"), "reason: {reason}");
        assert!(queue.submitted().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn idles_on_empty_queue() {
        let queue = MockQueue::default();
        let worker = ProofWorker::new(queue.clone(), MockBackend::Ok, config());
        let handle = tokio::spawn(worker);

        tokio::time::sleep(Duration::from_millis(100)).await;
        handle.abort();

        assert!(queue.submitted().is_empty());
        assert!(queue.failed().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn proves_jobs_concurrently_up_to_cap() {
        // Both jobs must be proving at once for the barrier to release; with a cap of 1 this
        // would deadlock and time out.
        let barrier = Arc::new(Barrier::new(2));
        let queue = MockQueue::with_jobs([request_at(1_200), request_at(2_400)]);
        let worker = ProofWorker::new(
            queue.clone(),
            MockBackend::Rendezvous(barrier),
            ProofWorkerConfig {
                max_concurrent_jobs: 2,
                ..config()
            },
        );
        let handle = tokio::spawn(worker);

        assert!(wait_for(|| queue.submitted().len() == 2).await);
        handle.abort();
        assert!(queue.failed().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolves_on_cancellation() {
        let queue = MockQueue::default();
        let worker = ProofWorker::new(queue, MockBackend::Ok, config());
        let token = worker.cancellation_token();
        let handle = tokio::spawn(worker);

        token.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("worker resolves after cancellation")
            .expect("worker task succeeds");
    }
}
