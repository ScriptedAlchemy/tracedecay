//! Monotonic deadlines and the cooperative cancellation token.
//!
//! These two primitives were defined in the root crate's
//! `application::context`, but the kernel's `store_runtime::rusqlite_parity`
//! bounds every parity probe with them, so they had to come down with the
//! store-runtime move. The root re-exports both from their historical path.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

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
    cancelled: Arc<AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self {
            token_id: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(tokio::sync::Notify::new()),
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
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Whether two handles observe the same underlying cancellation state.
    #[must_use]
    pub fn is_same_token(&self, other: &Self) -> bool {
        self.token_id == other.token_id
            && Arc::ptr_eq(&self.cancelled, &other.cancelled)
            && Arc::ptr_eq(&self.notify, &other.notify)
    }

    /// Resolves once the token is cancelled.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}
