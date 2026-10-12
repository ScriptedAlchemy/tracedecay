//! Monotonic deadlines and the cooperative cancellation token.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use asupersync::Cx;
use asupersync::cx::cap;
use asupersync::sync::Notify;
use asupersync::types::CancelKind;

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
    inner: Arc<CancellationScope>,
}

/// Request admission can precede a runtime scope. The detached context owns
/// cancellation without granting runtime capabilities, and ancestry keeps
/// parent cancellation observable after intermediate handles are dropped.
#[derive(Debug)]
struct CancellationScope {
    context: Cx<cap::None>,
    parent: Option<Arc<Self>>,
    changed: Arc<Notify>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self {
            token_id: None,
            inner: Arc::new(CancellationScope {
                context: Cx::detached_cancel_context(),
                parent: None,
                changed: Arc::new(Notify::new()),
            }),
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

    /// Adopts the exact cancellation identity from an already-admitted
    /// application context.
    ///
    /// Unlike [`Self::for_application_request`], this does not mint a new
    /// identity. The application boundary has already validated the token, but
    /// the kernel repeats the bounded-string invariant before retaining it.
    #[must_use]
    pub fn for_admitted_application_request(token_id: &str) -> Option<Self> {
        if token_id.is_empty()
            || token_id.trim() != token_id
            || token_id.len() > 512
            || token_id.chars().any(char::is_control)
        {
            return None;
        }
        Some(Self {
            token_id: Some(Arc::from(token_id)),
            ..Self::default()
        })
    }

    #[must_use]
    pub fn application_token_id(&self) -> Option<&str> {
        self.token_id.as_deref()
    }

    /// Creates a cancellation scope for work performed on behalf of this token.
    ///
    /// The child retains the admitted request identity, observes cancellation
    /// from this token, and can be cancelled independently without revoking
    /// the parent request.
    #[must_use]
    pub fn child_token(&self) -> Self {
        Self {
            token_id: self.token_id.clone(),
            inner: Arc::new(CancellationScope {
                context: Cx::detached_cancel_context(),
                parent: Some(Arc::clone(&self.inner)),
                changed: Arc::clone(&self.inner.changed),
            }),
        }
    }

    pub fn cancel(&self) {
        self.inner.context.cancel_fast(CancelKind::User);
        self.inner.changed.notify_waiters();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        let mut scope = Some(self.inner.as_ref());
        while let Some(current) = scope {
            if current.context.is_cancelled() {
                return true;
            }
            scope = current.parent.as_deref();
        }
        false
    }

    /// Whether two handles observe the same underlying cancellation state.
    #[must_use]
    pub fn is_same_token(&self, other: &Self) -> bool {
        self.token_id == other.token_id && Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Resolves once the token is cancelled.
    pub async fn cancelled(&self) {
        // Native wait_until arms its notification generation before checking
        // the predicate, so cancellation cannot race between checking and parking.
        self.inner.changed.wait_until(|| self.is_cancelled()).await;
    }
}

#[cfg(test)]
mod tests {
    use std::future::{Future, poll_fn};
    use std::pin::pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;
    use std::time::Duration;

    use asupersync::Cx;
    use asupersync::sync::Notify;
    use asupersync::time::timeout;

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

    #[test]
    fn admitted_application_identity_is_preserved_exactly() {
        let token =
            CancellationToken::for_admitted_application_request("cancellation.already-admitted")
                .expect("valid admitted cancellation identity");
        let independent =
            CancellationToken::for_admitted_application_request("cancellation.already-admitted")
                .expect("valid admitted cancellation identity");

        assert_eq!(
            token.application_token_id(),
            Some("cancellation.already-admitted")
        );
        assert!(!token.is_same_token(&independent));
        token.cancel();
        assert!(!independent.is_cancelled());
        assert!(CancellationToken::for_admitted_application_request("").is_none());
        assert!(
            CancellationToken::for_admitted_application_request(" cancellation.invalid").is_none()
        );
    }

    #[asupersync::test]
    async fn cancellation_wakes_current_and_future_clone_waiters(cx: &Cx) {
        let token = CancellationToken::new();
        let registered = Arc::new(AtomicUsize::new(0));
        let registration = Arc::new(Notify::new());
        let mut waiters = Vec::new();
        for value in [7, 11] {
            let token = token.clone();
            let registered = Arc::clone(&registered);
            let registration = Arc::clone(&registration);
            waiters.push(
                cx.spawn(move |_| async move {
                    let mut waiting = pin!(token.cancelled());
                    poll_fn(|task_cx| {
                        assert!(waiting.as_mut().poll(task_cx).is_pending());
                        Poll::Ready(())
                    })
                    .await;
                    registered.fetch_add(1, Ordering::Release);
                    registration.notify_waiters();
                    waiting.await;
                    value
                })
                .expect("native waiter task should be admitted"),
            );
        }

        timeout(
            cx.now(),
            Duration::from_secs(1),
            registration.wait_until(|| registered.load(Ordering::Acquire) == 2),
        )
        .await
        .expect("both cancellation waiters should register");
        token.cancel();
        token.cancel();

        for (mut waiter, expected) in waiters.into_iter().zip([7, 11]) {
            let value = timeout(cx.now(), Duration::from_secs(1), waiter.join(cx))
                .await
                .expect("registered waiter should wake")
                .expect("native waiter task should finish");
            assert_eq!(value, expected);
        }
        timeout(cx.now(), Duration::from_secs(1), token.cancelled())
            .await
            .expect("future waiter should complete");
        assert!(token.is_cancelled());
    }

    #[asupersync::test]
    async fn child_cancellation_preserves_identity_and_observes_parent_cancellation(cx: &Cx) {
        let parent =
            CancellationToken::for_admitted_application_request("cancellation.request.semantic")
                .expect("valid admitted cancellation identity");
        let child = parent.child_token();
        let grandchild = child.child_token();

        assert_eq!(
            child.application_token_id(),
            parent.application_token_id(),
            "child work remains attributed to the admitted request"
        );
        assert!(
            !parent.is_same_token(&child),
            "a child must be independently cancellable"
        );
        let mut waiting = pin!(grandchild.cancelled());
        poll_fn(|task_cx| {
            assert!(waiting.as_mut().poll(task_cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(child);
        parent.cancel();

        timeout(cx.now(), Duration::from_secs(1), waiting)
            .await
            .expect("parent cancellation should wake descendants through a dropped child");
        assert!(grandchild.is_cancelled());
        let late_child = parent.child_token();
        assert!(late_child.is_cancelled());
        timeout(cx.now(), Duration::from_secs(1), late_child.cancelled())
            .await
            .expect("children created after cancellation should complete immediately");
    }

    #[asupersync::test]
    async fn child_cancellation_does_not_cancel_the_parent_or_siblings(cx: &Cx) {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        let sibling = parent.child_token();
        let grandchild = child.child_token();
        let mut parent_waiting = pin!(parent.cancelled());
        let mut sibling_waiting = pin!(sibling.cancelled());
        poll_fn(|task_cx| {
            assert!(parent_waiting.as_mut().poll(task_cx).is_pending());
            assert!(sibling_waiting.as_mut().poll(task_cx).is_pending());
            Poll::Ready(())
        })
        .await;

        child.cancel();

        assert!(child.is_cancelled());
        assert!(grandchild.is_cancelled());
        assert!(!parent.is_cancelled());
        assert!(!sibling.is_cancelled());
        assert!(
            timeout(cx.now(), Duration::from_millis(20), parent_waiting)
                .await
                .is_err(),
            "the parent cancellation waiter must stay pending"
        );
        assert!(
            timeout(cx.now(), Duration::from_millis(20), sibling_waiting)
                .await
                .is_err(),
            "the sibling cancellation waiter must stay pending"
        );
        timeout(cx.now(), Duration::from_secs(1), grandchild.cancelled())
            .await
            .expect("child cancellation should reach its own descendants");
        parent.cancel();
        timeout(cx.now(), Duration::from_secs(1), sibling.cancelled())
            .await
            .expect("dropping a pending waiter must not prevent later cancellation");
    }

    #[asupersync::test]
    async fn cancellation_racing_the_first_poll_is_never_lost(cx: &Cx) {
        for _ in 0..64 {
            let token = CancellationToken::new();
            let child = token.child_token();
            let start = Arc::new(std::sync::Barrier::new(2));
            let cancel_start = Arc::clone(&start);
            let cancel = std::thread::spawn(move || {
                cancel_start.wait();
                token.cancel();
            });
            let mut waiting = pin!(child.cancelled());
            start.wait();
            timeout(cx.now(), Duration::from_secs(1), waiting.as_mut())
                .await
                .expect("cancellation racing first poll must wake its waiter");
            cancel.join().expect("cancelling thread should finish");
            assert!(child.is_cancelled());
        }
    }
}
