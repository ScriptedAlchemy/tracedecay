use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::retained_surfaces::AutomationTaskV1;
use crate::retrieval::SessionRetrievalBudgetStageV1;

const MAX_AUTOMATION_TERMINAL_COUNT: u64 = 1_000_000;

/// One table owns the skip vocabulary. Token, parser, and `ALL` cannot drift;
/// `matches_task` stays a handwritten exhaustive match so a new variant fails
/// compilation until its task affinity is decided.
macro_rules! automation_skip_reasons {
    ($(($variant:ident, $token:literal)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
        #[serde(rename_all = "snake_case")]
        pub enum AutomationSkipReasonV1 {
            $($variant,)+
        }

        impl AutomationSkipReasonV1 {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            /// Canonical ledger/log token. Producers persist this string; they
            /// do not invent a parallel label.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $token,)+
                }
            }

            fn from_canonical_token(reason: &str) -> Option<Self> {
                match reason {
                    $($token => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

automation_skip_reasons! {
    (AutomationDisabled, "automation_disabled"),
    (MemoryCuratorDisabled, "memory_curator_disabled"),
    (SessionReflectorDisabled, "session_reflector_disabled"),
    (SkillWriterDisabled, "skill_writer_disabled"),
    (CombinedReviewDisabled, "combined_review_disabled"),
    (UserJobDisabled, "user_job_disabled"),
    (JobCommandsDisabled, "job_commands_disabled"),
    (JobLockActive, "job_lock_active"),
    (DelegatedHostMode, "delegated_host_mode"),
    (BackendDisabled, "backend_disabled"),
    (BackendIdentitySuppressed, "backend_identity_suppressed"),
    (SchedulerLockActive, "scheduler_lock_active"),
    (SchedulerPaused, "scheduler_paused"),
    (SchedulerHistoryInvalid, "scheduler_history_invalid"),
    (TaskNotSchedulable, "task_not_schedulable"),
    (SchedulerScheduleInvalid, "scheduler_schedule_invalid"),
    (SchedulerScheduleManual, "scheduler_schedule_manual"),
    (SchedulerIdleWindowActive, "scheduler_idle_window_active"),
    (SchedulerNonRetryableFailure, "scheduler_non_retryable_failure"),
    (SchedulerCooldownActive, "scheduler_cooldown_active"),
    (SchedulerIntervalNotElapsed, "scheduler_interval_not_elapsed"),
    (SchedulerCronNotDue, "scheduler_cron_not_due"),
    (NoNewSessionActivity, "no_new_session_activity"),
    (SimilarityAuthorityUnavailable, "similarity_authority_unavailable"),
    (PartialCoverageNoCandidates, "partial_coverage_no_candidates"),
    (NothingToReview, "nothing_to_review"),
    (SessionEvidenceFilterUnavailable, "session_evidence_filter_unavailable"),
    (SessionEvidenceRetrievalUnavailable, "session_evidence_retrieval_unavailable"),
    (SessionEvidenceUnavailable, "session_evidence_unavailable"),
    (SessionEvidencePartial, "session_evidence_partial"),
    (SessionEvidenceStale, "session_evidence_stale"),
    (SessionEvidenceDenied, "session_evidence_denied"),
    (SessionEvidenceLocked, "session_evidence_locked"),
    (SessionEvidenceResetRequired, "session_evidence_reset_required"),
    (SessionCursorManifestLimitExceeded, "session_cursor_manifest_limit_exceeded"),
    (SessionEvidenceBudgetExhausted, "session_evidence_budget_exhausted"),
    (SessionEvidenceBudgetSuppressed, "session_evidence_budget_suppressed"),
    (SessionEvidenceTimedOut, "session_evidence_timed_out"),
    (SessionEvidenceCancelled, "session_evidence_cancelled"),
    (NoSessionEvidence, "no_session_evidence"),
    (ShippedFactProposalHistoryRetired, "shipped_fact_proposal_history_retired"),
}

/// How a skip affects the next cadence decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomationSkipCadenceEffectV1 {
    /// Admission refusal. Does not postpone the next real attempt.
    Diagnostic,
    /// Entered the task and finished as a transient retrieval timeout.
    EffectfulTimeout,
    /// Entered the task or consumed its review opportunity.
    Effectful,
}

impl AutomationSkipReasonV1 {
    /// Projects a persisted ledger label into the closed terminal.
    ///
    /// Canonical tokens come from [`Self::as_str`]. Historical aliases that
    /// collapse into one variant (manifest-limit kinds, budget stages, the
    /// skill-writer empty-evidence label) are parsed only at this boundary.
    /// Unknown labels cannot become durable skipped outcomes.
    pub fn from_ledger_reason(reason: &str) -> Option<Self> {
        if let Some(reason) = Self::from_canonical_token(reason) {
            return Some(reason);
        }
        Some(match reason {
            "session_cursor_manifest_participants_limit_exceeded"
            | "session_cursor_manifest_canonical_bytes_limit_exceeded" => {
                Self::SessionCursorManifestLimitExceeded
            }
            reason
                if reason
                    .strip_prefix("session_evidence_budget_exhausted_")
                    .is_some_and(|stage| {
                        SessionRetrievalBudgetStageV1::deserialize(
                            serde::de::value::StrDeserializer::<serde::de::value::Error>::new(
                                stage,
                            ),
                        )
                        .is_ok()
                    }) =>
            {
                Self::SessionEvidenceBudgetExhausted
            }
            "no_skill_writer_evidence" => Self::NoSessionEvidence,
            _ => return None,
        })
    }

    /// Whether this skip moved cadence and, if it did, whether it is a
    /// transient retrieval timeout. Admission diagnostics must not postpone
    /// the next attempt. A new variant fails compilation until it is classified.
    pub const fn cadence_effect(self) -> AutomationSkipCadenceEffectV1 {
        match self {
            Self::AutomationDisabled
            | Self::MemoryCuratorDisabled
            | Self::SessionReflectorDisabled
            | Self::SkillWriterDisabled
            | Self::CombinedReviewDisabled
            | Self::UserJobDisabled
            | Self::DelegatedHostMode
            | Self::BackendDisabled
            | Self::BackendIdentitySuppressed
            | Self::SchedulerLockActive
            | Self::SchedulerPaused
            | Self::SchedulerHistoryInvalid
            | Self::TaskNotSchedulable
            | Self::SchedulerScheduleInvalid
            | Self::SchedulerScheduleManual
            | Self::SchedulerIdleWindowActive
            | Self::SchedulerNonRetryableFailure
            | Self::SchedulerCooldownActive
            | Self::SchedulerIntervalNotElapsed
            | Self::SchedulerCronNotDue
            | Self::NoNewSessionActivity
            | Self::SessionEvidenceBudgetSuppressed
            | Self::JobLockActive => AutomationSkipCadenceEffectV1::Diagnostic,
            Self::SessionEvidenceTimedOut => AutomationSkipCadenceEffectV1::EffectfulTimeout,
            Self::JobCommandsDisabled
            | Self::SimilarityAuthorityUnavailable
            | Self::PartialCoverageNoCandidates
            | Self::NothingToReview
            | Self::SessionEvidenceFilterUnavailable
            | Self::SessionEvidenceRetrievalUnavailable
            | Self::SessionEvidenceUnavailable
            | Self::SessionEvidencePartial
            | Self::SessionEvidenceStale
            | Self::SessionEvidenceDenied
            | Self::SessionEvidenceLocked
            | Self::SessionEvidenceResetRequired
            | Self::SessionCursorManifestLimitExceeded
            | Self::SessionEvidenceBudgetExhausted
            | Self::SessionEvidenceCancelled
            | Self::NoSessionEvidence
            | Self::ShippedFactProposalHistoryRetired => AutomationSkipCadenceEffectV1::Effectful,
        }
    }

    pub const fn is_cadence_diagnostic(self) -> bool {
        matches!(
            self.cadence_effect(),
            AutomationSkipCadenceEffectV1::Diagnostic
        )
    }

    pub const fn is_retryable_retrieval_timeout(self) -> bool {
        matches!(
            self.cadence_effect(),
            AutomationSkipCadenceEffectV1::EffectfulTimeout
        )
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
            Self::UserJobDisabled | Self::JobCommandsDisabled | Self::JobLockActive => {
                task == AutomationTaskV1::UserJob
            }
            Self::AutomationDisabled
            | Self::DelegatedHostMode
            | Self::BackendDisabled
            | Self::BackendIdentitySuppressed
            | Self::SchedulerLockActive
            | Self::SchedulerPaused
            | Self::SchedulerHistoryInvalid
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

#[cfg(test)]
mod tests {
    use super::AutomationSkipReasonV1;
    use tracedecay_domain::{
        SESSION_EVIDENCE_BUDGET_EXHAUSTED, SESSION_EVIDENCE_BUDGET_SUPPRESSED,
    };

    #[test]
    fn budget_stage_skips_accept_known_stages_only() {
        assert_eq!(
            AutomationSkipReasonV1::from_ledger_reason(
                "session_evidence_budget_exhausted_execution_work_exhausted"
            ),
            Some(AutomationSkipReasonV1::SessionEvidenceBudgetExhausted),
        );
        assert_eq!(
            AutomationSkipReasonV1::from_ledger_reason("session_evidence_budget_exhausted_unknown"),
            None,
        );
    }

    #[test]
    fn every_skip_reason_token_is_its_serde_name_and_parses_back() {
        for reason in AutomationSkipReasonV1::ALL {
            assert_eq!(
                AutomationSkipReasonV1::from_ledger_reason(reason.as_str()),
                Some(*reason),
                "{}",
                reason.as_str()
            );
            assert_eq!(
                serde_json::to_value(reason).expect("skip reason json"),
                serde_json::json!(reason.as_str())
            );
        }
        assert_eq!(
            SESSION_EVIDENCE_BUDGET_EXHAUSTED,
            AutomationSkipReasonV1::SessionEvidenceBudgetExhausted.as_str()
        );
        assert_eq!(
            SESSION_EVIDENCE_BUDGET_SUPPRESSED,
            AutomationSkipReasonV1::SessionEvidenceBudgetSuppressed.as_str()
        );
        assert!(
            AutomationSkipReasonV1::BackendIdentitySuppressed.is_cadence_diagnostic()
                && AutomationSkipReasonV1::SchedulerHistoryInvalid.is_cadence_diagnostic()
                && AutomationSkipReasonV1::JobLockActive.is_cadence_diagnostic()
        );
        assert!(AutomationSkipReasonV1::SessionEvidenceTimedOut.is_retryable_retrieval_timeout());
        assert!(
            !AutomationSkipReasonV1::JobLockActive
                .matches_task(crate::retained_surfaces::AutomationTaskV1::MemoryCurator)
        );
        assert!(
            AutomationSkipReasonV1::JobLockActive
                .matches_task(crate::retained_surfaces::AutomationTaskV1::UserJob)
        );
    }
}
