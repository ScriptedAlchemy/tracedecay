//! Cadence loop that drives admitted maintenance ticks.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::Notify;

use crate::tick::{
    CadenceInstant, MaintenanceCadence, MaintenanceContinuation, MaintenanceTickOutcome,
};

static MAINTENANCE_FUTURES_ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Process-wide count of live maintenance loops. Tests isolate overlapping loops.
#[must_use]
pub fn maintenance_futures_active() -> usize {
    MAINTENANCE_FUTURES_ACTIVE.load(Ordering::SeqCst)
}

struct MaintenanceLoopActive;

impl MaintenanceLoopActive {
    fn enter() -> Self {
        MAINTENANCE_FUTURES_ACTIVE.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for MaintenanceLoopActive {
    fn drop(&mut self) {
        MAINTENANCE_FUTURES_ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The maintenance loop's wake handle.
///
/// [`Self::wake`] parks the loop out of its timer so an already-due tick
/// starts promptly; it never moves the due deadline, so a burst of git-watch
/// events cannot turn the daily cadence into a busy loop.
/// [`Self::request_due`] is for an event that just made retention work
/// collectable, a sealed code generation superseding its predecessor, and
/// pulls the next tick forward to at most one retry delay away. Requests
/// inside that window coalesce into one tick. Before this, every publication
/// left its predecessor's sealed artifact and segments on disk
/// until the daily tick.
#[derive(Default)]
pub struct MaintenanceWake {
    notify: Notify,
    due_requested: AtomicBool,
}

impl MaintenanceWake {
    pub fn wake(&self) {
        self.notify.notify_one();
    }

    pub fn request_due(&self) {
        self.due_requested.store(true, Ordering::Release);
        self.notify.notify_one();
    }

    /// Release every parked waiter (shutdown).
    pub fn notify_waiters(&self) {
        self.notify.notify_waiters();
    }

    fn take_due_request(&self) -> bool {
        self.due_requested.swap(false, Ordering::AcqRel)
    }
}

/// Park on cancel / wake / cadence, then run the next admitted tick.
pub async fn run_maintenance_loop<F, Fut>(
    cancellation: &tracedecay_runtime_core::cancellation::CancellationToken,
    wake: &MaintenanceWake,
    interval: Duration,
    mut run_tick: F,
) where
    F: FnMut(Option<MaintenanceContinuation>) -> Fut,
    Fut: Future<Output = MaintenanceTickOutcome>,
{
    let _active = MaintenanceLoopActive::enter();
    let mut cadence = MaintenanceCadence::new(interval);
    let mut deadline = CadenceInstant::now() + cadence.retry_delay();
    let mut continuation = None;
    loop {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                break;
            }
            () = wake.notify.notified() => {}
            () = tokio::time::sleep_until(deadline) => {}
        }
        if cancellation.is_cancelled() {
            break;
        }
        let now = CadenceInstant::now();
        if wake.take_due_request() {
            deadline = cadence.pull_forward(now, deadline);
        }
        if now < deadline || !cadence.reserve(now) {
            continue;
        }
        let outcome = run_tick(continuation).await;
        if cancellation.is_cancelled() {
            break;
        }
        continuation = outcome.continuation();
        deadline = cadence.finish(CadenceInstant::now(), outcome);
    }
}
