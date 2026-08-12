use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::{
    ActorId, DomainError, FactAssertionId, FactEventId, FactId, ManifestDigest, ProvenanceId,
    RunId, SanitizationReceiptV1, SanitizerDispositionV1, UtcMicros, canonical_sha256,
};

use super::FactCommitOwnerV1;
use crate::memory::{FactCategoryV1, FactMetadataV1};
use crate::retained_surfaces::{
    MemoryAutomationTaskV1, RetainedSurfaceOperation, retained_surface_application_operation,
    retained_surface_problem_matches_terminal,
};
use crate::{
    ApplicationContractError, ApplicationProblemEnvelope, ApplicationProblemKind, RequestId,
    ResolvedScope,
};

mod curation;

use curation::curation_receipt_matches;
pub use curation::{
    MemoryAutomationCurationAddDispositionV1, MemoryAutomationCurationLinkDispositionV1,
    MemoryAutomationCurationMergeV1, MemoryAutomationCurationOperationEffectV1,
    MemoryAutomationCurationReceiptV1, MemoryAutomationCurationRelationKindV1,
    MemoryAutomationCurationRelationProvenanceV1, MemoryAutomationCurationRelationV1,
    MemoryAutomationCurationRemoveDispositionV1, MemoryAutomationCurationResultV1,
};

const MAX_AUTOMATION_TERMINAL_COUNT: u64 = 1_000_000;

#[derive(Serialize)]
struct AutomaticFactDigestProjection<'a> {
    domain: &'static str,
    apply_id: &'a ProvenanceId,
    owner: &'a FactCommitOwnerV1,
    state: MemoryAutomationFactStateV1,
    operation_id: &'a ProvenanceId,
    input_digest: &'a str,
    actor: Option<&'a ActorId>,
    sanitization_receipt: &'a SanitizationReceiptV1,
    content: &'a str,
    category: FactCategoryV1,
    source_label: Option<&'a str>,
    tags: &'a [String],
    entities: &'a [String],
    default_trust: f64,
    metadata: &'a FactMetadataV1,
    automation_run_id: Option<&'a str>,
    evidence: &'a MemoryAutomationFactEvidenceV1,
    effect_state: MemoryAutomationFactStateV1,
    fact_id: Option<&'a FactId>,
    target_owner: Option<&'a FactCommitOwnerV1>,
    target_fact_id: Option<&'a FactId>,
    assertion_id: Option<&'a FactAssertionId>,
    event_id: Option<&'a FactEventId>,
    quarantine_reason: Option<&'a str>,
    recorded_at_micros: UtcMicros,
    disposition: &'static str,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAutomationSkipReasonV1 {
    AutomationDisabled,
    MemoryCuratorDisabled,
    SessionReflectorDisabled,
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
    SessionEvidenceCancelled,
    NoSessionEvidence,
    ShippedFactProposalHistoryRetired,
}

impl MemoryAutomationSkipReasonV1 {
    /// Projects the exact agent-host ledger label into the closed application
    /// terminal. Unknown labels cannot become durable skipped outcomes.
    pub fn from_ledger_reason(reason: &str) -> Option<Self> {
        use MemoryAutomationSkipReasonV1 as Reason;

        Some(match reason {
            "automation_disabled" => Reason::AutomationDisabled,
            "memory_curator_disabled" => Reason::MemoryCuratorDisabled,
            "session_reflector_disabled" => Reason::SessionReflectorDisabled,
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
            "session_evidence_cancelled" => Reason::SessionEvidenceCancelled,
            "no_session_evidence" => Reason::NoSessionEvidence,
            "shipped_fact_proposal_history_retired" => Reason::ShippedFactProposalHistoryRetired,
            _ => return None,
        })
    }

