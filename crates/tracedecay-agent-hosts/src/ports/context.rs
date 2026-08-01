//! Cooperative cancellation and deadline primitives.
//!
//! These are a **downward move**, not a port. `agents::context_scout_*` takes
//! a deadline and a cancellation token from whichever adapter drove the
//! request — the daemon, a hook, or an MCP handler — and all three live in the
//! root crate. A duplicated definition would break the contract outright: a
//! cancellation token only propagates cancellation when the canceller and the
//! observer hold the *same* `Arc`, so the two sides must name one type.
//!
//! Root wiring: `src/application/context.rs` drops its own declarations of
//! `MonotonicDeadline` and `CancellationToken` and re-exports these instead, so
//! `crate::application::context::{CancellationToken, MonotonicDeadline}` keeps
//! resolving for every root call site.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// A wall-clock-independent deadline.
///
/// Monotonic by construction: a system clock adjustment mid-request cannot
/// move it, which is why request budgets are expressed against `Instant`
/// rather than `SystemTime`.
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

/// Cooperative cancellation shared by everything working on one request.
///
/// Clones observe the same cancellation: the flag and the notifier are shared
/// `Arc`s, so cancelling any clone releases every waiter on all of them.
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
    /// The sequence number keeps two tokens minted for the same request id
    /// distinguishable, so [`is_same_token`](Self::is_same_token) cannot
    /// confuse a retry's authority with the attempt it replaced.
    #[must_use]
    pub fn for_application_request(request_id: &tracedecay_application::RequestId) -> Self {
        static NEXT_TOKEN_SEQUENCE: AtomicU64 = AtomicU64::new(1);
        let sequence = NEXT_TOKEN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self {
            token_id: Some(Arc::from(format!(
                "cancellation.{}.{sequence}",
                request_id.as_str()
            ))),
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

    /// Whether both handles are clones of one authority rather than two
    /// separately minted tokens that merely share an id.
    #[must_use]
    pub fn is_same_token(&self, other: &Self) -> bool {
        self.token_id == other.token_id
            && Arc::ptr_eq(&self.cancelled, &other.cancelled)
            && Arc::ptr_eq(&self.notify, &other.notify)
    }

    /// Resolves once the token is cancelled.
    ///
    /// The notification is registered *before* the flag is re-read so a cancel
    /// racing this call cannot be missed between the two.
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
