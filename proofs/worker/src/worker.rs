//! Service polling the `prover-service` for claimed proof jobs and submitting results.

use backon::Retryable;
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, info, info_span, warn};
use world_chain_prover_service::{
    LockedProofRequest, ProofJobQueue, ProofJobQueueError, SucceededProofResponse,
};

use crate::{
    backend::{ClaimedProofJobHandler, JobSessions, ProofJob},
    heartbeat::{WorkerHeartbeat, WorkerHeartbeatConfig},
    retry::RetryConfig,
};

/// Default number of jobs proving concurrently. One is right for a local CPU prover, which a
/// single job already saturates; raise it for backends that parallelize externally (the
/// Succinct proving network) or that are cheap and local (TEE attestation).
pub const DEFAULT_MAX_CONCURRENT_JOBS: usize = 1;

/// Default sleep between claim attempts when no work is available.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Configuration for a [`ProofWorker`].
#[derive(Clone, Debug)]
pub struct ProofWorkerConfig {
    /// The worker id.
    pub worker_id: String,
    /// Sleep between claim attempts when no work is available.
    pub poll_interval: Duration,
    /// Maximum number of jobs this worker proves concurrently. Per-worker, not global, so an
    /// SP1 worker and a TEE worker in the same process throttle independently.
    pub max_concurrent_jobs: usize,
    /// Configurations for retry.
    pub retry_config: RetryConfig,
    /// Configurations for worker heartbeat.
    pub heartbeat_config: WorkerHeartbeatConfig,
}

impl ProofWorkerConfig {
    /// Create a new `ProofWorkerConfig` with the provided `worker_id`,
    /// `poll_interval`, `max_concurrent_jobs`, `retry_config` and `heartbeat_config`.
    #[must_use]
    pub fn new(
        worker_id: String,
        poll_interval: Duration,
        max_concurrent_jobs: usize,
        retry_config: RetryConfig,
        heartbeat_config: WorkerHeartbeatConfig,
    ) -> Self {
        Self {
            worker_id,
            poll_interval,
            max_concurrent_jobs,
            retry_config,
            heartbeat_config,
        }
    }
}

/// Worker that polls one backend lane of the `prover-service` for claimed proof jobs, hands
/// each to a [`ClaimedProofJobHandler`], and submits the finished proofs back.
///
/// The worker runs a single loop: it acquires a per-worker concurrency permit, claims the
/// next job for its lane, and spawns a task that drives the handler to completion and submits
/// the result. With the default concurrency of one this is fully serial; raising it lets
/// backends that parallelize externally prove several jobs at once. It is backend-agnostic,
/// so a process can run one instance per lane (an SP1 worker, a TEE worker), each its own
/// future with independent config and concurrency, composed with `join!`/`select!`.
///
/// ```text
/// PROVER-SERVICE --getNextProof(lane)--> PROOF-WORKER --handle_claimed_job--> BACKEND
///        ^                                                                       |
///        +-------------------- submitProof <-------------------------- ProofData +
/// ```
///
/// Individual job failures are logged and never abort the worker: the job's lease expires
/// server-side and the request is re-queued. The future resolves only when
/// [`ProofWorker::cancellation_token`] is cancelled: the worker stops claiming, signals the
/// backend to shut down, and abandons jobs still proving — their leases expire and re-queue.
pub struct ProofWorker {
    cancel: CancellationToken,
    run: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl ProofWorker {
    /// Creates a worker claiming `backend`'s lane from `queue`.
    pub fn new<Q, B>(queue: Q, backend: B, config: ProofWorkerConfig) -> Self
    where
        Q: ProofJobQueue + Send + Sync + 'static,
        B: ClaimedProofJobHandler,
    {
        let cancel = CancellationToken::new();
        let run = Box::pin(run_worker(
            Arc::new(queue),
            Arc::new(backend),
            config,
            cancel.clone(),
        ));
        Self { cancel, run }
    }

    /// Token that gracefully shuts the worker down when cancelled.
    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }
}

impl Future for ProofWorker {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.run.as_mut().poll(cx)
    }
}

/// Drives the poll/claim/handle/submit loop until cancelled.
async fn run_worker<Q, B>(
    queue: Arc<Q>,
    backend: Arc<B>,
    config: ProofWorkerConfig,
    cancel: CancellationToken,
) where
    Q: ProofJobQueue + Send + Sync + 'static,
    B: ClaimedProofJobHandler,
{
    let lane = backend.lane();
    let concurrency = Arc::new(Semaphore::new(config.max_concurrent_jobs.max(1)));
    let mut jobs: JoinSet<()> = JoinSet::new();

    loop {
        if cancel.is_cancelled() {
            break;
        }

        // Reap finished tasks so the set does not retain their handles unbounded.
        while let Some(joined) = jobs.try_join_next() {
            if let Err(join_error) = joined {
                warn!(%join_error, "proving task failed to join");
            }
        }

        // A backend at external capacity asks the worker to back off rather than lock work
        // it cannot start.
        if !backend.ready_to_claim(&config.worker_id).await {
            if sleep_or_cancel(&cancel, config.poll_interval).await {
                break;
            }
            continue;
        }

        // Hold a permit before claiming so a claimed job always has capacity to run.
        let permit = tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            permit = Arc::clone(&concurrency).acquire_owned() => {
                permit.expect("worker semaphore is never closed")
            }
        };

        let claimed = tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            claimed = queue.get_next_proof(lane, config.worker_id.clone()) => claimed,
        };

        match claimed {
            Ok(Some(locked)) => {
                spawn_job(
                    &mut jobs,
                    &queue,
                    &backend,
                    &config.worker_id,
                    locked,
                    permit,
                    config.retry_config,
                    config.heartbeat_config,
                );
            }
            Ok(None) => {
                drop(permit);
                if sleep_or_cancel(&cancel, config.poll_interval).await {
                    break;
                }
            }
            Err(error) => {
                warn!(%error, "failed to claim next proof job");
                drop(permit);
                if sleep_or_cancel(&cancel, config.poll_interval).await {
                    break;
                }
            }
        }
    }

    backend.shutdown();
    if !jobs.is_empty() {
        warn!(
            jobs = jobs.len(),
            "abandoning in-flight proving jobs; their leases will expire and re-queue"
        );
    }
    // Dropping the set aborts in-flight proving tasks; their server-side leases expire.
    drop(jobs);
    info!("proof worker shut down");
}

