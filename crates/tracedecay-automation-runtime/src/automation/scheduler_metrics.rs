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

use super::run_ledger::AutomationRunStatus;
use tracedecay_contracts::retained_surfaces::AutomationSkipReasonV1;

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

/// Maps the closed skip vocabulary onto a bounded static counter family.
/// A new variant fails compilation until it chooses a counter.
#[cfg(feature = "hotpath")]
fn count_skip_reason(reason: AutomationSkipReasonV1) {
    match reason {
        AutomationSkipReasonV1::SchedulerLockActive | AutomationSkipReasonV1::JobLockActive => {
            hotpath::gauge!("automation.skips.lock_total").inc(1_u64);
        }
        AutomationSkipReasonV1::SchedulerCooldownActive => {
            hotpath::gauge!("automation.skips.cooldown_total").inc(1_u64);
        }
        AutomationSkipReasonV1::SchedulerIntervalNotElapsed
        | AutomationSkipReasonV1::SchedulerCronNotDue
        | AutomationSkipReasonV1::SchedulerIdleWindowActive
        | AutomationSkipReasonV1::SchedulerScheduleManual
        | AutomationSkipReasonV1::SchedulerPaused => {
            hotpath::gauge!("automation.skips.not_due_total").inc(1_u64);
        }
        AutomationSkipReasonV1::NoNewSessionActivity => {
            hotpath::gauge!("automation.skips.no_activity_total").inc(1_u64);
        }
        AutomationSkipReasonV1::AutomationDisabled
        | AutomationSkipReasonV1::DelegatedHostMode
        | AutomationSkipReasonV1::BackendDisabled
        | AutomationSkipReasonV1::TaskNotSchedulable
        | AutomationSkipReasonV1::MemoryCuratorDisabled
        | AutomationSkipReasonV1::SessionReflectorDisabled
        | AutomationSkipReasonV1::SkillWriterDisabled
        | AutomationSkipReasonV1::CombinedReviewDisabled
        | AutomationSkipReasonV1::UserJobDisabled
        | AutomationSkipReasonV1::JobCommandsDisabled => {
            hotpath::gauge!("automation.skips.disabled_total").inc(1_u64);
        }
        AutomationSkipReasonV1::SessionEvidenceBudgetSuppressed
        | AutomationSkipReasonV1::BackendIdentitySuppressed
        | AutomationSkipReasonV1::SchedulerNonRetryableFailure => {
            hotpath::gauge!("automation.skips.suppressed_total").inc(1_u64);
        }
        AutomationSkipReasonV1::SchedulerHistoryInvalid
        | AutomationSkipReasonV1::SchedulerScheduleInvalid => {
            hotpath::gauge!("automation.skips.invalid_total").inc(1_u64);
        }
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
        | AutomationSkipReasonV1::NoSessionEvidence
        | AutomationSkipReasonV1::ShippedFactProposalHistoryRetired => {
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
pub(crate) fn observe_skip_reason(reason: AutomationSkipReasonV1) {
    #[cfg(not(feature = "hotpath"))]
    let _ = reason;
    #[cfg(feature = "hotpath")]
    {
        count_skip_reason(reason);
        match reason {
            AutomationSkipReasonV1::SchedulerLockActive | AutomationSkipReasonV1::JobLockActive => {
                hotpath::val!("automation.schedule_state").set(&STATE_QUEUED);
                QUEUED.store(1, Ordering::Relaxed);
            }
            AutomationSkipReasonV1::SchedulerCooldownActive => {
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
