//! Bounded scheduler and automation-run metrics backed by Hotpath.
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
use tracedecay_automation::evidence_budget::SESSION_EVIDENCE_BUDGET_SUPPRESSED;
#[cfg(feature = "hotpath")]
use tracedecay_automation::run_labels::AUTOMATION_DISABLED;

#[cfg(feature = "hotpath")]
use super::backend_identity::BACKEND_IDENTITY_SUPPRESSED;
use super::run_ledger::AutomationRunStatus;

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
    hotpath::gauge!("automation.queued").set(QUEUED.load(Ordering::Relaxed));
    hotpath::gauge!("automation.running").set(RUNNING.load(Ordering::Relaxed));
    hotpath::gauge!("automation.cooldown").set(COOLDOWN.load(Ordering::Relaxed));
}

/// Holds `automation.running` for the lifetime of one orchestration run.
pub(crate) struct RunningGuard;

impl RunningGuard {
    #[inline]
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

impl Drop for RunningGuard {
    fn drop(&mut self) {
        #[cfg(feature = "hotpath")]
        {
            let _ = RUNNING.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_sub(1))
            });
            publish_queue_gauges();
        }
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
    #[inline]
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

    #[inline]
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

impl Drop for DurationGuard {
    fn drop(&mut self) {
        #[cfg(feature = "hotpath")]
        {
            let ms = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
            match self.kind {
                DurationKind::BackendStartup => {
                    hotpath::gauge!("automation.backend.startup_ms").set(ms);
                }
                DurationKind::Run => {
                    hotpath::gauge!("automation.run_ms").set(ms);
                }
            }
        }
    }
}

/// Counts one constructed run terminal per status. Failed and skipped
/// terminals are counted alongside successes because a success-only counter
/// hides exactly the waste an automation stall/skip diagnosis needs.
#[inline]
pub(crate) fn observe_run_terminal(_status: AutomationRunStatus) {
    #[cfg(feature = "hotpath")]
    {
        match _status {
            AutomationRunStatus::Succeeded => {
                hotpath::gauge!("automation.runs.succeeded_total").inc(1_u64);
            }
            AutomationRunStatus::Failed => {
                hotpath::gauge!("automation.runs.failed_total").inc(1_u64);
            }
            AutomationRunStatus::Skipped => {
                hotpath::gauge!("automation.runs.skipped_total").inc(1_u64);
            }
            // Non-terminal statuses never reach terminal-record construction.
            AutomationRunStatus::Queued | AutomationRunStatus::Running => {}
        }
    }
}

/// Maps the open-ended skip-reason strings onto a bounded static counter
/// family so gauge keys stay compile-time constants with fixed cardinality.
#[cfg(feature = "hotpath")]
fn count_skip_reason(reason: &str) {
    match reason {
        "scheduler_lock_active" => {
            hotpath::gauge!("automation.skips.lock_total").inc(1_u64);
        }
        "scheduler_cooldown_active" => {
            hotpath::gauge!("automation.skips.cooldown_total").inc(1_u64);
        }
        "scheduler_interval_not_elapsed"
        | "scheduler_cron_not_due"
        | "scheduler_idle_window_active"
        | "scheduler_schedule_manual" => {
            hotpath::gauge!("automation.skips.not_due_total").inc(1_u64);
        }
        "no_new_session_activity" => {
            hotpath::gauge!("automation.skips.no_activity_total").inc(1_u64);
        }
        AUTOMATION_DISABLED
        | "delegated_host_mode"
        | "backend_disabled"
        | "task_not_schedulable"
        | "task_disabled"
        | "memory_curator_disabled"
        | "session_reflector_disabled"
        | "skill_writer_disabled"
        | "combined_review_disabled"
        | "user_job_disabled" => {
            hotpath::gauge!("automation.skips.disabled_total").inc(1_u64);
        }
        SESSION_EVIDENCE_BUDGET_SUPPRESSED
        | BACKEND_IDENTITY_SUPPRESSED
        | "scheduler_non_retryable_failure" => {
            hotpath::gauge!("automation.skips.suppressed_total").inc(1_u64);
        }
        "scheduler_history_invalid" | "scheduler_schedule_invalid" => {
            hotpath::gauge!("automation.skips.invalid_total").inc(1_u64);
        }
        _ => {
            hotpath::gauge!("automation.skips.other_total").inc(1_u64);
        }
    }
}

#[inline]
pub(crate) fn observe_due() {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("automation.due_total").inc(1_u64);
        hotpath::val!("automation.schedule_state").set(&STATE_DUE);
        QUEUED.store(1, Ordering::Relaxed);
        COOLDOWN.store(0, Ordering::Relaxed);
        publish_queue_gauges();
    }
}

#[inline]
pub(crate) fn observe_skip_reason(_reason: &str) {
    #[cfg(feature = "hotpath")]
    {
        count_skip_reason(_reason);
        match _reason {
            "scheduler_lock_active" => {
                hotpath::val!("automation.schedule_state").set(&STATE_QUEUED);
                QUEUED.store(1, Ordering::Relaxed);
            }
            "scheduler_cooldown_active" => {
                hotpath::val!("automation.schedule_state").set(&STATE_COOLDOWN);
                COOLDOWN.store(1, Ordering::Relaxed);
                QUEUED.store(0, Ordering::Relaxed);
            }
            _ => {
                hotpath::val!("automation.schedule_state").set(&STATE_SKIP);
                QUEUED.store(0, Ordering::Relaxed);
                COOLDOWN.store(0, Ordering::Relaxed);
            }
        }
        publish_queue_gauges();
    }
}
