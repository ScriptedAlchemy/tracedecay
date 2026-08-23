//! Bounded Hotpath gauges for the agent-host automation scheduler.
//!
//! These gauges live at this crate's orchestration boundary so
//! `tracedecay-automation` does not duplicate scheduler or run-queue metrics.
//! Every `hotpath::*` macro expands to a no-op unless the `hotpath` feature is
//! selected; names are static and never include job, host, or session identity.

#[cfg(feature = "hotpath")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "hotpath")]
use std::time::Instant;

#[cfg(feature = "hotpath")]
static QUEUED: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "hotpath")]
static RUNNING: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "hotpath")]
static COOLDOWN: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "hotpath")]
const STATE_DUE: &str = "due";
#[cfg(feature = "hotpath")]
const STATE_QUEUED: &str = "queued";
#[cfg(feature = "hotpath")]
const STATE_COOLDOWN: &str = "cooldown";
#[cfg(feature = "hotpath")]
const STATE_SKIP: &str = "skip";

#[cfg(feature = "hotpath")]
fn publish_queue_gauges() {
    hotpath::gauge!("automation_queued").set(QUEUED.load(Ordering::Relaxed));
    hotpath::gauge!("automation_running").set(RUNNING.load(Ordering::Relaxed));
    hotpath::gauge!("automation_cooldown").set(COOLDOWN.load(Ordering::Relaxed));
}

/// Holds `automation_running` for the lifetime of one orchestration run.
pub(crate) struct RunningGuard;

impl RunningGuard {
    #[inline(always)]
    pub(crate) fn enter() -> Self {
        #[cfg(feature = "hotpath")]
        {
            RUNNING.fetch_add(1, Ordering::Relaxed);
            QUEUED.store(0, Ordering::Relaxed);
            publish_queue_gauges();
        }
        Self
    }
}

#[cfg(feature = "hotpath")]
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
    #[cfg(feature = "hotpath")]
    start: Instant,
    #[cfg(feature = "hotpath")]
    kind: DurationKind,
}

#[cfg(feature = "hotpath")]
#[derive(Clone, Copy)]
pub(crate) enum DurationKind {
    BackendStartup,
    Run,
}

impl DurationGuard {
    #[inline(always)]
    pub(crate) fn backend_startup() -> Self {
        #[cfg(feature = "hotpath")]
        {
            Self {
                start: Instant::now(),
                kind: DurationKind::BackendStartup,
            }
        }
        #[cfg(not(feature = "hotpath"))]
        {
            Self {}
        }
    }

    #[inline(always)]
    pub(crate) fn run() -> Self {
        #[cfg(feature = "hotpath")]
        {
            Self {
                start: Instant::now(),
                kind: DurationKind::Run,
            }
        }
        #[cfg(not(feature = "hotpath"))]
        {
            Self {}
        }
    }
}

#[cfg(feature = "hotpath")]
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

#[inline(always)]
pub(crate) fn observe_due() {
    #[cfg(feature = "hotpath")]
    {
        hotpath::val!("automation_schedule_state").set(&STATE_DUE);
        QUEUED.store(1, Ordering::Relaxed);
        COOLDOWN.store(0, Ordering::Relaxed);
        publish_queue_gauges();
    }
}

#[inline(always)]
pub(crate) fn observe_skip_reason(reason: &str) {
    #[cfg(feature = "hotpath")]
    {
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
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = reason;
    }
}
