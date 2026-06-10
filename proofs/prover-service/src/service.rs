use crate::{
    config::ProverServiceConfig,
    error::{InvalidConfigError, ProofJobQueueError, ProofRequestError},
    traits::{ProofJobQueue, ProofRequester},
    types::{ProofBackend, ProofData, ProofRequest, ProofRequestId, ProofResponse, ProofStatus},
};
use async_trait::async_trait;
use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::Instant,
};
use tracing::{debug, info, warn};

/// The in-memory state of a single proof job.
#[derive(Debug)]
enum JobState {
    /// Waiting in its backend queue.
    Queued,
    /// Leased to a worker until `lease_deadline`.
    InProgress {
        /// When the lease expires and the job is re-queued.
        lease_deadline: Instant,
    },
    /// Proof available.
    Completed(ProofData),
    /// Permanently failed.
    Failed {
        /// The reason reported on the last attempt.
        reason: String,
    },
}

impl JobState {
    const fn status(&self) -> ProofStatus {
        match self {
            Self::Queued => ProofStatus::Queued,
            Self::InProgress { .. } => ProofStatus::InProgress,
            Self::Completed(_) => ProofStatus::Completed,
            Self::Failed { .. } => ProofStatus::Failed,
        }
    }
}

/// A proof job tracked by the `prover-service`.
#[derive(Debug)]
struct JobEntry {
    request: ProofRequest,
    state: JobState,
    /// Number of leases handed out for this job.
    attempts: u32,
}

/// Mutable service state guarded by a single lock.
#[derive(Debug, Default)]
struct State {
    jobs: HashMap<ProofRequestId, JobEntry>,
    sp1_queue: VecDeque<ProofRequestId>,
    nitro_queue: VecDeque<ProofRequestId>,
    /// Finished (completed or failed) jobs in finishing order,
    /// used to evict the oldest once over capacity.
    finished: VecDeque<ProofRequestId>,
}

impl State {
    fn queue_mut(&mut self, backend: ProofBackend) -> &mut VecDeque<ProofRequestId> {
        match backend {
            ProofBackend::Sp1 => &mut self.sp1_queue,
            ProofBackend::Nitro => &mut self.nitro_queue,
        }
    }

    /// Record a job as finished and evict the oldest finished jobs
    /// beyond `max_finished_jobs`.
    fn finish_job(&mut self, id: ProofRequestId, max_finished_jobs: usize) {
        self.finished.push_back(id);
        while self.finished.len() > max_finished_jobs {
            if let Some(evicted) = self.finished.pop_front() {
                self.jobs.remove(&evicted);
                debug!(id = %evicted, "evicted finished proof job");
            }
        }
    }
}

/// The central orchestration service between defenders and proof
/// generation backends.
///
/// It keeps a per-backend FIFO queue of proof requests and an in-memory
/// proof store. Jobs handed to workers are leased: a job that is neither
/// submitted nor failed before [`ProverServiceConfig::lease_timeout`]
/// elapses is re-queued, until [`ProverServiceConfig::max_attempts`]
/// leases are exhausted and the job is marked as failed.
#[derive(Debug)]
pub struct ProverService {
    config: ProverServiceConfig,
    state: Mutex<State>,
}

