use std::time::Duration;

use backon::ExponentialBuilder;

/// Minimum retry delay used to avoid tight retry loops.
pub const MIN_RETRY_DELAY: Duration = Duration::from_millis(1);
/// Default maximum bounded retry attempts.
pub const DEFAULT_BOUNDED_MAX_ATTEMPTS: usize = 10;
/// Default initial bounded retry delay.
pub const DEFAULT_BOUNDED_INITIAL_DELAY: Duration = Duration::from_millis(100);
/// Default maximum bounded retry delay.
pub const DEFAULT_BOUNDED_MAX_DELAY: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy)]
pub struct RetryConfig {
    /// Maximum number of retries performed after the initial call.
    pub max_attempts: usize,
    /// First delay after a retryable failure.
    pub initial_delay: Duration,
    /// Maximum delay between retry attempts.
    pub max_delay: Duration,
}

impl RetryConfig {
    /// Creates a bounded retry config.
    pub const fn new(max_attempts: usize, initial_delay: Duration, max_delay: Duration) -> Self {
        Self {
            max_attempts,
            initial_delay,
            max_delay,
        }
    }

    /// Returns the configured max delay, clamped to the minimum allowed delay.
    pub fn normalized_max_delay(&self) -> Duration {
        self.max_delay.max(MIN_RETRY_DELAY)
    }

    /// Returns the configured initial delay, clamped to the configured max delay.
    pub fn normalized_initial_delay(&self) -> Duration {
        self.initial_delay
            .max(MIN_RETRY_DELAY)
            .min(self.normalized_max_delay())
    }

    /// Creates a `backon` [`ExponentialBuilder`] from this configuration.
    pub fn to_backoff_builder(&self) -> ExponentialBuilder {
        ExponentialBuilder::default()
            .with_min_delay(self.normalized_initial_delay())
            .with_max_delay(self.normalized_max_delay())
            .with_max_times(self.max_attempts)
            .with_jitter()
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self::new(
            DEFAULT_BOUNDED_MAX_ATTEMPTS,
            DEFAULT_BOUNDED_INITIAL_DELAY,
            DEFAULT_BOUNDED_MAX_DELAY,
        )
    }
}
