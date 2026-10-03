//! Bounded scheduler and automation-run metrics on the `metrics` facade.
//!
//! These gauges live at this crate's orchestration boundary so
//! `tracedecay-automation` does not duplicate scheduler or run-queue metrics.
//! Every gauge drops when no recorder is installed; the `metrics` facade is
//! selected; names are static and never include job, host, or session identity.

use std::sync::atomic::{AtomicU64, Ordering};

use std::time::Instant;

use super::run_ledger::AutomationRunStatus;
use tracedecay_contracts::retained_surfaces::AutomationSkipReasonV1;

static QUEUED: AtomicU64 = AtomicU64::new(0);

static RUNNING: AtomicU64 = AtomicU64::new(0);

static COOLDOWN: AtomicU64 = AtomicU64::new(0);

const STATE_DUE: &str = "due";

const STATE_QUEUED: &str = "queued";

const STATE_COOLDOWN: &str = "cooldown";

const STATE_SKIP: &str = "skip";

/// Holds `automation.running` for the lifetime of one orchestration run.
pub(crate) struct RunningGuard;

impl RunningGuard {
    #[inline]
    pub(crate) fn enter() -> Self {
        {
            RUNNING.fetch_add(1, Ordering::Relaxed);
            QUEUED.store(0, Ordering::Relaxed);
        }
        Self
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        {
            let _ = RUNNING.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_sub(1))
            });
        }
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
    #[inline]
    pub(crate) fn backend_startup() -> Self {
        {
            Self {
                start: Instant::now(),
                kind: DurationKind::BackendStartup,
            }
        }
    }

    #[inline]
    pub(crate) fn run() -> Self {
        {
            Self {
                start: Instant::now(),
                kind: DurationKind::Run,
            }
        }
    }
}

impl Drop for DurationGuard {
    fn drop(&mut self) {
        {
            let ms = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        }
    }
}

#[inline]
pub(crate) fn observe_due() {
    {
        tracing::trace!(name: "automation.schedule_state", value = ?STATE_DUE);
        QUEUED.store(1, Ordering::Relaxed);
        COOLDOWN.store(0, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn observe_skip_reason(reason: AutomationSkipReasonV1) {
    {
        match reason {
            AutomationSkipReasonV1::SchedulerLockActive | AutomationSkipReasonV1::JobLockActive => {
                tracing::trace!(name: "automation.schedule_state", value = ?STATE_QUEUED);
                QUEUED.store(1, Ordering::Relaxed);
            }
            AutomationSkipReasonV1::SchedulerCooldownActive => {
                tracing::trace!(name: "automation.schedule_state", value = ?STATE_COOLDOWN);
                COOLDOWN.store(1, Ordering::Relaxed);
                QUEUED.store(0, Ordering::Relaxed);
            }
            _ => {
                tracing::trace!(name: "automation.schedule_state", value = ?STATE_SKIP);
                QUEUED.store(0, Ordering::Relaxed);
                COOLDOWN.store(0, Ordering::Relaxed);
            }
        }
    }
}
