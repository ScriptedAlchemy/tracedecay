use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{SESSION_EVIDENCE_BUDGET_EXHAUSTED, SESSION_EVIDENCE_BUDGET_SUPPRESSED};

use crate::retained_surfaces::AutomationTaskV1;

const MAX_AUTOMATION_TERMINAL_COUNT: u64 = 1_000_000;

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
    SessionEvidenceTimedOut,
    SessionEvidenceCancelled,
    NoSessionEvidence,
    ShippedFactProposalHistoryRetired,
}

impl AutomationSkipReasonV1 {
    /// Projects the exact agent-host ledger label into the closed application
    /// terminal. Unknown labels cannot become durable skipped outcomes.
    pub fn from_ledger_reason(reason: &str) -> Option<Self> {
        Some(match reason {
            "automation_disabled" => Self::AutomationDisabled,
            "memory_curator_disabled" => Self::MemoryCuratorDisabled,
            "session_reflector_disabled" => Self::SessionReflectorDisabled,
            "skill_writer_disabled" => Self::SkillWriterDisabled,
            "combined_review_disabled" => Self::CombinedReviewDisabled,
            "user_job_disabled" => Self::UserJobDisabled,
            "job_commands_disabled" => Self::JobCommandsDisabled,
            "delegated_host_mode" => Self::DelegatedHostMode,
            "backend_disabled" => Self::BackendDisabled,
            "scheduler_lock_active" => Self::SchedulerLockActive,
            "task_not_schedulable" => Self::TaskNotSchedulable,
            "scheduler_schedule_invalid" => Self::SchedulerScheduleInvalid,
            "scheduler_schedule_manual" => Self::SchedulerScheduleManual,
            "scheduler_idle_window_active" => Self::SchedulerIdleWindowActive,
            "scheduler_non_retryable_failure" => Self::SchedulerNonRetryableFailure,
            "scheduler_cooldown_active" => Self::SchedulerCooldownActive,
            "scheduler_interval_not_elapsed" => Self::SchedulerIntervalNotElapsed,
            "scheduler_cron_not_due" => Self::SchedulerCronNotDue,
            "no_new_session_activity" => Self::NoNewSessionActivity,
            "similarity_authority_unavailable" => Self::SimilarityAuthorityUnavailable,
            "partial_coverage_no_candidates" => Self::PartialCoverageNoCandidates,
            "nothing_to_review" => Self::NothingToReview,
            "session_evidence_filter_unavailable" => Self::SessionEvidenceFilterUnavailable,
            "session_evidence_retrieval_unavailable" => Self::SessionEvidenceRetrievalUnavailable,
            "session_evidence_unavailable" => Self::SessionEvidenceUnavailable,
            "session_evidence_partial" => Self::SessionEvidencePartial,
            "session_evidence_stale" => Self::SessionEvidenceStale,
            "session_evidence_denied" => Self::SessionEvidenceDenied,
            "session_evidence_locked" => Self::SessionEvidenceLocked,
            "session_evidence_reset_required" => Self::SessionEvidenceResetRequired,
            "session_cursor_manifest_limit_exceeded"
            | "session_cursor_manifest_participants_limit_exceeded"
            | "session_cursor_manifest_canonical_bytes_limit_exceeded" => {
                Self::SessionCursorManifestLimitExceeded
            }
            SESSION_EVIDENCE_BUDGET_EXHAUSTED => Self::SessionEvidenceBudgetExhausted,
            SESSION_EVIDENCE_BUDGET_SUPPRESSED => Self::SessionEvidenceBudgetSuppressed,
            "session_evidence_timed_out" => Self::SessionEvidenceTimedOut,
            "session_evidence_cancelled" => Self::SessionEvidenceCancelled,
            "no_session_evidence" | "no_skill_writer_evidence" => Self::NoSessionEvidence,
            "shipped_fact_proposal_history_retired" => Self::ShippedFactProposalHistoryRetired,
            _ => return None,
        })
    }

    pub(super) fn matches_task(self, task: AutomationTaskV1) -> bool {
        match self {
            Self::MemoryCuratorDisabled
            | Self::SimilarityAuthorityUnavailable
            | Self::PartialCoverageNoCandidates
            | Self::NothingToReview => task == AutomationTaskV1::MemoryCurator,
            Self::SessionReflectorDisabled
            | Self::NoNewSessionActivity
            | Self::ShippedFactProposalHistoryRetired => task == AutomationTaskV1::SessionReflector,
            // Skill writer and combined review retrieve the same session
            // evidence surface as the reflector. A typed evidence skip must
            // remain a skip for those tasks instead of failing settlement.
            Self::SessionEvidenceFilterUnavailable
            | Self::SessionEvidenceRetrievalUnavailable
            | Self::SessionEvidenceUnavailable
            | Self::SessionEvidencePartial
            | Self::SessionEvidenceStale
            | Self::SessionEvidenceDenied
            | Self::SessionEvidenceLocked
            | Self::SessionEvidenceResetRequired
            | Self::SessionCursorManifestLimitExceeded
            | Self::SessionEvidenceBudgetExhausted
            | Self::SessionEvidenceBudgetSuppressed
            | Self::SessionEvidenceTimedOut
            | Self::SessionEvidenceCancelled
            | Self::NoSessionEvidence => matches!(
                task,
                AutomationTaskV1::SessionReflector
                    | AutomationTaskV1::SkillWriter
                    | AutomationTaskV1::CombinedReview
            ),
            Self::SkillWriterDisabled => task == AutomationTaskV1::SkillWriter,
            Self::CombinedReviewDisabled => task == AutomationTaskV1::CombinedReview,
            Self::UserJobDisabled | Self::JobCommandsDisabled => task == AutomationTaskV1::UserJob,
            Self::AutomationDisabled
            | Self::DelegatedHostMode
            | Self::BackendDisabled
            | Self::SchedulerLockActive
            | Self::TaskNotSchedulable
            | Self::SchedulerScheduleInvalid
            | Self::SchedulerScheduleManual
            | Self::SchedulerIdleWindowActive
            | Self::SchedulerNonRetryableFailure
            | Self::SchedulerCooldownActive
            | Self::SchedulerIntervalNotElapsed
            | Self::SchedulerCronNotDue => true,
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
