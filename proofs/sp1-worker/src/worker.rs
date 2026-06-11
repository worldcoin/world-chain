//! Service future leasing SP1 proof jobs from the `prover-service` and submitting results.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use alloy_primitives::B256;
use alloy_sol_types::SolValue;
use futures_util::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
use pin_project::pin_project;
use tokio::{
    sync::{Semaphore, SemaphorePermit},
    task::JoinSet,
    time::Sleep,
};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};
use tracing::{Instrument, info, info_span, warn};
use world_chain_proof_core::artifacts::AggregationProofArtifact;
use world_chain_prover_service::{
    ProofBackend, ProofData, ProofJobQueue, ProofJobQueueError, ProofRequest, ProofResponse,
};

use crate::backend::ValidityProofBackend;

/// Maximum number of proof jobs proving concurrently in this process. A single job already
/// saturates a local CPU prover; the headroom is for backends that parallelize externally
/// (e.g. the Succinct proving network).
const MAX_CONCURRENT_JOBS: usize = 4;

/// Permits gating concurrent proving. A permit is acquired before a job is leased and held
/// until its proof completes, so the worker never leases work it has no capacity to run.
///
/// `static` rather than `const`: a `const` semaphore would be a fresh value at every use
/// site, so its permits would never contend.
static MAX_CONCURRENCY: Semaphore = Semaphore::const_new(MAX_CONCURRENT_JOBS);

type QueueFuture<T> = Pin<Box<dyn Future<Output = Result<T, ProofJobQueueError>> + Send>>;

/// A self-contained report of one job result: performs the `prover-service` round-trip and
/// logs its own outcome.
type ReportFuture = BoxFuture<'static, ()>;

/// A proving result paired with the job it answers.
struct FinishedJob {
    request: ProofRequest,
    result: anyhow::Result<AggregationProofArtifact>,
}

/// Lease sub-state: at most one `getNextProof` request is in flight at a time, and a lease
/// is only started once a concurrency permit is held for the job it may return.
#[pin_project(project = LeaseStateProj)]
enum LeaseState {
    /// Acquire a permit and start a lease on the next poll.
    Ready,
    /// Sleeping out the poll interval after an empty or failed lease attempt.
    Idle(Pin<Box<Sleep>>),
    /// Waiting on `getNextProof`, holding the permit for the prospective job.
    Leasing {
        future: QueueFuture<Option<ProofRequest>>,
        /// Taken exactly once, when the leased job spawns.
        permit: Option<SemaphorePermit<'static>>,
    },
}

impl LeaseState {
    /// Transitions to sleeping out the poll interval before the next lease attempt.
    fn idle(poll_interval: Duration) -> Self {
        Self::Idle(Box::pin(tokio::time::sleep(poll_interval)))
    }

    /// Transitions to leasing the next queued SP1 job.
    fn leasing<Q>(queue: &Arc<Q>, permit: SemaphorePermit<'static>) -> Self
    where
        Q: ProofJobQueue + Send + Sync + 'static,
    {
        let queue = Arc::clone(queue);
        Self::Leasing {
            future: Box::pin(async move { queue.get_next_proof(ProofBackend::Sp1).await }),
            permit: Some(permit),
        }
    }
}

/// Worker that leases SP1 proof jobs from the `prover-service`, proves them with a
/// [`ValidityProofBackend`], and submits the results back.
///
/// The worker is a [`Future`] driving three sources concurrently: a lease loop throttled by
/// [`MAX_CONCURRENCY`], a [`JoinSet`] of proving tasks on the blocking pool, and the
/// in-flight result reports. It composes with other service futures (spawned, `select!`ed,
/// or joined) by the surrounding architecture. Individual job failures are reported to the
/// `prover-service` (which re-queues them until their attempts are exhausted) and never
/// abort the worker.
///
/// The future resolves only when [`Sp1Worker::cancellation_token`] is cancelled: the worker
/// stops leasing, flushes pending reports, and abandons jobs still proving — their leases
/// expire server-side and the jobs are re-queued.
#[pin_project]
pub struct Sp1Worker<Q, B> {
    queue: Arc<Q>,
    backend: Arc<B>,
    poll_interval: Duration,
    cancel: CancellationToken,
    cancelled: Pin<Box<WaitForCancellationFutureOwned>>,
    /// In-flight proving tasks on the blocking pool.
    jobs: JoinSet<FinishedJob>,
    /// In-flight result reports to the `prover-service`.
    reports: FuturesUnordered<ReportFuture>,

