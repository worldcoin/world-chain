use crate::error::InvalidConfigError;
use std::time::Duration;

/// Default lock duration granted to a worker for a single proving attempt.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_mins(30);

/// Default maximum number of proving attempts per request.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// Default maximum number of requests queued per backend.
pub const DEFAULT_MAX_QUEUE_LEN: usize = 1024;

/// Default delay before polling an unchanged backend job again.
pub const DEFAULT_BACKEND_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Configuration for the `prover-service`.
#[derive(Debug, Clone)]
pub struct ProverServiceConfig {
    /// How long a worker holds a job lock before the job is
    /// considered abandoned and re-queued.
    pub lock_timeout: Duration,
    /// Maximum number of proving attempts (locks) per request before
    /// it is marked as failed.
    pub max_attempts: u32,
    /// Maximum number of requests queued per backend.
    pub max_queue_len: usize,
    /// Delay before a backend job that returned `Noop` becomes pollable again.
    pub backend_poll_interval: Duration,
}

impl ProverServiceConfig {
    pub(crate) fn validate(&self) -> Result<(), InvalidConfigError> {
        if self.lock_timeout.is_zero() {
            return Err(InvalidConfigError("lock_timeout must be greater than zero"));
        }
        if self.max_attempts == 0 {
            return Err(InvalidConfigError("max_attempts must be greater than zero"));
        }
        if self.max_queue_len == 0 {
            return Err(InvalidConfigError(
                "max_queue_len must be greater than zero",
            ));
        }
        if self.backend_poll_interval.is_zero() {
            return Err(InvalidConfigError(
                "backend_poll_interval must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl Default for ProverServiceConfig {
    fn default() -> Self {
        Self {
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            max_queue_len: DEFAULT_MAX_QUEUE_LEN,
            backend_poll_interval: DEFAULT_BACKEND_POLL_INTERVAL,
        }
    }
}