impl ProverService {
    /// Create a new `ProverService` with the given configuration.
    pub fn new(config: ProverServiceConfig) -> Result<Self, InvalidConfigError> {
        config.validate()?;
        Ok(Self {
            config,
            state: Mutex::new(State::default()),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("prover-service state mutex poisoned")
    }

    /// Move expired leases for `backend` back to the queue, failing jobs
    /// that exhausted their attempts.
    fn requeue_expired(&self, state: &mut State, backend: ProofBackend, now: Instant) {
        let expired: Vec<ProofRequestId> = state
            .jobs
            .iter()
            .filter(|(_, entry)| {
                entry.request.backend == backend
                    && matches!(
                        entry.state,
                        JobState::InProgress { lease_deadline } if lease_deadline <= now
                    )
            })
            .map(|(id, _)| *id)
            .collect();

        for id in expired {
            let entry = state.jobs.get_mut(&id).expect("job exists");
            if entry.attempts >= self.config.max_attempts {
                warn!(%id, attempts = entry.attempts, "proof job failed: lease expired");
                entry.state = JobState::Failed {
                    reason: format!("lease expired after {} attempts", entry.attempts),
                };
                state.finish_job(id, self.config.max_finished_jobs);
            } else {
                debug!(%id, attempts = entry.attempts, "proof job lease expired, re-queueing");
                entry.state = JobState::Queued;
                state.queue_mut(backend).push_front(id);
            }
        }
    }
}

#[async_trait]
impl ProofRequester for ProverService {
    async fn request_proof(
        &self,
        proof_request: ProofRequest,
    ) -> Result<ProofRequestId, ProofRequestError> {
        let id = proof_request.id();
        let backend = proof_request.backend;
        let mut state = self.lock();

        if let Some(entry) = state.jobs.get_mut(&id) {
            // Re-requesting a failed proof re-queues it; any other state
            // already has the proof queued, running, or available.
            if matches!(entry.state, JobState::Failed { .. }) {
                entry.state = JobState::Queued;
                entry.attempts = 0;
                state.finished.retain(|finished| *finished != id);
                state.queue_mut(backend).push_back(id);
                info!(%id, %backend, "failed proof request re-queued");
            }
            return Ok(id);
        }

        if state.queue_mut(backend).len() >= self.config.max_queue_len {
            return Err(ProofRequestError::QueueFull(backend));
        }

        state.jobs.insert(
            id,
            JobEntry {
                request: proof_request,
                state: JobState::Queued,
                attempts: 0,
            },
        );
        state.queue_mut(backend).push_back(id);
        info!(%id, %backend, "proof request queued");

        Ok(id)
    }

    async fn proof_status(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofStatus, ProofRequestError> {
        let state = self.lock();
        state
            .jobs
            .get(&proof_id)
            .map(|entry| entry.state.status())
            .ok_or(ProofRequestError::NotFound(proof_id))
    }

    async fn get_proof(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofResponse, ProofRequestError> {
        let state = self.lock();
        let entry = state
            .jobs
            .get(&proof_id)
            .ok_or(ProofRequestError::NotFound(proof_id))?;

        match &entry.state {
            JobState::Completed(proof) => Ok(ProofResponse {
                id: proof_id,
                proof: proof.clone(),
            }),
            JobState::Failed { reason } => Err(ProofRequestError::Failed {
                id: proof_id,
                reason: reason.clone(),
            }),
            state => Err(ProofRequestError::Pending {
                id: proof_id,
                status: state.status(),
            }),
        }
    }
}

#[async_trait]
impl ProofJobQueue for ProverService {
    async fn get_next_proof(
        &self,
        backend: ProofBackend,
    ) -> Result<Option<ProofRequest>, ProofJobQueueError> {
        let now = Instant::now();
        let mut state = self.lock();
        self.requeue_expired(&mut state, backend, now);

        while let Some(id) = state.queue_mut(backend).pop_front() {
            // Skip ids whose job was completed, evicted, or re-queued
            // through another path while waiting in the queue.
            let Some(entry) = state.jobs.get_mut(&id) else {
                continue;
            };
            if !matches!(entry.state, JobState::Queued) {
                continue;
            }

            entry.attempts += 1;
            entry.state = JobState::InProgress {
                lease_deadline: now + self.config.lease_timeout,
            };
            debug!(%id, %backend, attempts = entry.attempts, "proof job leased");
            return Ok(Some(entry.request.clone()));
        }

        Ok(None)
    }

    async fn submit_proof(&self, proof: ProofResponse) -> Result<(), ProofJobQueueError> {
        let mut state = self.lock();
        let entry = state
            .jobs
            .get_mut(&proof.id)
            .ok_or(ProofJobQueueError::UnknownJob(proof.id))?;

        // A duplicate submission for an already completed job is a no-op.
        if matches!(entry.state, JobState::Completed(_)) {
            return Ok(());
        }

        if proof.proof.backend() != entry.request.backend {
            return Err(ProofJobQueueError::InvalidProof {
                id: proof.id,
                reason: format!(
                    "backend mismatch: expected {}, got {}",
                    entry.request.backend,
                    proof.proof.backend()
                ),
            });
        }

        // Accept proofs from expired or already failed jobs too: a valid
        // proof is useful no matter how late it arrives.
        let was_queued = matches!(entry.state, JobState::Queued);
        let backend = entry.request.backend;
        entry.state = JobState::Completed(proof.proof);
        if was_queued {
            // The job was re-queued after its lease expired; drop the queue
            // entry so it cannot be leased again or count as queued work.
            state
                .queue_mut(backend)
                .retain(|queued| *queued != proof.id);
        }
        // Give jobs that were already finished (failed) a fresh eviction
        // slot, so the completed proof is not evicted in failure order.
        state.finished.retain(|finished| *finished != proof.id);
        state.finish_job(proof.id, self.config.max_finished_jobs);
        info!(id = %proof.id, "proof job completed");

        Ok(())
    }

    async fn fail_proof(
        &self,
        proof_id: ProofRequestId,
        reason: String,
    ) -> Result<(), ProofJobQueueError> {
        let mut state = self.lock();
        let entry = state
            .jobs
            .get_mut(&proof_id)
            .ok_or(ProofJobQueueError::UnknownJob(proof_id))?;

        // Only an in-progress job can be failed by its worker: queued jobs
        // are already pending a retry and finished jobs keep their outcome.
        if !matches!(entry.state, JobState::InProgress { .. }) {
            return Ok(());
        }

        if entry.attempts >= self.config.max_attempts {
            warn!(%proof_id, attempts = entry.attempts, %reason, "proof job failed");
            entry.state = JobState::Failed { reason };
            state.finish_job(proof_id, self.config.max_finished_jobs);
        } else {
            debug!(%proof_id, attempts = entry.attempts, %reason, "proof attempt failed, re-queueing");
            let backend = entry.request.backend;
            entry.state = JobState::Queued;
            state.queue_mut(backend).push_back(proof_id);
        }

        Ok(())
    }
}
