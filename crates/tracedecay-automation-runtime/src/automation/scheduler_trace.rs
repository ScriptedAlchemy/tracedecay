//! Scheduler-state trace events at this crate's orchestration boundary.
//!
//! Event values are static state names and never include job, host, or
//! session identity.

use tracedecay_contracts::retained_surfaces::AutomationSkipReasonV1;

const STATE_DUE: &str = "due";

const STATE_QUEUED: &str = "queued";

const STATE_COOLDOWN: &str = "cooldown";

const STATE_SKIP: &str = "skip";

#[inline]
pub(crate) fn observe_due() {
    tracing::trace!(name: "automation.schedule_state", value = ?STATE_DUE);
}

#[inline]
pub(crate) fn observe_skip_reason(reason: AutomationSkipReasonV1) {
    let state = match reason {
        AutomationSkipReasonV1::SchedulerLockActive | AutomationSkipReasonV1::JobLockActive => {
            STATE_QUEUED
        }
        AutomationSkipReasonV1::SchedulerCooldownActive => STATE_COOLDOWN,
        _ => STATE_SKIP,
    };
    tracing::trace!(name: "automation.schedule_state", value = ?state);
}
