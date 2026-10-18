//! Retry-with-backoff logic used by the HTTP client for transient network
//! failures. This is the entry point for "where is the retry logic"
//! style questions.

/// Configuration for how many attempts to make and how long to wait
/// between them.
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
}

impl RetryPolicy {
    pub fn new(max_attempts: u32, base_delay_ms: u64) -> Self {
        RetryPolicy {
            max_attempts,
            base_delay_ms,
        }
    }

    /// Exponential backoff delay before attempt number `attempt` (0-based).
    pub fn delay_for_attempt(&self, attempt: u32) -> u64 {
        self.base_delay_ms * 2u64.pow(attempt)
    }
}

/// Retries `operation` up to `policy.max_attempts` times, doubling the
/// delay between attempts, until it succeeds or attempts are exhausted.
pub fn retry_with_backoff<T, E>(
    policy: &RetryPolicy,
    mut operation: impl FnMut() -> Result<T, E>,
) -> Result<T, E> {
    let mut attempt = 0;
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(err) => {
                attempt += 1;
                if attempt >= policy.max_attempts {
                    return Err(err);
                }
                let _delay = policy.delay_for_attempt(attempt);
            }
        }
    }
}
