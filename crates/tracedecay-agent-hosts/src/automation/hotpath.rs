//! Bounded Hotpath gauges for the agent-host automation scheduler.
//!
//! These gauges live at this crate's orchestration boundary so
//! `tracedecay-automation` does not duplicate scheduler or run-queue metrics.
//! Every `hotpath::*` macro expands to a no-op unless the `hotpath` feature is
//! selected; names are static and never include job, host, or session identity.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static QUEUED: AtomicU64 = AtomicU64::new(0);
static RUNNING: AtomicU64 = AtomicU64::new(0);
static COOLDOWN: AtomicU64 = AtomicU64::new(0);

const STATE_DUE: &str = "due";
const STATE_QUEUED: &str = "queued";
const STATE_COOLDOWN: &str = "cooldown";
const STATE_SKIP: &str = "skip";

fn publish_queue_gauges() {
    hotpath::gauge!("automation_queued").set(QUEUED.load(Ordering::Relaxed));
    hotpath::gauge!("automation_running").set(RUNNING.load(Ordering::Relaxed));
    hotpath::gauge!("automation_cooldown").set(COOLDOWN.load(Ordering::Relaxed));
}

/// Holds `automation_running` for the lifetime of one orchestration run.
pub(crate) struct RunningGuard;

impl RunningGuard {
    pub(crate) fn enter() -> Self {
        RUNNING.fetch_add(1, Ordering::Relaxed);
        QUEUED.store(0, Ordering::Relaxed);
        publish_queue_gauges();
        Self
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        let _ = RUNNING.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            Some(value.saturating_sub(1))
        });
        publish_queue_gauges();
    }
}

/// Last-writer duration gauge. Cardinality is one series per kind.
pub(crate) struct DurationGuard {
    start: Instant,
    kind: DurationKind,
}

#[derive(Clone, Copy)]
pub(crate) enum DurationKind {
    BackendStartup,
    Run,
}

impl DurationGuard {
    pub(crate) fn backend_startup() -> Self {
        Self {
            start: Instant::now(),
            kind: DurationKind::BackendStartup,
        }
    }

    pub(crate) fn run() -> Self {
        Self {
            start: Instant::now(),
            kind: DurationKind::Run,
        }
    }
}

impl Drop for DurationGuard {
    fn drop(&mut self) {
        let ms = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        match self.kind {
            DurationKind::BackendStartup => {
                hotpath::gauge!("automation_backend_startup_ms").set(ms);
            }
            DurationKind::Run => {
                hotpath::gauge!("automation_run_ms").set(ms);
            }
        }
    }
}

pub(crate) fn observe_due() {
    hotpath::val!("automation_schedule_state").set(&STATE_DUE);
    QUEUED.store(1, Ordering::Relaxed);
    COOLDOWN.store(0, Ordering::Relaxed);
    publish_queue_gauges();
}

pub(crate) fn observe_skip_reason(reason: &str) {
    match reason {
        "scheduler_lock_active" => {
            hotpath::val!("automation_schedule_state").set(&STATE_QUEUED);
            QUEUED.store(1, Ordering::Relaxed);
        }
        "scheduler_cooldown_active" => {
            hotpath::val!("automation_schedule_state").set(&STATE_COOLDOWN);
            COOLDOWN.store(1, Ordering::Relaxed);
            QUEUED.store(0, Ordering::Relaxed);
        }
        _ => {
            hotpath::val!("automation_schedule_state").set(&STATE_SKIP);
            QUEUED.store(0, Ordering::Relaxed);
            COOLDOWN.store(0, Ordering::Relaxed);
        }
    }
    publish_queue_gauges();
}