    fn matches_task(self, task: MemoryAutomationTaskV1) -> bool {
        use MemoryAutomationSkipReasonV1 as Reason;

        match self {
            Reason::MemoryCuratorDisabled
            | Reason::SimilarityAuthorityUnavailable
            | Reason::PartialCoverageNoCandidates
            | Reason::NothingToReview => task == MemoryAutomationTaskV1::MemoryCurator,
            Reason::SessionReflectorDisabled
            | Reason::NoNewSessionActivity
            | Reason::SessionEvidenceFilterUnavailable
            | Reason::SessionEvidenceRetrievalUnavailable
            | Reason::SessionEvidenceUnavailable
            | Reason::SessionEvidencePartial
            | Reason::SessionEvidenceStale
            | Reason::SessionEvidenceDenied
            | Reason::SessionEvidenceLocked
            | Reason::SessionEvidenceResetRequired
            | Reason::SessionCursorManifestLimitExceeded
            | Reason::SessionEvidenceBudgetExhausted
            | Reason::SessionEvidenceCancelled
            | Reason::NoSessionEvidence
            | Reason::ShippedFactProposalHistoryRetired => {
                task == MemoryAutomationTaskV1::SessionReflector
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
pub struct MemoryAutomationRunSummaryV1 {
    pub reviewed_count: u64,
    pub accepted_count: u64,
    pub rejected_count: u64,
    pub skipped_count: u64,
}

impl MemoryAutomationRunSummaryV1 {
    fn is_bounded(&self) -> bool {
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
pub enum MemoryAutomationRunTerminalV1 {
    Completed {
        summary: MemoryAutomationRunSummaryV1,
    },
    Skipped {
        reason: MemoryAutomationSkipReasonV1,
        summary: MemoryAutomationRunSummaryV1,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAutomationFactStateV1 {
    Applied,
    Quarantined,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAutomationFactDispositionV1 {
    Applied,
    AlreadyApplied,
    Quarantined,
}

/// Store-owned raw SHA-256 input digest. This intentionally has no algorithm
/// prefix because the canonical fact command does not expose one.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(transparent)]
pub struct MemoryAutomationFactInputDigestV1(String);

impl MemoryAutomationFactInputDigestV1 {
    pub fn new(value: impl Into<String>) -> Result<Self, MemoryAutomationFactInputDigestError> {
        let value = value.into();
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value))
        } else {
            Err(MemoryAutomationFactInputDigestError)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MemoryAutomationFactInputDigestV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryAutomationFactInputDigestError;

impl fmt::Display for MemoryAutomationFactInputDigestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("automatic fact input digest must be 64 lowercase hexadecimal bytes")
    }
}

impl std::error::Error for MemoryAutomationFactInputDigestError {}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationFactRequestV1 {
    pub operation_id: ProvenanceId,
    pub input_digest: MemoryAutomationFactInputDigestV1,
    pub actor: Option<ActorId>,
    pub sanitization_receipt: SanitizationReceiptV1,
    pub content: String,
    pub category: FactCategoryV1,
    pub source_label: Option<String>,
    pub tags: Vec<String>,
    pub entities: Vec<String>,
    pub default_trust_millionths: u32,
    pub metadata: FactMetadataV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationFactEvidenceSourceSpanV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
}

impl MemoryAutomationFactEvidenceSourceSpanV1 {
    fn matches_terminal(&self) -> bool {
        let raw_message = self
            .session_id
            .as_deref()
            .zip(self.message_id.as_deref())
            .is_some_and(|(session_id, message_id)| {
                valid_text(session_id, 4_096) && valid_text(message_id, 4_096)
            });
        let raw_store = self.store_id.is_some();
        let summary_node = self
            .node_id
            .as_deref()
            .is_some_and(|node_id| valid_text(node_id, 4_096));
        let complete_raw_identity = self.session_id.is_some() == self.message_id.is_some();
        complete_raw_identity
            && usize::from(raw_message) + usize::from(raw_store) + usize::from(summary_node) == 1
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationFactEvidenceItemV1 {
    pub content: String,
    pub category: FactCategoryV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entities: Option<Vec<String>>,
    pub trust: MemoryAutomationFactEvidenceTrustV1,
    pub source_span: MemoryAutomationFactEvidenceSourceSpanV1,
    pub reason: String,
}

impl MemoryAutomationFactEvidenceItemV1 {
    fn matches_terminal(&self) -> bool {
        valid_text(&self.content, 64 * 1_024)
            && self.tags.as_ref().is_none_or(|values| {
                values.len() <= 20 && values.iter().all(|value| valid_text(value, 4_096))
            })
            && self.entities.as_ref().is_none_or(|values| {
                values.len() <= 20 && values.iter().all(|value| valid_text(value, 4_096))
            })
            && self.trust.matches_terminal()
            && self.source_span.matches_terminal()
            && valid_text(&self.reason, 4_096)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum MemoryAutomationFactEvidenceTrustV1 {
    Numeric(f64),
    Bucket(MemoryAutomationFactEvidenceTrustBucketV1),
}

impl MemoryAutomationFactEvidenceTrustV1 {
    fn matches_terminal(self) -> bool {
        match self {
            Self::Numeric(value) => value.is_finite() && (0.0..=1.0).contains(&value),
            Self::Bucket(_) => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAutomationFactEvidenceTrustBucketV1 {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationFactNearestMatchV1 {
    pub canonical_fact_id: FactId,
    pub score: f64,
    pub category: FactCategoryV1,
}

impl MemoryAutomationFactNearestMatchV1 {
    fn matches_terminal(&self) -> bool {
        self.score.is_finite() && (0.0..=1.0).contains(&self.score)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAutomationFactValidationStatusV1 {
    Accepted,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAutomationFactConflictSourceV1 {
    ApplyTimeAddFactDiff,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationFactDedupeValidationV1 {
    pub nearest: Option<MemoryAutomationFactNearestMatchV1>,
    pub near_duplicate_threshold: f64,
}

impl MemoryAutomationFactDedupeValidationV1 {
    fn matches_terminal(&self) -> bool {
        self.near_duplicate_threshold.is_finite()
            && (0.0..=1.0).contains(&self.near_duplicate_threshold)
            && self
                .nearest
                .as_ref()
                .is_none_or(MemoryAutomationFactNearestMatchV1::matches_terminal)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationFactConflictValidationV1 {
    pub source: MemoryAutomationFactConflictSourceV1,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationFactValidationV1 {
    pub status: MemoryAutomationFactValidationStatusV1,
    pub dedupe: MemoryAutomationFactDedupeValidationV1,
    pub conflict: MemoryAutomationFactConflictValidationV1,
}

impl MemoryAutomationFactValidationV1 {
    fn matches_terminal(&self) -> bool {
        self.dedupe.matches_terminal() && valid_text(&self.conflict.note, 4_096)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationFactEvidenceV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<MemoryAutomationFactEvidenceItemV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<MemoryAutomationFactValidationV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationFactTargetV1 {
    pub owner: FactCommitOwnerV1,
    pub fact_id: FactId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryAutomationFactEffectV1 {
    Applied {
        fact_id: FactId,
        target: MemoryAutomationFactTargetV1,
        assertion_id: FactAssertionId,
        event_id: FactEventId,
    },
    Quarantined {
        reason: String,
    },
}

/// Exact public projection of one canonical automatic-fact authority result.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationFactReceiptV1 {
    pub apply_id: ProvenanceId,
    pub owner: FactCommitOwnerV1,
    pub state: MemoryAutomationFactStateV1,
    pub disposition: MemoryAutomationFactDispositionV1,
    pub automation_run_id: RunId,
    pub request: MemoryAutomationFactRequestV1,
    pub evidence: MemoryAutomationFactEvidenceV1,
    pub effect: MemoryAutomationFactEffectV1,
    pub recorded_at_micros: UtcMicros,
    pub canonical_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(
    tag = "kind",
    content = "receipt",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum MemoryAutomationCommittedReceiptV1 {
    Curation(MemoryAutomationCurationReceiptV1),
    AutomaticFact(MemoryAutomationFactReceiptV1),
}

impl MemoryAutomationCommittedReceiptV1 {
    fn matches_terminal(&self, run_id: &RunId) -> bool {
        match self {
            Self::Curation(receipt) => curation_receipt_matches(run_id, receipt),
            Self::AutomaticFact(receipt) => automatic_fact_receipt_matches(run_id, receipt),
        }
    }
}

/// Durable terminal payload for one admitted automatic-memory run.
///
/// An empty receipt list is valid for completed or skipped zero-effect runs.
/// Partial effects are represented only by an application problem carrying a
/// non-empty committed effect receipt.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationRunResultV1 {
    pub run_id: RunId,
    pub task: MemoryAutomationTaskV1,
    pub terminal: MemoryAutomationRunTerminalV1,
    pub committed_receipts: Vec<MemoryAutomationCommittedReceiptV1>,
}

/// Canonical admitted problem for one automatic-memory run.
///
/// The generic application receipt binds the outer operation. The ordered
/// receipts retain the exact canonical memory effects needed to reconcile a
/// partial terminal without inventing an endpoint-specific payload.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationRunProblemV1 {
    pub run_id: RunId,
    pub task: MemoryAutomationTaskV1,
    pub scope: ResolvedScope,
    pub problem: ApplicationProblemEnvelope,
    pub committed_receipts: Vec<MemoryAutomationCommittedReceiptV1>,
}

impl<'de> Deserialize<'de> for MemoryAutomationRunProblemV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            run_id: RunId,
            task: MemoryAutomationTaskV1,
            scope: ResolvedScope,
            problem: ApplicationProblemEnvelope,
            committed_receipts: Vec<MemoryAutomationCommittedReceiptV1>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let terminal = Self {
            run_id: wire.run_id,
            task: wire.task,
            scope: wire.scope,
            problem: wire.problem,
            committed_receipts: wire.committed_receipts,
        };
        if !terminal.matches_terminal(&terminal.problem.request_id) {
            return Err(serde::de::Error::custom(
                "memory automation problem does not match its admitted terminal",
            ));
        }
        Ok(terminal)
    }
}

impl MemoryAutomationRunProblemV1 {
    pub fn new(
        run_id: RunId,
        task: MemoryAutomationTaskV1,
        scope: ResolvedScope,
        problem: ApplicationProblemEnvelope,
        committed_receipts: Vec<MemoryAutomationCommittedReceiptV1>,
        request_id: &RequestId,
    ) -> Result<Self, ApplicationContractError> {
        let terminal = Self {
            run_id,
            task,
            scope,
            problem,
            committed_receipts,
        };
        if terminal.matches_terminal(request_id) {
            Ok(terminal)
        } else {
            Err(ApplicationContractError::Inconsistent {
                field: "memory automation problem terminal",
            })
        }
    }

    pub fn matches_terminal(&self, request_id: &RequestId) -> bool {
        let Ok(operation) =
            retained_surface_application_operation(RetainedSurfaceOperation::MemoryAutomationRun)
        else {
            return false;
        };
        if self.run_id.validate().is_err()
            || self.scope.validate().is_err()
            || self.problem.request_id != *request_id
            || self.problem.contract != *operation.result_contract()
            || !self.problem.problem.source().is_admitted_terminal()
            || !retained_surface_problem_matches_terminal(
                RetainedSurfaceOperation::MemoryAutomationRun,
                request_id,
                Some(&self.scope),
                self.problem.problem.source(),
            )
        {
            return false;
        }

        let is_partial = self.problem.problem.kind() == ApplicationProblemKind::PartialEffect;
        if is_partial != !self.committed_receipts.is_empty() {
            return false;
        }
        if !is_partial {
            return true;
        }

        if !receipts_match_task_and_identity(self.task, &self.run_id, &self.committed_receipts) {
            return false;
        }

        let Ok(committed_state) = canonical_sha256(&(
            "tracedecay.memory-automation-run.partial-state.v1",
            self.run_id.as_str(),
            &self.committed_receipts,
        )) else {
            return false;
        };
        self.problem
            .problem
            .source()
            .committed_receipt()
            .and_then(|receipt| receipt.committed_state.as_ref())
            == Some(&committed_state)
    }

    /// Binds every problem, including zero-effect terminals, to its admitted
    /// automatic-memory run.
    pub fn matches_admission(&self, run_id: &RunId, task: MemoryAutomationTaskV1) -> bool {
        &self.run_id == run_id && self.task == task
    }
}

impl MemoryAutomationRunResultV1 {
    /// Verifies the durable terminal against the exact admitted run and task.
    /// This closes the zero-effect case where no inner receipt can carry the
    /// admission identity on its own.
    pub fn matches_admission(&self, run_id: &RunId, task: MemoryAutomationTaskV1) -> bool {
        &self.run_id == run_id && self.task == task && self.matches_terminal()
    }

    /// Validates invariants that span the outer run and its ordered inner
    /// authority receipts before a transport may expose the terminal.
    pub fn matches_terminal(&self) -> bool {
        if self.run_id.validate().is_err() {
            return false;
        }
        let terminal_matches = match &self.terminal {
            MemoryAutomationRunTerminalV1::Completed { summary } => {
                summary.is_bounded()
                    && summary.skipped_count == 0
                    && summary.reviewed_count
                        == summary
                            .accepted_count
                            .saturating_add(summary.rejected_count)
            }
            MemoryAutomationRunTerminalV1::Skipped { reason, summary } => {
                summary.is_bounded()
                    && summary.reviewed_count == 0
                    && summary.accepted_count == 0
                    && summary.rejected_count == 0
                    && summary.skipped_count == 1
                    && self.committed_receipts.is_empty()
                    && reason.matches_task(self.task)
            }
        };
        terminal_matches
            && self.summary_matches_receipts()
            && receipts_match_task_and_identity(self.task, &self.run_id, &self.committed_receipts)
    }

    fn summary_matches_receipts(&self) -> bool {
        let MemoryAutomationRunTerminalV1::Completed { summary } = &self.terminal else {
            return true;
        };
        match self.task {
            MemoryAutomationTaskV1::MemoryCurator => {
                let mut accepted_count = 0_u64;
                let mut receipt_count = 0_usize;
                for receipt in &self.committed_receipts {
                    let MemoryAutomationCommittedReceiptV1::Curation(receipt) = receipt else {
                        return false;
                    };
                    receipt_count += 1;
                    accepted_count =
                        match accepted_count.checked_add(receipt.receipt.accepted_operations) {
                            Some(count) => count,
                            None => return false,
                        };
                }
                receipt_count <= 1
                    && summary.accepted_count == accepted_count
                    && summary.rejected_count == 0
            }
            MemoryAutomationTaskV1::SessionReflector => {
                let mut applied_count = 0_u64;
                let mut quarantined_count = 0_u64;
                for receipt in &self.committed_receipts {
                    let MemoryAutomationCommittedReceiptV1::AutomaticFact(receipt) = receipt else {
                        return false;
                    };
                    match receipt.state {
                        MemoryAutomationFactStateV1::Applied => applied_count += 1,
                        MemoryAutomationFactStateV1::Quarantined => quarantined_count += 1,
                    }
                }
                summary.accepted_count == applied_count
                    && summary.rejected_count == quarantined_count
            }
        }
    }
}

fn receipts_match_task_and_identity(
    task: MemoryAutomationTaskV1,
    run_id: &RunId,
    receipts: &[MemoryAutomationCommittedReceiptV1],
) -> bool {
    if task == MemoryAutomationTaskV1::MemoryCurator && receipts.len() > 1 {
        return false;
    }
    let family_matches = receipts.iter().all(|receipt| {
        receipt.matches_terminal(run_id)
            && matches!(
                (task, receipt),
                (
                    MemoryAutomationTaskV1::MemoryCurator,
                    MemoryAutomationCommittedReceiptV1::Curation(_)
                ) | (
                    MemoryAutomationTaskV1::SessionReflector,
                    MemoryAutomationCommittedReceiptV1::AutomaticFact(_)
                )
            )
    });
    if !family_matches {
        return false;
    }

    let mut identities = std::collections::BTreeSet::new();
    receipts.iter().all(|receipt| {
        let identity = match receipt {
            MemoryAutomationCommittedReceiptV1::Curation(receipt) => canonical_sha256(&(
                "tracedecay.memory-automation-run.curation-identity.v1",
                &receipt.receipt.owner,
                &receipt.receipt.operation_id,
            )),
            MemoryAutomationCommittedReceiptV1::AutomaticFact(receipt) => canonical_sha256(&(
                "tracedecay.memory-automation-run.automatic-fact-identity.v1",
                &receipt.owner,
                &receipt.apply_id,
            )),
        };
        identity.is_ok_and(|identity| identities.insert(identity))
    })
}

fn automatic_fact_receipt_matches(run_id: &RunId, receipt: &MemoryAutomationFactReceiptV1) -> bool {
    let state_matches_effect = matches!(
        (receipt.state, &receipt.effect),
        (
            MemoryAutomationFactStateV1::Applied,
            MemoryAutomationFactEffectV1::Applied { .. }
        ) | (
            MemoryAutomationFactStateV1::Quarantined,
            MemoryAutomationFactEffectV1::Quarantined { .. }
        )
    );
    let disposition_matches_state = matches!(
        (receipt.state, receipt.disposition),
        (
            MemoryAutomationFactStateV1::Applied,
            MemoryAutomationFactDispositionV1::Applied
                | MemoryAutomationFactDispositionV1::AlreadyApplied
        ) | (
            MemoryAutomationFactStateV1::Quarantined,
            MemoryAutomationFactDispositionV1::Quarantined
        )
    );
    let target_matches = match &receipt.effect {
        MemoryAutomationFactEffectV1::Applied {
            fact_id, target, ..
        } => target.fact_id == *fact_id && target.owner == receipt.owner,
        MemoryAutomationFactEffectV1::Quarantined { reason } => {
            !reason.trim().is_empty() && reason.len() <= 4_096
        }
    };
    state_matches_effect
        && disposition_matches_state
        && target_matches
        && &receipt.automation_run_id == run_id
        && receipt.request.operation_id.validate().is_ok()
        && receipt
            .request
            .actor
            .as_ref()
            .is_none_or(|actor| actor.validate().is_ok())
        && receipt.request.default_trust_millionths <= 1_000_000
        && matches!(
            receipt.request.sanitization_receipt.disposition(),
            SanitizerDispositionV1::Accepted | SanitizerDispositionV1::Redacted
        )
        && receipt.request.sanitization_receipt.payload().is_some()
        && valid_text(&receipt.request.content, 64 * 1_024)
        && receipt.request.tags.len() <= 20
        && receipt.request.entities.len() <= 20
        && receipt
            .evidence
            .evidence_hash
            .as_deref()
            .is_none_or(|value| valid_text(value, 160))
        && receipt
            .evidence
            .item
            .as_ref()
            .is_none_or(MemoryAutomationFactEvidenceItemV1::matches_terminal)
        && receipt
            .evidence
            .validation
            .as_ref()
            .is_none_or(MemoryAutomationFactValidationV1::matches_terminal)
        && receipt
            .computed_canonical_digest()
            .is_ok_and(|digest| digest == receipt.canonical_digest)
}

impl MemoryAutomationFactReceiptV1 {
    pub fn computed_canonical_digest(&self) -> Result<ManifestDigest, DomainError> {
        let disposition = match self.disposition {
            MemoryAutomationFactDispositionV1::Applied => "applied",
            MemoryAutomationFactDispositionV1::AlreadyApplied => "already_applied",
            MemoryAutomationFactDispositionV1::Quarantined => "quarantined",
        };
        let (fact_id, target_owner, target_fact_id, assertion_id, event_id, reason) =
            match &self.effect {
                MemoryAutomationFactEffectV1::Applied {
                    fact_id,
                    target,
                    assertion_id,
                    event_id,
                } => (
                    Some(fact_id),
                    Some(&target.owner),
                    Some(&target.fact_id),
                    Some(assertion_id),
                    Some(event_id),
                    None,
                ),
                MemoryAutomationFactEffectV1::Quarantined { reason } => {
                    (None, None, None, None, None, Some(reason.as_str()))
                }
            };
        canonical_sha256(&AutomaticFactDigestProjection {
            domain: "tracedecay.project-memory.automatic-fact-apply-result.v1",
            apply_id: &self.apply_id,
            owner: &self.owner,
            state: self.state,
            operation_id: &self.request.operation_id,
            input_digest: self.request.input_digest.as_str(),
            actor: self.request.actor.as_ref(),
            sanitization_receipt: &self.request.sanitization_receipt,
            content: &self.request.content,
            category: self.request.category,
            source_label: self.request.source_label.as_deref(),
            tags: &self.request.tags,
            entities: &self.request.entities,
            default_trust: f64::from(self.request.default_trust_millionths) / 1_000_000.0,
            metadata: &self.request.metadata,
            automation_run_id: Some(self.automation_run_id.as_str()),
            evidence: &self.evidence,
            effect_state: self.state,
            fact_id,
            target_owner,
            target_fact_id,
            assertion_id,
            event_id,
            quarantine_reason: reason,
            recorded_at_micros: self.recorded_at_micros,
            disposition,
        })
    }
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    let value = value.trim();
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

#[cfg(test)]
#[path = "automation/tests.rs"]
mod tests;