/// Spawns a task that drives the handler for one claimed job and submits its result,
/// holding the concurrency permit until the job finishes.
fn spawn_job<Q, B>(
    jobs: &mut JoinSet<()>,
    queue: &Arc<Q>,
    backend: &Arc<B>,
    worker_id: &str,
    locked: LockedProofRequest,
    permit: OwnedSemaphorePermit,
    retry_config: RetryConfig,
    worker_heartbeat_config: WorkerHeartbeatConfig,
) where
    Q: ProofJobQueue + Send + Sync + 'static,
    B: ClaimedProofJobHandler,
{
    let LockedProofRequest { request, lock_id } = locked;
    let proof_id = request.id();
    let lane = request.backend;
    let span = info_span!("proof_job", id = %proof_id, lane = %lane);
    span.in_scope(|| {
        info!(
            game = %request.game,
            l2_block_number = request.l2_block_number,
            "handling claimed proof job"
        );
    });

    let queue = Arc::clone(queue);
    let backend = Arc::clone(backend);
    let worker_id = worker_id.to_string();
    let session_queue: Arc<dyn ProofJobQueue + Send + Sync> = queue.clone();

    jobs.spawn(
        async move {
            let _permit = permit;
            let sessions = JobSessions::new(session_queue, proof_id, worker_id.clone(), lock_id);
            let job = ProofJob {
                request,
                lock_id,
                worker_id: worker_id.clone(),
                sessions,
            };

            let worker_heartbeat = WorkerHeartbeat::new(
                proof_id,
                worker_id.clone(),
                lock_id,
                worker_heartbeat_config,
                queue.clone(),
            );
            let heartbeat = worker_heartbeat.run_until_failure();
            tokio::pin!(heartbeat);

            let proof_result = tokio::select! {
                biased;
                result = backend.handle_claimed_job(job) => result,
                lease_lost = &mut heartbeat => {
                    warn!(%lease_lost, "heartbeat failed, cancelling proof job");
                    return;
                }
            };

            let proof = match proof_result {
                Ok(proof) => proof,
                // `{:#}` renders the full anyhow context chain into the failure reason.
                Err(error) => {
                    warn!(
                        reason = %format!("{error:#}"),
                        "proving failed, lease will expire and re-queue"
                    );
                    return;
                }
            };

            let response = SucceededProofResponse {
                id: proof_id,
                proof,
            };

            let submit_proof = async {
                (|| async {
                    queue
                        .submit_proof(response.clone(), worker_id.clone(), lock_id)
                        .await
                })
                .retry(retry_config.to_backoff_builder())
                .when(|err| err.is_retryable())
                .notify(|error, delay| {
                    warn!(%error, ?delay, "failed to submit proof, retrying");
                })
                .await
            };

            let submit_result = tokio::select! {
                biased;
                submit_result = submit_proof => submit_result,
                lease_lost = &mut heartbeat => {
                    if matches!(lease_lost, ProofJobQueueError::AlreadyTerminal(_)) {
                        // When the submit_proof work submits the proof, it changes the job status to succeeded.
                        // If shortly after that, the heartbeat tries to extend the lock, the heartbeat will fail
                        // becasue the job status is not claimed anymore, therefore returns `AlreadyTerminal`.
                        // In this case, try the `submit_proof` idempotent operation again. This should return
                        // Ok(()) if everything is fine.
                        (|| async {
                            queue
                                .submit_proof(response.clone(), worker_id.clone(), lock_id)
                                .await
                        })
                        .retry(retry_config.to_backoff_builder())
                        .when(|err| err.is_retryable())
                        .notify(|error, delay| {
                            warn!(%error, ?delay, "failed to submit proof, retrying");
                        })
                        .await
                    } else {
                        warn!(%lease_lost, "heartbeat failed, cancelling proof job");
                        return;
                    }
                }
            };

            match submit_result {
                Ok(()) => info!("proof submitted"),
                Err(error) => warn!(
                    %error,
                    "failed to submit proof after retries, lease will expire and re-queue"
                ),
            }
        }
        .instrument(span),
    );
}

/// Sleeps for `dur` unless cancelled first. Returns `true` if cancellation won the race.
async fn sleep_or_cancel(cancel: &CancellationToken, dur: Duration) -> bool {
    tokio::select! {
        biased;
        () = cancel.cancelled() => true,
        () = tokio::time::sleep(dur) => false,
    }
}
