//! The backend abstraction the worker dispatches claimed jobs to.

use std::sync::Arc;

use async_trait::async_trait;
use world_chain_prover_service::{
    BackendSession, BackendSessionStatus, LockId, ProofBackend, ProofData, ProofJobQueue,
    ProofJobQueueError, ProofRequest, ProofRequestId, SessionType,
};

/// Session bookkeeping handle pre-bound to one claimed job's identifiers.
///
/// A backend uses this to checkpoint and resume long-running external proving work (for
/// example an SP1 range proof followed by an aggregation proof) so a worker restart does not
/// re-run completed phases. It wraps the worker's queue as a trait object, pre-binding the
/// proof id, worker id, and lock id so the backend never re-plumbs them.
#[derive(Clone)]
pub struct JobSessions {
    queue: Arc<dyn ProofJobQueue + Send + Sync>,
    proof_id: ProofRequestId,
    worker_id: String,
    lock_id: LockId,
}

impl JobSessions {
    /// Create a handle bound to the claimed job's identifiers.
    #[must_use]
    pub fn new(
        queue: Arc<dyn ProofJobQueue + Send + Sync>,
        proof_id: ProofRequestId,
        worker_id: String,
        lock_id: LockId,
    ) -> Self {
        Self {
            queue,
            proof_id,
            worker_id,
            lock_id,
        }
    }

    /// Fetch the active backend session of the given type, if any.
    pub async fn get(
        &self,
        session_type: SessionType,
    ) -> Result<Option<BackendSession>, ProofJobQueueError> {
        self.queue
            .get_proof_session(self.proof_id, session_type)
            .await
    }

    /// Record (insert or update) a backend session for this job.
    pub async fn record(
        &self,
        session_type: SessionType,
        backend_session_id: String,
        status: BackendSessionStatus,
        failure_reason: String,
    ) -> Result<(), ProofJobQueueError> {
        self.queue
            .record_proof_session(
                self.proof_id,
                session_type,
                self.worker_id.clone(),
                self.lock_id,
                backend_session_id,
                status,
                failure_reason,
            )
            .await
    }
}

impl std::fmt::Debug for JobSessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobSessions")
            .field("proof_id", &self.proof_id)
            .field("worker_id", &self.worker_id)
            .field("lock_id", &self.lock_id)
            .finish_non_exhaustive()
    }
}

/// A proof job claimed from the `prover-service`, handed to a [`ClaimedProofJobHandler`].
#[derive(Debug)]
pub struct ProofJob {
    /// The user-facing proof request to prove.
    pub request: ProofRequest,
    /// Token authorizing durable updates to the claimed row.
    pub lock_id: LockId,
    /// Id of the worker that claimed the job.
    pub worker_id: String,
    /// Durable session bookkeeping for resuming external proving work.
    pub sessions: JobSessions,
}

/// Handles a claimed proof job end to end for one [`ProofBackend`] lane.
///
/// The handler owns the entire proving workflow for a claimed job: building witnesses,
/// driving the prover (optionally checkpointing progress through [`ProofJob::sessions`]),
/// and validating that the result defends the requested root. It returns the finished
/// [`ProofData`]; the worker submits it to the `prover-service`.
///
/// `handle_claimed_job` may be long-running. It must finish within the service lock
/// timeout: there is no lock-renewal API, so a job that outlives its lock fails to submit
/// and is re-queued.
#[async_trait]
pub trait ClaimedProofJobHandler: Send + Sync + 'static {
    /// The queue lane this handler claims jobs from.
    fn lane(&self) -> ProofBackend;

    /// Returns whether this worker should attempt to claim a job now.
    ///
    /// Defaults to always ready. Backends with external capacity limits can gate claiming
    /// here so the worker backs off instead of locking work it cannot start.
    async fn ready_to_claim(&self, _worker_id: &str) -> bool {
        true
    }

    /// Handles a claimed proof job, returning the finished proof.
    async fn handle_claimed_job(&self, job: ProofJob) -> anyhow::Result<ProofData>;

    /// Signals backend-specific spawned work to stop during shutdown.
    fn shutdown(&self) {}
}