    #[pin]
    lease: LeaseState,
}

impl<Q, B> Sp1Worker<Q, B>
where
    Q: ProofJobQueue + Send + Sync + 'static,
    B: ValidityProofBackend,
{
    /// Creates a worker that sleeps `poll_interval` between lease attempts when idle.
    pub fn new(queue: Q, backend: B, poll_interval: Duration) -> Self {
        let cancel = CancellationToken::new();
        let cancelled = Box::pin(cancel.clone().cancelled_owned());
        Self {
            queue: Arc::new(queue),
            backend: Arc::new(backend),
            poll_interval,
            cancel,
            cancelled,
            jobs: JoinSet::new(),
            reports: FuturesUnordered::new(),
            lease: LeaseState::Ready,
        }
    }

    /// Token that gracefully shuts the worker down when cancelled.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }
}

impl<Q, B> Future for Sp1Worker<Q, B>
where
    Q: ProofJobQueue + Send + Sync + 'static,
    B: ValidityProofBackend,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        let shutting_down = this.cancelled.as_mut().poll(cx).is_ready();

        // Turn finished proving tasks into report futures, then drive every report so newly
        // pushed ones register their wakers in this same pass.
        while let Poll::Ready(Some(joined)) = this.jobs.poll_join_next(cx) {
            match joined {
                Ok(FinishedJob { request, result }) => {
                    this.reports.push(report(this.queue, &request, result));
                }
                Err(join_error) => warn!(%join_error, "proving task failed to join"),
            }
        }
        while let Poll::Ready(Some(())) = this.reports.poll_next_unpin(cx) {}

