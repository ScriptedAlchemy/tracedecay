use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::retained_surfaces::AutomationTaskV1;

const MAX_AUTOMATION_TERMINAL_COUNT: u64 = 1_000_000;

/// Exact agent-host ledger label for a budget-backoff suppression skip. The
/// projection below must consume this binding so the wire string cannot drift
/// from the typed terminal.
pub const SESSION_EVIDENCE_BUDGET_SUPPRESSED: &str = "session_evidence_budget_suppressed";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutomationSkipReasonV1 {
    AutomationDisabled,
    MemoryCuratorDisabled,
    SessionReflectorDisabled,
    SkillWriterDisabled,
    CombinedReviewDisabled,
    UserJobDisabled,
    JobCommandsDisabled,
    DelegatedHostMode,
    BackendDisabled,
    SchedulerLockActive,
    TaskNotSchedulable,
    SchedulerScheduleInvalid,
    SchedulerScheduleManual,
    SchedulerIdleWindowActive,
    SchedulerNonRetryableFailure,
    SchedulerCooldownActive,
    SchedulerIntervalNotElapsed,
    SchedulerCronNotDue,
    NoNewSessionActivity,
    SimilarityAuthorityUnavailable,
    PartialCoverageNoCandidates,
    NothingToReview,
    SessionEvidenceFilterUnavailable,
    SessionEvidenceRetrievalUnavailable,
    SessionEvidenceUnavailable,
    SessionEvidencePartial,
    SessionEvidenceStale,
    SessionEvidenceDenied,
    SessionEvidenceLocked,
    SessionEvidenceResetRequired,
    SessionCursorManifestLimitExceeded,
    SessionEvidenceBudgetExhausted,
    SessionEvidenceBudgetSuppressed,
    SessionEvidenceCancelled,
    NoSessionEvidence,
    ShippedFactProposalHistoryRetired,
}

impl AutomationSkipReasonV1 {
    /// Projects the exact agent-host ledger label into the closed application
    /// terminal. Unknown labels cannot become durable skipped outcomes.
    pub fn from_ledger_reason(reason: &str) -> Option<Self> {
        use AutomationSkipReasonV1 as Reason;

        Some(match reason {
            "automation_disabled" => Reason::AutomationDisabled,
            "memory_curator_disabled" => Reason::MemoryCuratorDisabled,
            "session_reflector_disabled" => Reason::SessionReflectorDisabled,
            "skill_writer_disabled" => Reason::SkillWriterDisabled,
            "combined_review_disabled" => Reason::CombinedReviewDisabled,
            "user_job_disabled" => Reason::UserJobDisabled,
            "job_commands_disabled" => Reason::JobCommandsDisabled,
            "delegated_host_mode" => Reason::DelegatedHostMode,
            "backend_disabled" => Reason::BackendDisabled,
            "scheduler_lock_active" => Reason::SchedulerLockActive,
            "task_not_schedulable" => Reason::TaskNotSchedulable,
            "scheduler_schedule_invalid" => Reason::SchedulerScheduleInvalid,
            "scheduler_schedule_manual" => Reason::SchedulerScheduleManual,
            "scheduler_idle_window_active" => Reason::SchedulerIdleWindowActive,
            "scheduler_non_retryable_failure" => Reason::SchedulerNonRetryableFailure,
            "scheduler_cooldown_active" => Reason::SchedulerCooldownActive,
            "scheduler_interval_not_elapsed" => Reason::SchedulerIntervalNotElapsed,
            "scheduler_cron_not_due" => Reason::SchedulerCronNotDue,
            "no_new_session_activity" => Reason::NoNewSessionActivity,
            "similarity_authority_unavailable" => Reason::SimilarityAuthorityUnavailable,
            "partial_coverage_no_candidates" => Reason::PartialCoverageNoCandidates,
            "nothing_to_review" => Reason::NothingToReview,
            "session_evidence_filter_unavailable" => Reason::SessionEvidenceFilterUnavailable,
            "session_evidence_retrieval_unavailable" => Reason::SessionEvidenceRetrievalUnavailable,
            "session_evidence_unavailable" => Reason::SessionEvidenceUnavailable,
            "session_evidence_partial" => Reason::SessionEvidencePartial,
            "session_evidence_stale" => Reason::SessionEvidenceStale,
            "session_evidence_denied" => Reason::SessionEvidenceDenied,
            "session_evidence_locked" => Reason::SessionEvidenceLocked,
            "session_evidence_reset_required" => Reason::SessionEvidenceResetRequired,
            "session_cursor_manifest_limit_exceeded" => Reason::SessionCursorManifestLimitExceeded,
            "session_evidence_budget_exhausted" => Reason::SessionEvidenceBudgetExhausted,
            SESSION_EVIDENCE_BUDGET_SUPPRESSED => Reason::SessionEvidenceBudgetSuppressed,
            "session_evidence_cancelled" => Reason::SessionEvidenceCancelled,
            "no_session_evidence" | "no_skill_writer_evidence" => Reason::NoSessionEvidence,
            "shipped_fact_proposal_history_retired" => Reason::ShippedFactProposalHistoryRetired,
            _ => return None,
        })
    }

