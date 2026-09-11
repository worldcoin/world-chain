use crate::error::InvalidConfigError;
use std::time::Duration;

/// Default lock duration granted to a worker for a single proving attempt.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_mins(30);

/// Default maximum number of proving attempts per request.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// Default maximum number of retries per request.
pub const DEFAULT_MAX_RETRIES: u32 = 3;

/// Default delay before polling an unchanged backend job again.
pub const DEFAULT_BACKEND_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Default delay between status-poller scans.
pub const DEFAULT_STATUS_POLLER_INTERVAL: Duration = Duration::from_secs(30);

/// Configuration for the `prover-service`.
#[derive(Debug, Clone)]
pub struct ProverServiceConfig {
    /// How long a worker holds a job lock before the job is
    /// considered abandoned and re-queued.
    pub lock_timeout: Duration,
    /// Maximum number of proving attempts (locks) per request before
    /// it is marked as failed.
    pub max_attempts: u32,
    /// Maximum number of times that a proof workflow can be
    /// entirely retried.
    pub max_retries: u32,
    /// Delay before a backend job that returned `Noop` becomes pollable again.
    pub backend_poll_interval: Duration,
    /// Delay between scans that terminalize unhealthy proof requests.
    pub status_poller_interval: Duration,
}

impl ProverServiceConfig {
    pub(crate) fn validate(&self) -> Result<(), InvalidConfigError> {
        if self.lock_timeout.is_zero() {
            return Err(InvalidConfigError("lock_timeout must be greater than zero"));
        }
        if self.max_attempts == 0 {
            return Err(InvalidConfigError("max_attempts must be greater than zero"));
        }
        if self.max_retries == 0 {
            return Err(InvalidConfigError("max_retries must be greater than zero"));
        }
        if self.backend_poll_interval.is_zero() {
            return Err(InvalidConfigError(
                "backend_poll_interval must be greater than zero",
            ));
        }
        if self.status_poller_interval.is_zero() {
            return Err(InvalidConfigError(
                "status_poller_interval must be greater than zero",
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
            max_retries: DEFAULT_MAX_RETRIES,
            backend_poll_interval: DEFAULT_BACKEND_POLL_INTERVAL,
            status_poller_interval: DEFAULT_STATUS_POLLER_INTERVAL,
        }
    }
}
