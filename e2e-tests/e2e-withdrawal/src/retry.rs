use backoff::{ExponentialBackoff, backoff::Backoff};
use std::{future::Future, time::Duration};

/// Default attempt budget for storage operations and post-tx persistence/cleanup.
pub const MAX_ATTEMPTS: u32 = 10;

/// Attempt budget for retryable RPC read transport requests.
pub const RPC_MAX_ATTEMPTS: u32 = 3;

/// Default deadline for RPC requests, receipt waits and storage attempts.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Retry `attempt_fn` with exponential backoff until success or the budget is exhausted.
pub async fn with_backoff<T, E, F, Fut>(
    max_attempts: u32,
    mut attempt_fn: F,
    mut should_retry: impl FnMut(&E) -> bool,
    mut on_retry: impl FnMut(u32, Duration, &E),
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let mut backoff = ExponentialBackoff::default();
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match attempt_fn().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                if attempt >= max_attempts || !should_retry(&error) {
                    return Err(error);
                }
                let Some(delay) = backoff.next_backoff() else {
                    return Err(error);
                };
                on_retry(attempt, delay, &error);
                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// Stateful attempt budget for callers that retry across separate invocations.
#[derive(Default)]
pub struct RetryBudget {
    /// Number of failures recorded so far, including the initial failure.
    pub failures: u32,
    backoff: ExponentialBackoff,
}

impl RetryBudget {
    /// Return the next sleep delay, or `None` when the budget is exhausted.
    ///
    /// Callers decide permanence (e.g. via `storage::is_permanent_error`).
    pub fn next_delay(&mut self, is_permanent: bool) -> Option<Duration> {
        self.failures += 1;
        if self.failures >= MAX_ATTEMPTS || is_permanent {
            return None;
        }
        self.backoff.next_backoff()
    }
}
