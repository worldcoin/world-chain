//! Service future leasing SP1 proof jobs from the `prover-service` and submitting results.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, ready},
    time::Duration,
};

use alloy_sol_types::SolValue;
use pin_project::pin_project;
use tokio::{task::JoinHandle, time::Sleep};
use tracing::{info, warn};
use world_chain_proof_core::artifacts::AggregationProofArtifact;
use world_chain_prover_service::{
    ProofBackend, ProofData, ProofJobQueue, ProofJobQueueError, ProofRequest, ProofRequestId,
    ProofResponse,
};

use crate::backend::ValidityProofBackend;

type QueueFuture<T> = Pin<Box<dyn Future<Output = Result<T, ProofJobQueueError>> + Send>>;

/// State machine driven by the worker's [`Future`] implementation.
#[pin_project(project = WorkerStateProj)]
enum WorkerState {
    /// Sleeping before the next lease attempt.
    Idle(#[pin] Sleep),
    /// Leasing the next queued job from the `prover-service`.
    Leasing(QueueFuture<Option<ProofRequest>>),
    /// Proving the leased job on a blocking thread.
    Proving {
        request: ProofRequest,
        handle: JoinHandle<anyhow::Result<AggregationProofArtifact>>,
    },
    /// Reporting the job result (submit or fail) to the `prover-service`.
    Reporting {
        id: ProofRequestId,
        submitted: bool,
        future: QueueFuture<()>,
    },
}

/// Worker that leases SP1 proof jobs from the `prover-service`, proves them with a
/// [`ValidityProofBackend`], and submits the results back.
///
/// The worker is a [`Future`] that never resolves: each poll advances a
/// lease → prove → report state machine, so it can be composed with other service futures
/// (spawned, `select!`ed, or joined) by the surrounding architecture. Individual job failures
/// are reported to the `prover-service` (which re-queues them until their attempts are
/// exhausted) and never abort the worker.
#[pin_project]
pub struct Sp1Worker<Q, B> {
    queue: Arc<Q>,
    backend: Arc<B>,
    poll_interval: Duration,
    #[pin]
    state: WorkerState,
}

impl<Q, B> Sp1Worker<Q, B>
where
    Q: ProofJobQueue + Send + Sync + 'static,
    B: ValidityProofBackend,
{
    /// Creates a worker that sleeps `poll_interval` between lease attempts when idle.
    pub fn new(queue: Q, backend: B, poll_interval: Duration) -> Self {
        let queue = Arc::new(queue);
        let state = WorkerState::Leasing(lease(Arc::clone(&queue)));
        Self {
            queue,
            backend: Arc::new(backend),
            poll_interval,
            state,
        }
    }

    /// Drives the worker until the process exits.
    pub async fn run(self) {
        self.await
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
        loop {
            match this.state.as_mut().project() {
                WorkerStateProj::Idle(sleep) => {
                    ready!(sleep.poll(cx));
                    this.state
                        .set(WorkerState::Leasing(lease(Arc::clone(this.queue))));
                }
                WorkerStateProj::Leasing(future) => match ready!(future.as_mut().poll(cx)) {
                    Ok(Some(request)) => {
                        info!(
                            id = %request.id(),
                            game = %request.game,
                            l2_block_number = request.l2_block_number,
                            "processing SP1 proof request"
                        );
                        let backend = Arc::clone(this.backend);
                        let job = request.clone();
                        let handle = tokio::task::spawn_blocking(move || backend.prove(&job));
                        this.state.set(WorkerState::Proving { request, handle });
                    }
                    Ok(None) => {
                        this.state
                            .set(WorkerState::Idle(tokio::time::sleep(*this.poll_interval)));
                    }
                    Err(error) => {
                        warn!(%error, "failed to lease next proof job");
                        this.state
                            .set(WorkerState::Idle(tokio::time::sleep(*this.poll_interval)));
                    }
                },
                WorkerStateProj::Proving { request, handle } => {
                    let joined = ready!(Pin::new(handle).poll(cx));
                    let result = joined
                        .map_err(|join_error| {
                            anyhow::anyhow!("proving task panicked: {join_error}")
                        })
                        .and_then(|result| result);
                    let next = report(Arc::clone(this.queue), request, result);
                    this.state.set(next);
                }
                WorkerStateProj::Reporting {
                    id,
                    submitted,
                    future,
                } => {
                    match ready!(future.as_mut().poll(cx)) {
                        Ok(()) if *submitted => info!(%id, "proof submitted"),
                        // Failure reasons are logged when the report is built.
                        Ok(()) => {}
                        // The lease expires server-side and the job is re-queued.
                        Err(error) => warn!(%id, %error, "failed to report proof result"),
                    }
                    // More work may be queued behind this job; lease again immediately.
                    this.state
                        .set(WorkerState::Leasing(lease(Arc::clone(this.queue))));
                }
            }
        }
    }
}

/// Future leasing the next queued SP1 job.
fn lease<Q>(queue: Arc<Q>) -> QueueFuture<Option<ProofRequest>>
where
    Q: ProofJobQueue + Send + Sync + 'static,
{
    Box::pin(async move { queue.get_next_proof(ProofBackend::Sp1).await })
}

/// Builds the reporting transition for a finished proving attempt.
fn report<Q>(
    queue: Arc<Q>,
    request: &ProofRequest,
    result: anyhow::Result<AggregationProofArtifact>,
) -> WorkerState
where
    Q: ProofJobQueue + Send + Sync + 'static,
{
    let id = request.id();

    // `{:#}` renders the full anyhow context chain into the failure reason.
    let outcome = result
        .map_err(|error| format!("{error:#}"))
        .and_then(|artifact| check_artifact(request, &artifact).map(|()| artifact));

    match outcome {
        Ok(artifact) => {
            let public_values = artifact.outputs.abi_encode();
            let response = ProofResponse {
                id,
                proof: ProofData::Sp1 {
                    proof: artifact.proof.into(),
                    public_values: public_values.into(),
                },
            };
            WorkerState::Reporting {
                id,
                submitted: true,
                future: Box::pin(async move { queue.submit_proof(response).await }),
            }
        }
        Err(reason) => {
            warn!(%id, %reason, "proving failed");
            WorkerState::Reporting {
                id,
                submitted: false,
                future: Box::pin(async move { queue.fail_proof(id, reason).await }),
            }
        }
    }
}

/// Checks that the artifact's committed outputs defend exactly the requested root.
fn check_artifact(
    request: &ProofRequest,
    artifact: &AggregationProofArtifact,
) -> Result<(), String> {
    let outputs = &artifact.outputs;
    if outputs.l2PostRoot != request.root_claim {
        return Err(format!(
            "aggregation post root {:?} does not match root claim {:?}",
            outputs.l2PostRoot, request.root_claim
        ));
    }
    if outputs.l2BlockNumber != request.l2_block_number {
        return Err(format!(
            "aggregation block number {} does not match request {}",
            outputs.l2BlockNumber, request.l2_block_number
        ));
    }
    if outputs.l1Head != request.l1_head {
        return Err(format!(
            "aggregation l1 head {:?} does not match request {:?}",
            outputs.l1Head, request.l1_head
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use alloy_primitives::{Address, B256};
    use anyhow::Context as _;
    use async_trait::async_trait;
    use world_chain_proof_core::types::AggregationOutputs;

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
    }

    impl ValidityProofBackend for MockBackend {
        fn prove(&self, request: &ProofRequest) -> anyhow::Result<AggregationProofArtifact> {
            match self {
                Self::Valid => Ok(artifact_for(request, request.root_claim)),
                Self::WrongRoot => Ok(artifact_for(request, B256::repeat_byte(0x99))),
                Self::Fails => Err(anyhow::anyhow!("witness generation failed"))
                    .context("building range witness"),
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
        ProofRequest {
            backend: ProofBackend::Sp1,
            game: Address::repeat_byte(0x42),
            root_claim: B256::repeat_byte(0x07),
            l2_block_number: 1_200,
            l1_head: B256::repeat_byte(0x11),
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
    async fn submits_matching_proof() {
        let request = request();
        let queue = MockQueue::with_jobs([request.clone()]);
        let worker = Sp1Worker::new(queue.clone(), MockBackend::Valid, Duration::from_millis(10));
        let handle = tokio::spawn(worker.run());

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
        let worker = Sp1Worker::new(
            queue.clone(),
            MockBackend::WrongRoot,
            Duration::from_millis(10),
        );
        let handle = tokio::spawn(worker.run());

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
        let worker = Sp1Worker::new(queue.clone(), MockBackend::Fails, Duration::from_millis(10));
        let handle = tokio::spawn(worker.run());

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
    async fn idles_on_empty_queue() {
        let queue = MockQueue::default();
        let worker = Sp1Worker::new(queue.clone(), MockBackend::Valid, Duration::from_millis(10));
        let handle = tokio::spawn(worker.run());

        tokio::time::sleep(Duration::from_millis(100)).await;
        handle.abort();

        assert!(queue.submitted().is_empty());
        assert!(queue.failed().is_empty());
    }
}
