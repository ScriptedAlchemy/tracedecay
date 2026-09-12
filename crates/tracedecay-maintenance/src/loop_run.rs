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

struct MaintenanceLifecycleInstrumentation;

impl MaintenanceLifecycleInstrumentation {
    fn new() -> Self {
        let active = MAINTENANCE_FUTURES_ACTIVE.fetch_add(1, Ordering::SeqCst) + 1;
        hotpath::gauge!("daemon_maintenance_futures_active").set(active);
        Self
    }

    fn record_outcome(&self, outcome: MaintenanceTickOutcome) {
        match outcome {
            MaintenanceTickOutcome::Complete => {
                hotpath::gauge!("daemon_maintenance_outcome_complete").inc(1.0);
            }
            MaintenanceTickOutcome::Continue(MaintenanceContinuation::SemanticVectorRetention) => {
                hotpath::gauge!("daemon_maintenance_outcome_semantic_vector_progress").inc(1.0);
            }
            MaintenanceTickOutcome::Continue(MaintenanceContinuation::CodeGenerationRetention) => {
                hotpath::gauge!("daemon_maintenance_outcome_code_generation_progress").inc(1.0);
            }
            MaintenanceTickOutcome::Retry => {
                hotpath::gauge!("daemon_maintenance_outcome_retry").inc(1.0);
            }
        }
    }

    fn record_cancellation(&self) {
        hotpath::gauge!("daemon_maintenance_outcome_cancelled").inc(1.0);
    }
}

impl Drop for MaintenanceLifecycleInstrumentation {
    fn drop(&mut self) {
        let active = MAINTENANCE_FUTURES_ACTIVE
            .fetch_sub(1, Ordering::SeqCst)
            .saturating_sub(1);
        hotpath::gauge!("daemon_maintenance_futures_active").set(active);
    }
}

struct MaintenancePhaseInstrumentation {
    continuation: Option<MaintenanceContinuation>,
}

impl MaintenancePhaseInstrumentation {
    fn new(continuation: Option<MaintenanceContinuation>) -> Self {
        match continuation {
            Some(MaintenanceContinuation::SemanticVectorRetention) => {
                hotpath::gauge!("daemon_maintenance_phase_semantic_vector_active").inc(1.0);
            }
            Some(MaintenanceContinuation::CodeGenerationRetention) => {
                hotpath::gauge!("daemon_maintenance_phase_code_generation_active").inc(1.0);
            }
            None => {
                hotpath::gauge!("daemon_maintenance_phase_full_tick_active").inc(1.0);
            }
        }
        Self { continuation }
    }
}

impl Drop for MaintenancePhaseInstrumentation {
    fn drop(&mut self) {
        match self.continuation {
            Some(MaintenanceContinuation::SemanticVectorRetention) => {
                hotpath::gauge!("daemon_maintenance_phase_semantic_vector_active").inc(-1.0);
            }
            Some(MaintenanceContinuation::CodeGenerationRetention) => {
                hotpath::gauge!("daemon_maintenance_phase_code_generation_active").inc(-1.0);
            }
            None => {
                hotpath::gauge!("daemon_maintenance_phase_full_tick_active").inc(-1.0);
            }
        }
    }
}

/// The maintenance loop's wake handle.
///
/// [`Self::wake`] parks the loop out of its timer so an already-due tick
/// starts promptly; it never moves the due deadline, so a burst of git-watch
/// events cannot turn the daily cadence into a busy loop.
/// [`Self::request_due`] is for an event that just made retention work
/// collectable — a sealed code generation superseding its predecessor — and
/// pulls the next tick forward to at most one retry delay away. Requests
/// inside that window coalesce into one tick. Before this, every publication
/// left its predecessor's sealed artifact, read bundle, and segments on disk
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
    cancellation: &tracedecay_session_memory::context::CancellationToken,
    wake: &MaintenanceWake,
    interval: Duration,
    mut run_tick: F,
) where
    F: FnMut(Option<MaintenanceContinuation>) -> Fut,
    Fut: Future<Output = MaintenanceTickOutcome>,
{
    let lifecycle = MaintenanceLifecycleInstrumentation::new();
    let mut cadence = MaintenanceCadence::new(interval);
    let mut deadline = CadenceInstant::now() + cadence.retry_delay();
    let mut continuation = None;
    loop {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                lifecycle.record_cancellation();
                break;
            }
            () = wake.notify.notified() => {}
            () = tokio::time::sleep_until(deadline) => {}
        }
        if cancellation.is_cancelled() {
            lifecycle.record_cancellation();
            break;
        }
        let now = CadenceInstant::now();
        if wake.take_due_request() {
            deadline = cadence.pull_forward(now, deadline);
        }
        if now < deadline || !cadence.reserve(now) {
            continue;
        }
        let _phase = MaintenancePhaseInstrumentation::new(continuation);
        let outcome = run_tick(continuation).await;
        if cancellation.is_cancelled() {
            lifecycle.record_cancellation();
            break;
        }
        lifecycle.record_outcome(outcome);
        continuation = outcome.continuation();
        deadline = cadence.finish(CadenceInstant::now(), outcome);
    }
}