        // Lease new work while concurrency permits are available. The loop only exits
        // through a `break` once an inner future returns `Pending`.
        if !shutting_down {
            loop {
                match this.lease.as_mut().project() {
                    LeaseStateProj::Ready => match MAX_CONCURRENCY.try_acquire() {
                        Ok(permit) => this.lease.set(LeaseState::leasing(this.queue, permit)),
                        // Every permit is proving. A finishing job wakes this task and frees
                        // its permit; the idle backoff is a safety net for permits held
                        // elsewhere in the process.
                        Err(_) => this.lease.set(LeaseState::idle(*this.poll_interval)),
                    },
                    LeaseStateProj::Idle(sleep) => {
                        if sleep.as_mut().poll(cx).is_pending() {
                            break;
                        }
                        this.lease.set(LeaseState::Ready);
                    }
                    LeaseStateProj::Leasing { future, permit } => match future.as_mut().poll(cx) {
                        Poll::Pending => break,
                        Poll::Ready(Ok(Some(request))) => {
                            let permit = permit
                                .take()
                                .expect("leasing state holds its permit until the job spawns");
                            spawn_blocking_on_with_shutdown(
                                this.jobs,
                                this.backend,
                                request,
                                permit,
                            );
                            this.lease.set(LeaseState::Ready);
                        }
                        Poll::Ready(Ok(None)) => {
                            this.lease.set(LeaseState::idle(*this.poll_interval));
                        }
                        Poll::Ready(Err(error)) => {
                            warn!(%error, "failed to lease next proof job");
                            this.lease.set(LeaseState::idle(*this.poll_interval));
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
            info!("sp1-worker shut down");
            return Poll::Ready(());
        }
        Poll::Pending
    }
}

/// Dispatches a leased job to the blocking pool under a per-job tracing span, holding its
/// concurrency permit until the proof completes.
///
/// Panics are contained so a crashed job is failed promptly instead of waiting out its
/// lease. The task is not interruptible once started: on worker shutdown it is abandoned
/// and its result discarded.
fn spawn_blocking_on_with_shutdown<B>(
    jobs: &mut JoinSet<FinishedJob>,
    backend: &Arc<B>,
    request: ProofRequest,
    permit: SemaphorePermit<'static>,
) where
    B: ValidityProofBackend,
{
    let span = info_span!("sp1_job", id = %request.id());
    span.in_scope(|| {
        info!(
            game = %request.game,
            l2_block_number = request.l2_block_number,
            "processing SP1 proof request"
        );
    });

    let backend = Arc::clone(backend);
    jobs.spawn_blocking(move || {
        let _guard = span.enter();
        let _permit = permit;
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| backend.prove(&request)))
                .unwrap_or_else(|panic| {
                    Err(anyhow::anyhow!(
                        "proving panicked: {}",
                        panic_message(&*panic)
                    ))
                });
        FinishedJob { request, result }
    });
}

/// Builds the future that reports one finished job (submit or fail) and logs the outcome.
fn report<Q>(
    queue: &Arc<Q>,
    request: &ProofRequest,
    result: anyhow::Result<AggregationProofArtifact>,
) -> ReportFuture
where
    Q: ProofJobQueue + Send + Sync + 'static,
{
    let id = request.id();
    let span = info_span!("sp1_job", %id);
    let queue = Arc::clone(queue);

    match job_outcome(request, result) {
        Ok(artifact) => {
            let response = ProofResponse {
                id,
                proof: ProofData::Sp1 {
                    proof: artifact.proof.into(),
                    public_values: artifact.outputs.abi_encode().into(),
                },
            };
            Box::pin(
                async move {
                    match queue.submit_proof(response).await {
                        Ok(()) => info!("proof submitted"),
                        // The lease expires server-side and the job is re-queued.
                        Err(error) => warn!(%error, "failed to submit proof"),
                    }
                }
                .instrument(span),
            )
        }
        Err(reason) => Box::pin(
            async move {
                warn!(%reason, "proving failed");
                if let Err(error) = queue.fail_proof(id, reason).await {
                    warn!(%error, "failed to report proving failure");
                }
            }
            .instrument(span),
        ),
    }
}

/// Flattens a proving result into the artifact to submit or the failure reason to report.
fn job_outcome(
    request: &ProofRequest,
    result: anyhow::Result<AggregationProofArtifact>,
) -> Result<AggregationProofArtifact, String> {
    // `{:#}` renders the full anyhow context chain into the failure reason.
    let artifact = result.map_err(|error| format!("{error:#}"))?;
    check_artifact(request, &artifact).map_err(|mismatch| mismatch.to_string())?;
    Ok(artifact)
}

/// A proof artifact whose committed outputs do not defend the requested root.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
enum ArtifactMismatch {
    #[error("aggregation post root {actual:?} does not match root claim {expected:?}")]
    PostRoot { expected: B256, actual: B256 },
    #[error("aggregation block number {actual} does not match request {expected}")]
    BlockNumber { expected: u64, actual: u64 },
    #[error("aggregation l1 head {actual:?} does not match request {expected:?}")]
    L1Head { expected: B256, actual: B256 },
}

/// Checks that the artifact's committed outputs defend exactly the requested root.
fn check_artifact(
    request: &ProofRequest,
    artifact: &AggregationProofArtifact,
) -> Result<(), ArtifactMismatch> {
    let outputs = &artifact.outputs;
    if outputs.l2PostRoot != request.root_claim {
        return Err(ArtifactMismatch::PostRoot {
            expected: request.root_claim,
            actual: outputs.l2PostRoot,
        });
    }
    if outputs.l2BlockNumber != request.l2_block_number {
        return Err(ArtifactMismatch::BlockNumber {
            expected: request.l2_block_number,
            actual: outputs.l2BlockNumber,
        });
    }
    if outputs.l1Head != request.l1_head {
        return Err(ArtifactMismatch::L1Head {
            expected: request.l1_head,
            actual: outputs.l1Head,
        });
    }
    Ok(())
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

    use alloy_primitives::{Address, B256};
    use anyhow::Context as _;
    use async_trait::async_trait;
    use world_chain_proof_core::types::AggregationOutputs;
    use world_chain_prover_service::ProofRequestId;

    use super::*;

    /// In-memory queue with shared interior so tests keep a handle for assertions.
    #[derive(Clone, Default)]
    struct MockQueue {
        jobs: Arc<Mutex<VecDeque<ProofRequest>>>,
        submitted: Arc<Mutex<Vec<ProofResponse>>>,
        failed: Arc<Mutex<Vec<(ProofRequestId, String)>>>,
    }

    impl MockQueue {
        fn with_jobs(jobs: impl IntoIterator<Item = ProofRequest>) -> Self {
            Self {
                jobs: Arc::new(Mutex::new(jobs.into_iter().collect())),
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
        ) -> Result<Option<ProofRequest>, ProofJobQueueError> {
            Ok(self.jobs.lock().expect("jobs poisoned").pop_front())
        }

        async fn submit_proof(&self, proof: ProofResponse) -> Result<(), ProofJobQueueError> {
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
        ) -> Result<(), ProofJobQueueError> {
            self.failed
                .lock()
                .expect("failed poisoned")
                .push((proof_id, reason));
            Ok(())
        }
    }

    /// Backend returning canned artifacts without touching RPC or SP1.
    enum MockBackend {
        Valid,
        WrongRoot,
        Fails,
        /// Completes only once `n` jobs rendezvous, proving they ran concurrently.
        Rendezvous(Arc<Barrier>),
    }

    impl ValidityProofBackend for MockBackend {
        fn prove(&self, request: &ProofRequest) -> anyhow::Result<AggregationProofArtifact> {
            match self {
                Self::Valid => Ok(artifact_for(request, request.root_claim)),
                Self::WrongRoot => Ok(artifact_for(request, B256::repeat_byte(0x99))),
                Self::Fails => Err(anyhow::anyhow!("witness generation failed"))
                    .context("building range witness"),
                Self::Rendezvous(barrier) => {
                    barrier.wait();
                    Ok(artifact_for(request, request.root_claim))
                }
            }
        }
    }

    fn artifact_for(request: &ProofRequest, post_root: B256) -> AggregationProofArtifact {
        AggregationProofArtifact {
            outputs: AggregationOutputs {
                l1Head: request.l1_head,
                l2PreRoot: B256::repeat_byte(0x01),
                l2PostRoot: post_root,
                l2BlockNumber: request.l2_block_number,
                rollupConfigHash: B256::repeat_byte(0x02),
                multiBlockVKey: B256::repeat_byte(0x03),
                proverAddress: Address::ZERO,
            },
            proof: vec![0xaa, 0xbb],
        }
    }

    fn request() -> ProofRequest {
        request_at(1_200)
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

    const POLL_INTERVAL: Duration = Duration::from_millis(10);

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
    async fn submits_matching_proof() {
        let request = request();
        let queue = MockQueue::with_jobs([request.clone()]);
        let worker = Sp1Worker::new(queue.clone(), MockBackend::Valid, POLL_INTERVAL);
        let handle = tokio::spawn(worker);

        assert!(wait_for(|| !queue.submitted().is_empty()).await);
        handle.abort();

        let submitted = queue.submitted();
        assert_eq!(submitted.len(), 1);
        assert_eq!(submitted[0].id, request.id());
        let ProofData::Sp1 {
            proof,
            public_values,
        } = &submitted[0].proof
        else {
            panic!("expected SP1 proof data");
        };
        assert_eq!(proof.as_ref(), [0xaa, 0xbb]);
        let expected = artifact_for(&request, request.root_claim)
            .outputs
            .abi_encode();
        assert_eq!(public_values.as_ref(), expected);
        assert!(queue.failed().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fails_job_on_root_claim_mismatch() {
        let request = request();
        let queue = MockQueue::with_jobs([request.clone()]);
        let worker = Sp1Worker::new(queue.clone(), MockBackend::WrongRoot, POLL_INTERVAL);
        let handle = tokio::spawn(worker);

        assert!(wait_for(|| !queue.failed().is_empty()).await);
        handle.abort();

        let failed = queue.failed();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, request.id());
        assert!(failed[0].1.contains("post root"), "reason: {}", failed[0].1);
        assert!(queue.submitted().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fails_job_with_error_context_chain() {
        let request = request();
        let queue = MockQueue::with_jobs([request.clone()]);
        let worker = Sp1Worker::new(queue.clone(), MockBackend::Fails, POLL_INTERVAL);
        let handle = tokio::spawn(worker);

        assert!(wait_for(|| !queue.failed().is_empty()).await);
        handle.abort();

        let reason = &queue.failed()[0].1;
        assert!(
            reason.contains("building range witness"),
            "reason: {reason}"
        );
        assert!(
            reason.contains("witness generation failed"),
            "reason: {reason}"
        );
        assert!(queue.submitted().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fails_job_when_proving_panics() {
        struct PanickingBackend;
        impl ValidityProofBackend for PanickingBackend {
            fn prove(&self, _request: &ProofRequest) -> anyhow::Result<AggregationProofArtifact> {
                panic!("prover exploded");
            }
        }

        let request = request();
        let queue = MockQueue::with_jobs([request.clone()]);
        let worker = Sp1Worker::new(queue.clone(), PanickingBackend, POLL_INTERVAL);
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
        let worker = Sp1Worker::new(queue.clone(), MockBackend::Valid, POLL_INTERVAL);
        let handle = tokio::spawn(worker);

        tokio::time::sleep(Duration::from_millis(100)).await;
        handle.abort();

        assert!(queue.submitted().is_empty());
        assert!(queue.failed().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn proves_jobs_concurrently() {
        // Both jobs must be proving at once for the barrier to release; a serial worker
        // would deadlock and time out.
        let barrier = Arc::new(Barrier::new(2));
        let queue = MockQueue::with_jobs([request_at(1_200), request_at(2_400)]);
        let worker = Sp1Worker::new(
            queue.clone(),
            MockBackend::Rendezvous(barrier),
            POLL_INTERVAL,
        );
        let handle = tokio::spawn(worker);

        assert!(wait_for(|| queue.submitted().len() == 2).await);
        handle.abort();
        assert!(queue.failed().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolves_on_cancellation() {
        let queue = MockQueue::default();
        let worker = Sp1Worker::new(queue, MockBackend::Valid, POLL_INTERVAL);
        let token = worker.cancellation_token();
        let handle = tokio::spawn(worker);

        token.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("worker resolves after cancellation")
            .expect("worker task succeeds");
    }
}
