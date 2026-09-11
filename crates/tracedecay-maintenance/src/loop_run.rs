//! Cadence loop that drives admitted maintenance ticks.

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// Park on cancel / wake / cadence, then run the next admitted tick.
pub async fn run_maintenance_loop<F, Fut>(
    cancellation: &tracedecay_session_memory::context::CancellationToken,
    wake: &Notify,
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
            () = wake.notified() => {}
            () = tokio::time::sleep_until(deadline) => {}
        }
        if cancellation.is_cancelled() {
            lifecycle.record_cancellation();
            break;
        }
        let now = CadenceInstant::now();
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
