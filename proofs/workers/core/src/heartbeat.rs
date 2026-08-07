use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;
use world_chain_prover_service::{
    HeartbeatRequest, LockId, ProofJobQueue, ProofJobQueueError, ProofRequestId,
};

/// Minimum proof-generation heartbeat interval.
pub const MIN_WORKER_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(1);

/// Default interval between worker API heartbeats while a proof is being generated.
pub const DEFAULT_WORKER_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Default maximum consecutive retryable heartbeat failures before aborting generation.
pub const DEFAULT_WORKER_MAX_CONSECUTIVE_HEARTBEAT_FAILURES: u32 = 5;

/// Heartbeat settings used while a worker is generating a proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerHeartbeatConfig {
    /// Delay between heartbeat attempts.
    pub interval: Duration,
    /// Maximum consecutive retryable heartbeat failures before aborting proof generation.
    pub max_consecutive_failures: u32,
}

impl WorkerHeartbeatConfig {
    /// Creates a heartbeat config.
    pub const fn new(interval: Duration) -> Self {
        Self::with_max_consecutive_failures(
            interval,
            DEFAULT_WORKER_MAX_CONSECUTIVE_HEARTBEAT_FAILURES,
        )
    }

    /// Creates a heartbeat config with an explicit retryable failure limit.
    pub const fn with_max_consecutive_failures(
        interval: Duration,
        max_consecutive_failures: u32,
    ) -> Self {
        Self {
            interval,
            max_consecutive_failures,
        }
    }

    /// Returns the configured interval clamped to the minimum allowed delay.
    pub fn normalized_interval(&self) -> Duration {
        self.interval.max(MIN_WORKER_HEARTBEAT_INTERVAL)
    }

    /// Returns the configured retryable failure limit clamped to at least one.
    pub const fn normalized_max_consecutive_failures(&self) -> u32 {
        if self.max_consecutive_failures == 0 {
            1
        } else {
            self.max_consecutive_failures
        }
    }
}

impl Default for WorkerHeartbeatConfig {
    fn default() -> Self {
        Self::new(DEFAULT_WORKER_HEARTBEAT_INTERVAL)
    }
}

/// Worker heartbeat delivery.
#[derive(Debug)]
pub struct WorkerHeartbeat<Q> {
    proof_id: ProofRequestId,
    worker_id: String,
    lock_id: LockId,
    config: WorkerHeartbeatConfig,
    queue: Q,
}

impl<Q: ProofJobQueue> WorkerHeartbeat<Q> {
    /// Create a new [WorkerHeartbeat] with the provided proof_id,
    ///  worker_id, lock_id, config and queue.
    pub fn new(
        proof_id: ProofRequestId,
        worker_id: String,
        lock_id: LockId,
        config: WorkerHeartbeatConfig,
        queue: Q,
    ) -> Self {
        Self {
            proof_id,
            worker_id,
            lock_id,
            config,
            queue,
        }
    }

    /// Sends heartbeats until a non-recoverable heartbeat failure occurs.
    pub async fn run_until_failure(&self) -> ProofJobQueueError {
        let max_consecutive_failures = self.config.normalized_max_consecutive_failures();
        let mut consecutive_failures = 0;

        loop {
            sleep(self.config.normalized_interval()).await;

            match self
                .queue
                .heartbeat(HeartbeatRequest {
                    proof_id: self.proof_id,
                    worker_id: self.worker_id.clone(),
                    lock_id: self.lock_id,
                })
                .await
            {
                Ok(_) => {
                    consecutive_failures = 0;
                }
                Err(error) if error.is_retryable() => {
                    consecutive_failures += 1;

                    if consecutive_failures >= max_consecutive_failures {
                        warn!(
                            lock_id = %self.lock_id,
                            worker_id = %self.worker_id,
                            consecutive_failures,
                            max_consecutive_failures,
                            error = %error,
                            "proof job heartbeat retryable failures exceeded limit"
                        );
                        return error;
                    }
                }
                Err(error) => return error,
            }
        }
    }
}
