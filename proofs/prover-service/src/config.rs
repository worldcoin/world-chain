use crate::error::InvalidConfigError;
use std::time::Duration;

/// Default lease duration granted to a worker for a single proving attempt.
pub const DEFAULT_LEASE_TIMEOUT: Duration = Duration::from_mins(30);

/// Default maximum number of proving attempts per request.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// Default maximum number of requests queued per backend.
pub const DEFAULT_MAX_QUEUE_LEN: usize = 1024;

/// Default maximum number of finished (completed or failed) jobs retained.
pub const DEFAULT_MAX_FINISHED_JOBS: usize = 1024;

/// Configuration for the `prover-service`.
#[derive(Debug, Clone)]
pub struct ProverServiceConfig {
    /// How long a worker holds a job lease before the job is
    /// considered abandoned and re-queued.
    pub lease_timeout: Duration,
    /// Maximum number of proving attempts (leases) per request before
    /// it is marked as failed.
    pub max_attempts: u32,
    /// Maximum number of requests queued per backend.
    pub max_queue_len: usize,
    /// Maximum number of finished (completed or failed) jobs retained
    /// in memory; the oldest are evicted first.
    pub max_finished_jobs: usize,
}

impl ProverServiceConfig {
    pub(crate) fn validate(&self) -> Result<(), InvalidConfigError> {
        if self.lease_timeout.is_zero() {
            return Err(InvalidConfigError(
                "lease_timeout must be greater than zero",
            ));
        }
        if self.max_attempts == 0 {
            return Err(InvalidConfigError("max_attempts must be greater than zero"));
        }
        if self.max_queue_len == 0 {
            return Err(InvalidConfigError(
                "max_queue_len must be greater than zero",
            ));
        }
        if self.max_finished_jobs == 0 {
            return Err(InvalidConfigError(
                "max_finished_jobs must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl Default for ProverServiceConfig {
    fn default() -> Self {
        Self {
            lease_timeout: DEFAULT_LEASE_TIMEOUT,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            max_queue_len: DEFAULT_MAX_QUEUE_LEN,
            max_finished_jobs: DEFAULT_MAX_FINISHED_JOBS,
        }
    }
}
