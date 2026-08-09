//! Bounded, evidence-backed adjudication of Work execution leaks.
//!
//! The service never infers a leak from elapsed time, a missing PID, or an
//! attempt state. A mounted evidence owner performs one deadline-bounded scan
//! and returns a typed snapshot; the canonical Work store then publishes a
//! revisioned receipt with compare-and-swap and exact command replay.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CoverageStateV1, LeakOwnerClassV1, ManifestDigest, UtcMicros, WorkAttemptIdentityV1,
    WorkAuthority, WorkCommandId, WorkExecutionLeakKindV1, WorkExecutionLeakObservedV1,
    WorkExecutionLeakRecoveryV1, canonical_sha256,
};

use crate::work::work_authority;
use crate::{
    ApplicationProblem, LegalAction, RequestAdmission, RequestContext, RetryDirective,
    SafeDiagnostic,
};

pub const MAX_WORK_LEAK_EVIDENCE_REFS_V1: usize = 8;
pub const MAX_WORK_LEAK_SCAN_MICROS_V1: u64 = 60_000_000;
pub const MAX_WORK_LEAK_HORIZON_MICROS_V1: u64 = 604_800_000_000;
const LEAK_INPUT_DIGEST_DOMAIN: &str = "tracedecay.application.work-leak-adjudication.v1";

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "AdjudicateWorkLeakCommandV1")]
pub struct AdjudicateWorkLeakCommandV1 {
    pub adjudication_id: String,
    pub expected_revision: Option<u64>,
    pub attempt: WorkAttemptIdentityV1,
    pub detection_horizon_micros: u64,
    pub command_id: WorkCommandId,
}

impl AdjudicateWorkLeakCommandV1 {
    fn validate(&self) -> bool {
        canonical_label(&self.adjudication_id, 256)
            && self.expected_revision.is_none_or(|revision| revision > 0)
            && self.detection_horizon_micros > 0
            && self.detection_horizon_micros <= MAX_WORK_LEAK_HORIZON_MICROS_V1
    }
}

/// Exact result of one scan by the canonical lease/process/effect/placement/
/// delivery evidence owner. Evidence references are opaque local anchors;
/// their owning records remain behind normal authorization.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedWorkLeakEvidenceV1 {
    pub attempt: WorkAttemptIdentityV1,
    pub kind: WorkExecutionLeakKindV1,
    pub recovery: WorkExecutionLeakRecoveryV1,
    pub owner_class: LeakOwnerClassV1,
    pub coverage: CoverageStateV1,
    pub detection_horizon_micros: u64,
    pub scan_started_at: UtcMicros,
    pub scan_completed_at: UtcMicros,
    pub evidence_refs: Vec<String>,
}

impl VerifiedWorkLeakEvidenceV1 {
    fn validate_for(
        &self,
        command: &AdjudicateWorkLeakCommandV1,
        scan_started_at: UtcMicros,
        scan_deadline: UtcMicros,
    ) -> bool {
        self.attempt == command.attempt
            && self.detection_horizon_micros == command.detection_horizon_micros
            && self.scan_started_at == scan_started_at
            && self.scan_completed_at.0 >= self.scan_started_at.0
            && self.scan_completed_at.0 <= scan_deadline.0
            && !self.evidence_refs.is_empty()
            && self.evidence_refs.len() <= MAX_WORK_LEAK_EVIDENCE_REFS_V1
            && self
                .evidence_refs
                .iter()
                .all(|reference| canonical_label(reference, 256))
            && self.evidence_refs.windows(2).all(|pair| pair[0] < pair[1])
            && verdict_matches_coverage(self.kind, self.recovery, self.coverage)
    }

    fn observability_payload(
        &self,
        adjudication_ref: String,
        adjudication_revision: u64,
    ) -> WorkExecutionLeakObservedV1 {
        WorkExecutionLeakObservedV1 {
            adjudication_ref,
            adjudication_revision,
            kind: self.kind,
            detection_horizon_micros: self.detection_horizon_micros,
            recovery: self.recovery,
            owner_class: self.owner_class,
            coverage: self.coverage,
        }
    }
}

