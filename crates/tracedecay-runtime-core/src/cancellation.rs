//! Monotonic deadlines and the cooperative cancellation token.
//!
//! These two primitives were defined in the root crate's
//! `application::context`, but the kernel bounds its store-runtime probes with
//! them, so they had to come down with the store-runtime move. The root
//! re-exports both from their historical path.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use tokio_util::sync::CancellationToken as TokioCancellationToken;

/// A deadline expressed on the monotonic clock.
///
/// Monotonic on purpose: wall-clock jumps must never shorten or extend a
/// request budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonotonicDeadline(Instant);

impl MonotonicDeadline {
    #[must_use]
    pub const fn at(deadline: Instant) -> Self {
        Self(deadline)
    }

    #[must_use]
    pub const fn instant(self) -> Instant {
        self.0
    }

    #[must_use]
    pub fn is_elapsed_at(self, now: Instant) -> bool {
        now >= self.0
    }
}

/// Cooperative cancellation shared by every worker serving one request.
#[derive(Clone, Debug)]
pub struct CancellationToken {
    token_id: Option<Arc<str>>,
    inner: TokioCancellationToken,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self {
            token_id: None,
            inner: TokioCancellationToken::new(),
        }
    }
}

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates the live cancellation authority for an application request.
    ///
    /// Takes the request id as a plain string so the kernel carries no
    /// dependency on the application contract crate.
    #[must_use]
    pub fn for_application_request(request_id: &str) -> Self {
        static NEXT_TOKEN_SEQUENCE: AtomicU64 = AtomicU64::new(1);
        let sequence = NEXT_TOKEN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self {
            token_id: Some(Arc::from(format!("cancellation.{request_id}.{sequence}"))),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn application_token_id(&self) -> Option<&str> {
        self.token_id.as_deref()
    }

    pub fn cancel(&self) {
        self.inner.cancel();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    /// Whether two handles observe the same underlying cancellation state.
    #[must_use]
    pub fn is_same_token(&self, other: &Self) -> bool {
        self.token_id == other.token_id && self.inner == other.inner
    }

    /// Resolves once the token is cancelled.
    pub async fn cancelled(&self) {
        self.inner.cancelled().await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::CancellationToken;

    #[test]
    fn application_identity_is_stable_only_across_clones() {
        let token = CancellationToken::for_application_request("request-7");
        let clone = token.clone();
        let independent = CancellationToken::for_application_request("request-7");

        assert_eq!(token.application_token_id(), clone.application_token_id());
        assert!(token.is_same_token(&clone));
        assert_ne!(
            token.application_token_id(),
            independent.application_token_id()
        );
        assert!(!token.is_same_token(&independent));
    }

    #[tokio::test]
    async fn cancellation_wakes_current_and_future_clone_waiters() {
        let token = CancellationToken::new();
        let first = token.clone();
        let second = token.clone();
        let first_waiter = tokio::spawn(async move { first.cancelled().await });
        let second_waiter = tokio::spawn(async move { second.cancelled().await });

        token.cancel();

        tokio::time::timeout(Duration::from_secs(1), first_waiter)
            .await
            .expect("first waiter should wake")
            .expect("first waiter should finish");
        tokio::time::timeout(Duration::from_secs(1), second_waiter)
            .await
            .expect("second waiter should wake")
            .expect("second waiter should finish");
        tokio::time::timeout(Duration::from_secs(1), token.cancelled())
            .await
            .expect("future waiter should complete");
    }
}
