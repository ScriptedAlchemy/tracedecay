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
            match self.kind {
                DurationKind::BackendStartup => {}
                DurationKind::Run => {}
            }
        }
    }
}

/// Counts one constructed run terminal per status. Failed and skipped
/// terminals are counted alongside successes because a success-only counter
/// hides exactly the waste an automation stall/skip diagnosis needs.
#[inline]
pub(crate) fn observe_run_terminal(_status: AutomationRunStatus) {
    {
        match _status {
            AutomationRunStatus::Succeeded => {}
            AutomationRunStatus::Failed => {}
            AutomationRunStatus::Skipped => {}
            // Non-terminal statuses never reach terminal-record construction.
            AutomationRunStatus::Queued | AutomationRunStatus::Running => {}
        }
    }
}

/// Maps the closed skip vocabulary onto a bounded static counter family.
/// A new variant fails compilation until it chooses a counter.
fn count_skip_reason(reason: AutomationSkipReasonV1) {
    match reason {
        AutomationSkipReasonV1::SchedulerLockActive | AutomationSkipReasonV1::JobLockActive => {}
        AutomationSkipReasonV1::SchedulerCooldownActive => {}
        AutomationSkipReasonV1::SchedulerIntervalNotElapsed
        | AutomationSkipReasonV1::SchedulerCronNotDue
        | AutomationSkipReasonV1::SchedulerIdleWindowActive
        | AutomationSkipReasonV1::SchedulerScheduleManual
        | AutomationSkipReasonV1::SchedulerPaused => {}
        AutomationSkipReasonV1::NoNewSessionActivity => {}
        AutomationSkipReasonV1::AutomationDisabled
        | AutomationSkipReasonV1::DelegatedHostMode
        | AutomationSkipReasonV1::BackendDisabled
        | AutomationSkipReasonV1::TaskNotSchedulable
        | AutomationSkipReasonV1::MemoryCuratorDisabled
        | AutomationSkipReasonV1::SessionReflectorDisabled
        | AutomationSkipReasonV1::SkillWriterDisabled
        | AutomationSkipReasonV1::CombinedReviewDisabled
        | AutomationSkipReasonV1::UserJobDisabled
        | AutomationSkipReasonV1::JobCommandsDisabled => {}
        AutomationSkipReasonV1::SessionEvidenceBudgetSuppressed
        | AutomationSkipReasonV1::BackendIdentitySuppressed
        | AutomationSkipReasonV1::SchedulerNonRetryableFailure => {}
        AutomationSkipReasonV1::SchedulerHistoryInvalid
        | AutomationSkipReasonV1::SchedulerScheduleInvalid => {}
        AutomationSkipReasonV1::SimilarityAuthorityUnavailable
        | AutomationSkipReasonV1::PartialCoverageNoCandidates
        | AutomationSkipReasonV1::NothingToReview
        | AutomationSkipReasonV1::SessionEvidenceFilterUnavailable
        | AutomationSkipReasonV1::SessionEvidenceRetrievalUnavailable
        | AutomationSkipReasonV1::SessionEvidenceUnavailable
        | AutomationSkipReasonV1::SessionEvidencePartial
        | AutomationSkipReasonV1::SessionEvidenceStale
        | AutomationSkipReasonV1::SessionEvidenceDenied
        | AutomationSkipReasonV1::SessionEvidenceLocked
        | AutomationSkipReasonV1::SessionEvidenceResetRequired
        | AutomationSkipReasonV1::SessionCursorManifestLimitExceeded
        | AutomationSkipReasonV1::SessionEvidenceBudgetExhausted
        | AutomationSkipReasonV1::SessionEvidenceTimedOut
        | AutomationSkipReasonV1::SessionEvidenceCancelled
        | AutomationSkipReasonV1::NoSessionEvidence => {}
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
        count_skip_reason(reason);
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