    pub(super) fn matches_task(self, task: AutomationTaskV1) -> bool {
        use AutomationSkipReasonV1 as Reason;

        match self {
            Reason::MemoryCuratorDisabled
            | Reason::SimilarityAuthorityUnavailable
            | Reason::PartialCoverageNoCandidates
            | Reason::NothingToReview => task == AutomationTaskV1::MemoryCurator,
            Reason::SessionReflectorDisabled
            | Reason::NoNewSessionActivity
            | Reason::ShippedFactProposalHistoryRetired => {
                task == AutomationTaskV1::SessionReflector
            }
            // Skill writer and combined review retrieve the same session
            // evidence surface as the reflector. A typed evidence skip must
            // remain a skip for those tasks instead of failing settlement.
            Reason::SessionEvidenceFilterUnavailable
            | Reason::SessionEvidenceRetrievalUnavailable
            | Reason::SessionEvidenceUnavailable
            | Reason::SessionEvidencePartial
            | Reason::SessionEvidenceStale
            | Reason::SessionEvidenceDenied
            | Reason::SessionEvidenceLocked
            | Reason::SessionEvidenceResetRequired
            | Reason::SessionCursorManifestLimitExceeded
            | Reason::SessionEvidenceBudgetExhausted
            | Reason::SessionEvidenceBudgetSuppressed
            | Reason::SessionEvidenceCancelled
            | Reason::NoSessionEvidence => matches!(
                task,
                AutomationTaskV1::SessionReflector
                    | AutomationTaskV1::SkillWriter
                    | AutomationTaskV1::CombinedReview
            ),
            Reason::SkillWriterDisabled => task == AutomationTaskV1::SkillWriter,
            Reason::CombinedReviewDisabled => task == AutomationTaskV1::CombinedReview,
            Reason::UserJobDisabled | Reason::JobCommandsDisabled => {
                task == AutomationTaskV1::UserJob
            }
            Reason::AutomationDisabled
            | Reason::DelegatedHostMode
            | Reason::BackendDisabled
            | Reason::SchedulerLockActive
            | Reason::TaskNotSchedulable
            | Reason::SchedulerScheduleInvalid
            | Reason::SchedulerScheduleManual
            | Reason::SchedulerIdleWindowActive
            | Reason::SchedulerNonRetryableFailure
            | Reason::SchedulerCooldownActive
            | Reason::SchedulerIntervalNotElapsed
            | Reason::SchedulerCronNotDue => true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutomationRunSummaryV1 {
    pub reviewed_count: u64,
    pub accepted_count: u64,
    pub rejected_count: u64,
    pub skipped_count: u64,
}

impl AutomationRunSummaryV1 {
    pub(super) fn is_bounded(&self) -> bool {
        [
            self.reviewed_count,
            self.accepted_count,
            self.rejected_count,
            self.skipped_count,
        ]
        .into_iter()
        .all(|count| count <= MAX_AUTOMATION_TERMINAL_COUNT)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum AutomationRunTerminalV1 {
    Completed {
        summary: AutomationRunSummaryV1,
    },
    Skipped {
        reason: AutomationSkipReasonV1,
        summary: AutomationRunSummaryV1,
    },
}