fn verdict_matches_coverage(
    kind: WorkExecutionLeakKindV1,
    recovery: WorkExecutionLeakRecoveryV1,
    coverage: CoverageStateV1,
) -> bool {
    match kind {
        WorkExecutionLeakKindV1::None => {
            recovery == WorkExecutionLeakRecoveryV1::NotRequired
                && coverage == CoverageStateV1::Known
        }
        WorkExecutionLeakKindV1::Unknown => {
            recovery == WorkExecutionLeakRecoveryV1::Unknown && coverage == CoverageStateV1::Unknown
        }
        WorkExecutionLeakKindV1::LeaseAfterTerminal
        | WorkExecutionLeakKindV1::AttemptWithoutLiveOwner
        | WorkExecutionLeakKindV1::EffectUnknownPastDeadline
        | WorkExecutionLeakKindV1::MissingWorktreeBinding
        | WorkExecutionLeakKindV1::UnboundedDelivery => coverage == CoverageStateV1::Known,
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WorkLeakEvidenceErrorV1 {
    #[error("Work leak evidence was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("Work leak evidence changed during the bounded scan")]
    Conflict,
    #[error("Work leak evidence scan exceeded its deadline")]
    TimedOut,
    #[error("Work leak evidence authority is unavailable")]
    Unavailable,
}

pub trait WorkLeakEvidencePortV1: Send + Sync {
    fn inspect(
        &self,
        authority: &WorkAuthority,
        command: &AdjudicateWorkLeakCommandV1,
        scan_started_at: UtcMicros,
        scan_deadline: UtcMicros,
    ) -> Result<VerifiedWorkLeakEvidenceV1, WorkLeakEvidenceErrorV1>;
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkLeakAdjudicationReceiptV1 {
    pub command: AdjudicateWorkLeakCommandV1,
    pub revision: u64,
    pub evidence: VerifiedWorkLeakEvidenceV1,
    /// Daemon/request-owned deadline that bounded the source scan.
    pub scan_deadline: UtcMicros,
    pub canonical_input_digest: ManifestDigest,
}

impl WorkLeakAdjudicationReceiptV1 {
    /// Revalidates the complete public receipt before it crosses into an
    /// observability producer or another downstream authority.
    pub fn validate_for_observation(&self) -> bool {
        let Ok(expected_digest) = canonical_sha256(&(
            LEAK_INPUT_DIGEST_DOMAIN,
            &self.command,
            &self.evidence,
            self.scan_deadline,
        )) else {
            return false;
        };
        let bounded_scan = self.scan_deadline.0 >= self.evidence.scan_started_at.0
            && u64::try_from(
                self.scan_deadline
                    .0
                    .saturating_sub(self.evidence.scan_started_at.0),
            )
            .is_ok_and(|duration| duration <= MAX_WORK_LEAK_SCAN_MICROS_V1);
        self.command.validate()
            && bounded_scan
            && self.evidence.validate_for(
                &self.command,
                self.evidence.scan_started_at,
                self.scan_deadline,
            )
            && self.command.expected_revision.unwrap_or(0).checked_add(1) == Some(self.revision)
            && self.canonical_input_digest == expected_digest
            && self
                .observability_payload()
                .is_ok_and(|payload| payload.validate().is_ok())
    }

    pub fn adjudication_ref(
        &self,
    ) -> Result<ManifestDigest, tracedecay_domain::research::DomainError> {
        canonical_sha256(&(
            "tracedecay.work-leak-adjudication-ref.v1",
            &self.command.adjudication_id,
            &self.command.attempt,
        ))
    }

    pub fn observability_payload(
        &self,
    ) -> Result<WorkExecutionLeakObservedV1, tracedecay_domain::research::DomainError> {
        Ok(self
            .evidence
            .observability_payload(self.adjudication_ref()?.as_str().to_owned(), self.revision))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "outcome", content = "receipt", rename_all = "snake_case")]
pub enum WorkLeakAdjudicationOutcomeV1 {
    Appended(WorkLeakAdjudicationReceiptV1),
    Replayed(WorkLeakAdjudicationReceiptV1),
}

impl WorkLeakAdjudicationOutcomeV1 {
    pub const fn receipt(&self) -> &WorkLeakAdjudicationReceiptV1 {
        match self {
            Self::Appended(receipt) | Self::Replayed(receipt) => receipt,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkLeakAdjudicationWriteV1 {
    pub receipt: WorkLeakAdjudicationReceiptV1,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WorkLeakAdjudicationStorageErrorV1 {
    #[error("Work leak adjudication or attempt was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("Work leak adjudication revision changed")]
    RevisionConflict,
    #[error("Work leak adjudication command identity conflicts")]
    IdempotencyConflict,
    #[error("Work leak adjudication authority is unavailable")]
    Unavailable,
}

pub trait WorkLeakAdjudicationStoragePortV1: Send + Sync {
    fn leak_by_command(
        &self,
        authority: &WorkAuthority,
        command_id: &WorkCommandId,
    ) -> Result<Option<WorkLeakAdjudicationReceiptV1>, WorkLeakAdjudicationStorageErrorV1>;

    fn compare_and_record_leak(
        &self,
        authority: &WorkAuthority,
        write: &WorkLeakAdjudicationWriteV1,
    ) -> Result<WorkLeakAdjudicationOutcomeV1, WorkLeakAdjudicationStorageErrorV1>;
}

pub struct WorkLeakAdjudicationServiceV1<S, E> {
    storage: S,
    evidence: E,
}

impl<S, E> WorkLeakAdjudicationServiceV1<S, E>
where
    S: WorkLeakAdjudicationStoragePortV1,
    E: WorkLeakEvidencePortV1,
{
    pub const fn new(storage: S, evidence: E) -> Self {
        Self { storage, evidence }
    }

    pub fn adjudicate(
        &self,
        context: &RequestContext,
        command: AdjudicateWorkLeakCommandV1,
        scan_started_at: UtcMicros,
        scan_deadline: UtcMicros,
    ) -> Result<WorkLeakAdjudicationOutcomeV1, ApplicationProblem> {
        admit(context, scan_started_at)?;
        if !command.validate()
            || scan_deadline.0 < scan_started_at.0
            || u64::try_from(scan_deadline.0.saturating_sub(scan_started_at.0))
                .map_or(true, |duration| duration > MAX_WORK_LEAK_SCAN_MICROS_V1)
        {
            return Err(invalid_problem());
        }
        let authority = work_authority(context)?;
        if let Some(receipt) = self
            .storage
            .leak_by_command(&authority, &command.command_id)
            .map_err(storage_problem)?
        {
            return if receipt.command == command {
                Ok(WorkLeakAdjudicationOutcomeV1::Replayed(receipt))
            } else {
                Err(storage_problem(
                    WorkLeakAdjudicationStorageErrorV1::IdempotencyConflict,
                ))
            };
        }
        let evidence = self
            .evidence
            .inspect(&authority, &command, scan_started_at, scan_deadline)
            .map_err(evidence_problem)?;
        if !evidence.validate_for(&command, scan_started_at, scan_deadline) {
            return Err(conflict_problem(
                "application.work-leak.evidence-conflict",
                "The bounded leak scan did not prove a valid verdict.",
            ));
        }
        let canonical_input_digest =
            canonical_sha256(&(LEAK_INPUT_DIGEST_DOMAIN, &command, &evidence, scan_deadline))
                .map_err(|_| invalid_problem())?;
        let revision = command
            .expected_revision
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(invalid_problem)?;
        self.storage
            .compare_and_record_leak(
                &authority,
                &WorkLeakAdjudicationWriteV1 {
                    receipt: WorkLeakAdjudicationReceiptV1 {
                        command,
                        revision,
                        evidence,
                        scan_deadline,
                        canonical_input_digest,
                    },
                },
            )
            .map_err(storage_problem)
    }
}

fn canonical_label(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'-' | b'_'))
}

fn admit(context: &RequestContext, observed_at: UtcMicros) -> Result<(), ApplicationProblem> {
    match context.admission_at(observed_at) {
        RequestAdmission::Admitted => Ok(()),
        RequestAdmission::Cancelled => Err(ApplicationProblem::cancelled_before_admission()),
        RequestAdmission::TimedOut => Err(ApplicationProblem::timed_out_before_admission()),
    }
}

fn invalid_problem() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "application.work-leak.invalid".to_owned(),
            message: "The Work leak adjudication request is invalid.".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    }
}

fn conflict_problem(code: &str, message: &str) -> ApplicationProblem {
    ApplicationProblem::Conflict {
        diagnostic: SafeDiagnostic {
            code: code.to_owned(),
            message: message.to_owned(),
        },
        retry: RetryDirective::AfterRevalidate,
        legal_actions: vec![LegalAction::Refresh],
    }
}

fn evidence_problem(error: WorkLeakEvidenceErrorV1) -> ApplicationProblem {
    match error {
        WorkLeakEvidenceErrorV1::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        WorkLeakEvidenceErrorV1::Conflict => conflict_problem(
            "application.work-leak.evidence-conflict",
            "The Work leak evidence changed during inspection.",
        ),
        WorkLeakEvidenceErrorV1::TimedOut => ApplicationProblem::TimedOut {
            retry: RetryDirective::AfterRevalidate,
            legal_actions: vec![LegalAction::Refresh],
        },
        WorkLeakEvidenceErrorV1::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.work-leak.evidence-unavailable".to_owned(),
            message: "The Work leak evidence authority is unavailable.".to_owned(),
        }),
    }
}

fn storage_problem(error: WorkLeakAdjudicationStorageErrorV1) -> ApplicationProblem {
    match error {
        WorkLeakAdjudicationStorageErrorV1::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        WorkLeakAdjudicationStorageErrorV1::RevisionConflict => conflict_problem(
            "application.work-leak.revision-conflict",
            "The Work leak adjudication changed before publication.",
        ),
        WorkLeakAdjudicationStorageErrorV1::IdempotencyConflict => conflict_problem(
            "application.work-leak.idempotency-conflict",
            "The Work leak command identity was already used with different input.",
        ),
        WorkLeakAdjudicationStorageErrorV1::Unavailable => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "application.work-leak.unavailable".to_owned(),
                message: "The Work leak adjudication authority is unavailable.".to_owned(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{AttemptId, RunId, TaskId};

    fn valid_receipt() -> WorkLeakAdjudicationReceiptV1 {
        let attempt = WorkAttemptIdentityV1::new(
            TaskId::new("task.leak".to_owned()).expect("task id"),
            RunId::new("run.leak".to_owned()).expect("run id"),
            AttemptId::new("attempt.leak".to_owned()).expect("attempt id"),
        )
        .expect("attempt identity");
        let command = AdjudicateWorkLeakCommandV1 {
            adjudication_id: "adjudication.leak".to_owned(),
            expected_revision: None,
            attempt: attempt.clone(),
            detection_horizon_micros: 1_000,
            command_id: WorkCommandId::new("command.leak".to_owned()).expect("command id"),
        };
        let evidence = VerifiedWorkLeakEvidenceV1 {
            attempt,
            kind: WorkExecutionLeakKindV1::AttemptWithoutLiveOwner,
            recovery: WorkExecutionLeakRecoveryV1::Pending,
            owner_class: LeakOwnerClassV1::Work,
            coverage: CoverageStateV1::Known,
            detection_horizon_micros: command.detection_horizon_micros,
            scan_started_at: UtcMicros(1_010),
            scan_completed_at: UtcMicros(1_020),
            evidence_refs: vec!["attempt:canonical".to_owned(), "owner:absent".to_owned()],
        };
        WorkLeakAdjudicationReceiptV1 {
            scan_deadline: UtcMicros(1_100),
            canonical_input_digest: canonical_sha256(&(
                LEAK_INPUT_DIGEST_DOMAIN,
                &command,
                &evidence,
                UtcMicros(1_100),
            ))
            .expect("input digest"),
            command,
            revision: 1,
            evidence,
        }
    }

    #[test]
    fn observation_validation_accepts_only_complete_bounded_receipt() {
        let mut receipt = valid_receipt();
        assert!(receipt.validate_for_observation());

        receipt.evidence.evidence_refs.reverse();
        assert!(!receipt.validate_for_observation());
    }

    #[test]
    fn observation_validation_rejects_revision_overflow_and_unknown_known_verdict() {
        let mut receipt = valid_receipt();
        receipt.command.expected_revision = Some(u64::MAX);
        receipt.revision = 0;
        assert!(!receipt.validate_for_observation());

        let mut receipt = valid_receipt();
        receipt.evidence.kind = WorkExecutionLeakKindV1::Unknown;
        assert!(!receipt.validate_for_observation());
    }
}
