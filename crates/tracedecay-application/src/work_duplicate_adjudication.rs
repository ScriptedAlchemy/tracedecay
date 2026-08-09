//! Application owner for explicit duplicate-Work adjudication.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ActorId, DuplicateEffortKindV1, ManifestDigest, ProjectionGenerationId, UtcMicros,
    WorkAttemptIdentityV1, WorkAuthority, WorkCommandId, WorkDuplicateAdjudicationCommandV1,
    WorkDuplicateAdjudicationContractErrorV1, WorkDuplicateAdjudicationEvidenceV1,
    WorkDuplicateAdjudicationQuantitiesV1, WorkDuplicateAdjudicationReceiptV1,
    WorkTopologyGenerationRefV1,
};

use crate::work::work_authority;
use crate::{
    ApplicationProblem, LegalAction, RequestAdmission, RequestContext, RetryDirective,
    SafeDiagnostic,
};

pub fn work_duplicate_adjudication_input_digest(
    command: &WorkDuplicateAdjudicationCommandV1,
) -> Result<ManifestDigest, tracedecay_domain::research::DomainError> {
    command.canonical_input_digest()
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkDuplicateAdjudicationStorageErrorV1 {
    #[error("duplicate Work adjudication or attempt was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("duplicate Work adjudication revision changed")]
    RevisionConflict,
    #[error("duplicate Work adjudication command identity conflicts")]
    IdempotencyConflict,
    #[error("duplicate Work adjudication authority is unavailable")]
    Unavailable,
}

pub const MAX_WORK_DUPLICATE_CLASSIFICATION_ATTEMPTS_V1: usize = 64;

/// Operator judgment before the owning Work authority binds exact current
/// generations, relation revision, command identity, and observation time.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrepareWorkDuplicateAdjudicationRequestV1 {
    pub first_attempt: WorkAttemptIdentityV1,
    pub second_attempt: WorkAttemptIdentityV1,
    pub verdict: DuplicateEffortKindV1,
    pub quantities: WorkDuplicateAdjudicationQuantitiesV1,
    pub reason: String,
}

pub fn prepare_work_duplicate_adjudication(
    request: PrepareWorkDuplicateAdjudicationRequestV1,
    evidence: WorkDuplicateAdjudicationEvidenceV1,
    latest: Option<&WorkDuplicateAdjudicationReceiptV1>,
    command_id: WorkCommandId,
    occurred_at: UtcMicros,
) -> Result<WorkDuplicateAdjudicationCommandV1, WorkDuplicateAdjudicationContractErrorV1> {
    let command = WorkDuplicateAdjudicationCommandV1 {
        expected_revision: latest.map(WorkDuplicateAdjudicationReceiptV1::revision),
        first_attempt: request.first_attempt,
        second_attempt: request.second_attempt,
        evidence,
        verdict: request.verdict,
        quantities: request.quantities,
        reason: request.reason,
        command_id,
        occurred_at,
    }
    .canonicalized();
    if latest.is_some_and(|receipt| {
        receipt.command().first_attempt != command.first_attempt
            || receipt.command().second_attempt != command.second_attempt
    }) {
        return Err(WorkDuplicateAdjudicationContractErrorV1::InvalidReceipt);
    }
    command.validate()?;
    Ok(command)
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkDuplicateAttemptClassificationRequestV1 {
    pub work_generation: ProjectionGenerationId,
    pub topology_generation: WorkTopologyGenerationRefV1,
    pub attempts: Vec<WorkAttemptIdentityV1>,
    pub observed_at: UtcMicros,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkDuplicateClassificationUnavailableReasonV1 {
    MissingPair,
    ConflictingPair,
    UnresolvedVerdict,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkDuplicateAttemptClassificationV1 {
    pub work_generation: ProjectionGenerationId,
    pub topology_generation: WorkTopologyGenerationRefV1,
    pub attempts: Vec<WorkAttemptIdentityV1>,
    pub duplicate_attempts: Vec<WorkAttemptIdentityV1>,
    pub non_duplicate_attempts: Vec<WorkAttemptIdentityV1>,
    pub relation_receipts: Vec<WorkDuplicateAdjudicationReceiptV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case")]
pub enum WorkDuplicateAttemptClassificationReadV1 {
    Complete {
        classification: WorkDuplicateAttemptClassificationV1,
    },
    Unavailable {
        reason: WorkDuplicateClassificationUnavailableReasonV1,
    },
}

impl WorkDuplicateAttemptClassificationReadV1 {
    pub const fn complete(&self) -> Option<&WorkDuplicateAttemptClassificationV1> {
        match self {
            Self::Complete { classification } => Some(classification),
            Self::Unavailable { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkDuplicateAdjudicationWriteV1 {
    pub actor_id: ActorId,
    pub command: WorkDuplicateAdjudicationCommandV1,
    pub canonical_input_digest: ManifestDigest,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "outcome", content = "receipt", rename_all = "snake_case")]
pub enum WorkDuplicateAdjudicationAppendOutcomeV1 {
    Appended(WorkDuplicateAdjudicationReceiptV1),
    Replayed(WorkDuplicateAdjudicationReceiptV1),
}

impl WorkDuplicateAdjudicationAppendOutcomeV1 {
    pub const fn receipt(&self) -> &WorkDuplicateAdjudicationReceiptV1 {
        match self {
            Self::Appended(receipt) | Self::Replayed(receipt) => receipt,
        }
    }

    pub const fn replayed(&self) -> bool {
        matches!(self, Self::Replayed(_))
    }
}

pub trait WorkDuplicateAdjudicationPortV1: Send + Sync {
    fn compare_and_record_duplicate_adjudication(
        &self,
        authority: &WorkAuthority,
        write: &WorkDuplicateAdjudicationWriteV1,
    ) -> Result<WorkDuplicateAdjudicationAppendOutcomeV1, WorkDuplicateAdjudicationStorageErrorV1>;

    fn latest_duplicate_adjudications_for_attempts(
        &self,
        authority: &WorkAuthority,
        work_generation: &ProjectionGenerationId,
        topology_generation: &WorkTopologyGenerationRefV1,
        attempts: &[WorkAttemptIdentityV1],
    ) -> Result<Vec<WorkDuplicateAdjudicationReceiptV1>, WorkDuplicateAdjudicationStorageErrorV1>;

    fn latest_duplicate_adjudication_for_pair(
        &self,
        authority: &WorkAuthority,
        first_attempt: &WorkAttemptIdentityV1,
        second_attempt: &WorkAttemptIdentityV1,
    ) -> Result<Option<WorkDuplicateAdjudicationReceiptV1>, WorkDuplicateAdjudicationStorageErrorV1>;
}

pub struct WorkDuplicateAdjudicationServiceV1<S> {
    storage: S,
}

impl<S> WorkDuplicateAdjudicationServiceV1<S>
where
    S: WorkDuplicateAdjudicationPortV1,
{
    pub const fn new(storage: S) -> Self {
        Self { storage }
    }

    pub fn adjudicate(
        &self,
        context: &RequestContext,
        command: WorkDuplicateAdjudicationCommandV1,
    ) -> Result<WorkDuplicateAdjudicationAppendOutcomeV1, ApplicationProblem> {
        admit(context, command.occurred_at)?;
        let authority = work_authority(context)?;
        let command = command.canonicalized();
        command.validate().map_err(|_| invalid_problem())?;
        let canonical_input_digest =
            work_duplicate_adjudication_input_digest(&command).map_err(|_| invalid_problem())?;
        self.storage
            .compare_and_record_duplicate_adjudication(
                &authority,
                &WorkDuplicateAdjudicationWriteV1 {
                    actor_id: context.actor().clone(),
                    command,
                    canonical_input_digest,
                },
            )
            .map_err(storage_problem)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_adjudication(
        &self,
        context: &RequestContext,
        request: PrepareWorkDuplicateAdjudicationRequestV1,
        evidence: WorkDuplicateAdjudicationEvidenceV1,
        command_id: WorkCommandId,
        occurred_at: UtcMicros,
    ) -> Result<WorkDuplicateAdjudicationCommandV1, ApplicationProblem> {
        admit(context, occurred_at)?;
        let authority = work_authority(context)?;
        if request.first_attempt == request.second_attempt {
            return Err(invalid_problem());
        }
        let (first_attempt, second_attempt) = if request.first_attempt <= request.second_attempt {
            (&request.first_attempt, &request.second_attempt)
        } else {
            (&request.second_attempt, &request.first_attempt)
        };
        let latest = self
            .storage
            .latest_duplicate_adjudication_for_pair(&authority, first_attempt, second_attempt)
            .map_err(storage_problem)?;
        prepare_work_duplicate_adjudication(
            request,
            evidence,
            latest.as_ref(),
            command_id,
            occurred_at,
        )
        .map_err(|_| invalid_problem())
    }

    /// Classifies useful attempts only with a complete exact pair matrix at
    /// one pinned Work projection and topology generation. Missing,
    /// conflicting, censored, and unknown relations stay explicitly
    /// unavailable.
    pub fn classify_attempts(
        &self,
        context: &RequestContext,
        request: WorkDuplicateAttemptClassificationRequestV1,
    ) -> Result<WorkDuplicateAttemptClassificationReadV1, ApplicationProblem> {
        admit(context, request.observed_at)?;
        let authority = work_authority(context)?;
        let mut attempts = request.attempts;
        attempts.sort();
        if attempts.len() > MAX_WORK_DUPLICATE_CLASSIFICATION_ATTEMPTS_V1
            || attempts.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(invalid_problem());
        }
        let receipts = self
            .storage
            .latest_duplicate_adjudications_for_attempts(
                &authority,
                &request.work_generation,
                &request.topology_generation,
                &attempts,
            )
            .map_err(storage_problem)?;
        Ok(classify_complete_attempt_relations(
            &authority,
            request.work_generation,
            request.topology_generation,
            attempts,
            receipts,
        ))
    }
}

fn classify_complete_attempt_relations(
    authority: &WorkAuthority,
    work_generation: ProjectionGenerationId,
    topology_generation: WorkTopologyGenerationRefV1,
    attempts: Vec<WorkAttemptIdentityV1>,
    receipts: Vec<WorkDuplicateAdjudicationReceiptV1>,
) -> WorkDuplicateAttemptClassificationReadV1 {
    let attempt_set = attempts.iter().cloned().collect::<BTreeSet<_>>();
    let mut by_pair = BTreeMap::new();
    for receipt in receipts {
        let canonical = WorkDuplicateAdjudicationReceiptV1::new(
            authority,
            receipt.command().clone(),
            receipt.revision(),
            receipt.canonical_input_digest().clone(),
        );
        let pair = (
            receipt.command().first_attempt.clone(),
            receipt.command().second_attempt.clone(),
        );
        if canonical.as_ref() != Ok(&receipt)
            || receipt.command().evidence.topology_generation != topology_generation
            || receipt.command().evidence.work_generation != work_generation
            || !attempt_set.contains(&pair.0)
            || !attempt_set.contains(&pair.1)
            || by_pair.insert(pair, receipt).is_some()
        {
            return WorkDuplicateAttemptClassificationReadV1::Unavailable {
                reason: WorkDuplicateClassificationUnavailableReasonV1::ConflictingPair,
            };
        }
    }

    let mut duplicate_attempts = BTreeSet::new();
    let mut relation_receipts = Vec::new();
    for (index, first) in attempts.iter().enumerate() {
        for second in &attempts[index + 1..] {
            let Some(receipt) = by_pair.remove(&(first.clone(), second.clone())) else {
                return WorkDuplicateAttemptClassificationReadV1::Unavailable {
                    reason: WorkDuplicateClassificationUnavailableReasonV1::MissingPair,
                };
            };
            match receipt.command().verdict {
                DuplicateEffortKindV1::ExactDuplicate
                | DuplicateEffortKindV1::SupersededOverlap
                | DuplicateEffortKindV1::RepeatedInvestigation
                | DuplicateEffortKindV1::DuplicateEffect => {
                    duplicate_attempts.insert(first.clone());
                    duplicate_attempts.insert(second.clone());
                }
                DuplicateEffortKindV1::NotDuplicate => {}
                DuplicateEffortKindV1::Censored | DuplicateEffortKindV1::Unknown => {
                    return WorkDuplicateAttemptClassificationReadV1::Unavailable {
                        reason: WorkDuplicateClassificationUnavailableReasonV1::UnresolvedVerdict,
                    };
                }
            }
            relation_receipts.push(receipt);
        }
    }
    let duplicate_attempts = duplicate_attempts.into_iter().collect::<Vec<_>>();
    let non_duplicate_attempts = attempts
        .iter()
        .filter(|attempt| duplicate_attempts.binary_search(attempt).is_err())
        .cloned()
        .collect();
    WorkDuplicateAttemptClassificationReadV1::Complete {
        classification: WorkDuplicateAttemptClassificationV1 {
            work_generation,
            topology_generation,
            attempts,
            duplicate_attempts,
            non_duplicate_attempts,
            relation_receipts,
        },
    }
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
            code: "application.work.duplicate-adjudication.invalid".to_owned(),
            message: "The duplicate Work adjudication is invalid.".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    }
}

fn storage_problem(error: WorkDuplicateAdjudicationStorageErrorV1) -> ApplicationProblem {
    match error {
        WorkDuplicateAdjudicationStorageErrorV1::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        WorkDuplicateAdjudicationStorageErrorV1::RevisionConflict => ApplicationProblem::Conflict {
            diagnostic: SafeDiagnostic {
                code: "application.work.duplicate-adjudication.revision-conflict".to_owned(),
                message: "The duplicate Work adjudication changed after this command was prepared."
                    .to_owned(),
            },
            retry: RetryDirective::AfterRevalidate,
            legal_actions: vec![LegalAction::Refresh],
        },
        WorkDuplicateAdjudicationStorageErrorV1::IdempotencyConflict => {
            ApplicationProblem::Conflict {
                diagnostic: SafeDiagnostic {
                    code: "application.work.duplicate-adjudication.idempotency-conflict".to_owned(),
                    message: "The duplicate Work adjudication command identity was already used with different input."
                        .to_owned(),
                },
                retry: RetryDirective::AfterRevalidate,
                legal_actions: vec![LegalAction::Refresh],
            }
        }
        WorkDuplicateAdjudicationStorageErrorV1::Unavailable => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "application.work.duplicate-adjudication.unavailable".to_owned(),
                message: "The duplicate Work adjudication authority is unavailable.".to_owned(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        ActorId, AttemptId, CoverageStateV1, DuplicateEffectOutcomeV1, QuantityEvidenceClassV1,
        RunId, TaskId, WorkCommandId, WorkDuplicateAdjudicationEvidenceV1,
        WorkDuplicateAdjudicationQuantitiesV1, WorkDuplicateAdjudicationRevisionV1,
        WorkTopologyGenerationRefV1,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn attempt(name: &str) -> WorkAttemptIdentityV1 {
        WorkAttemptIdentityV1::new(
            id::<TaskId>(&format!("task.{name}")),
            id::<RunId>(&format!("run.{name}")),
            id::<AttemptId>(&format!("attempt.{name}")),
        )
        .unwrap()
    }

    fn topology_ref(byte: char) -> WorkTopologyGenerationRefV1 {
        id(&format!("sha256:{}", byte.to_string().repeat(64)))
    }

    fn receipt(
        authority: &WorkAuthority,
        name: &str,
        first: WorkAttemptIdentityV1,
        second: WorkAttemptIdentityV1,
        generation: &WorkTopologyGenerationRefV1,
        verdict: DuplicateEffortKindV1,
    ) -> WorkDuplicateAdjudicationReceiptV1 {
        let coverage = match verdict {
            DuplicateEffortKindV1::Unknown => CoverageStateV1::Unknown,
            DuplicateEffortKindV1::Censored => CoverageStateV1::Partial,
            _ => CoverageStateV1::Known,
        };
        let command = WorkDuplicateAdjudicationCommandV1 {
            expected_revision: None,
            first_attempt: first,
            second_attempt: second,
            evidence: WorkDuplicateAdjudicationEvidenceV1 {
                work_generation: id::<ProjectionGenerationId>("generation.work.test"),
                topology_generation: generation.clone(),
            },
            verdict,
            quantities: WorkDuplicateAdjudicationQuantitiesV1 {
                wall_micros: None,
                token_count: None,
                cost_micros: None,
                test_count: None,
                effect_count: None,
                evidence: QuantityEvidenceClassV1::OwnerReceipt,
                effect_outcome: DuplicateEffectOutcomeV1::NotApplicable,
                coverage,
            },
            reason: "independent pair review".to_owned(),
            command_id: id::<WorkCommandId>(&format!("command.{name}")),
            occurred_at: UtcMicros(10),
        }
        .canonicalized();
        let digest = command.canonical_input_digest().unwrap();
        WorkDuplicateAdjudicationReceiptV1::new(
            authority,
            command,
            WorkDuplicateAdjudicationRevisionV1::initial(),
            digest,
        )
        .unwrap()
    }

    #[test]
    fn useful_attempt_classification_requires_every_resolved_pair() {
        let authority = WorkAuthority::new(
            id("project.classification.test"),
            id("repository.classification.test"),
            id("worktree.classification.test"),
            id::<ActorId>("actor.classification.test"),
            id("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        )
        .unwrap();
        let generation = topology_ref('1');
        let attempts = vec![attempt("a"), attempt("b"), attempt("c")];
        let ab = receipt(
            &authority,
            "ab",
            attempts[0].clone(),
            attempts[1].clone(),
            &generation,
            DuplicateEffortKindV1::ExactDuplicate,
        );
        let ac = receipt(
            &authority,
            "ac",
            attempts[0].clone(),
            attempts[2].clone(),
            &generation,
            DuplicateEffortKindV1::NotDuplicate,
        );
        assert!(matches!(
            classify_complete_attempt_relations(
                &authority,
                id("generation.work.test"),
                generation.clone(),
                attempts.clone(),
                vec![ab.clone(), ac.clone()]
            ),
            WorkDuplicateAttemptClassificationReadV1::Unavailable {
                reason: WorkDuplicateClassificationUnavailableReasonV1::MissingPair
            }
        ));
        let unresolved = receipt(
            &authority,
            "bc-unknown",
            attempts[1].clone(),
            attempts[2].clone(),
            &generation,
            DuplicateEffortKindV1::Unknown,
        );
        assert!(matches!(
            classify_complete_attempt_relations(
                &authority,
                id("generation.work.test"),
                generation.clone(),
                attempts.clone(),
                vec![ab.clone(), ac.clone(), unresolved]
            ),
            WorkDuplicateAttemptClassificationReadV1::Unavailable {
                reason: WorkDuplicateClassificationUnavailableReasonV1::UnresolvedVerdict
            }
        ));
        let bc = receipt(
            &authority,
            "bc",
            attempts[1].clone(),
            attempts[2].clone(),
            &generation,
            DuplicateEffortKindV1::NotDuplicate,
        );
        let complete = classify_complete_attempt_relations(
            &authority,
            id("generation.work.test"),
            generation,
            attempts.clone(),
            vec![ab, ac, bc],
        );
        let classification = complete.complete().unwrap();
        assert_eq!(classification.duplicate_attempts, attempts[..2]);
        assert_eq!(classification.non_duplicate_attempts, attempts[2..]);
        assert_eq!(classification.relation_receipts.len(), 3);
    }

    #[test]
    fn duplicate_preparation_binds_current_generations_and_latest_relation_revision() {
        let authority = WorkAuthority::new(
            id("project.prepare.test"),
            id("repository.prepare.test"),
            id("worktree.prepare.test"),
            id::<ActorId>("actor.prepare.test"),
            id("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        )
        .unwrap();
        let first = attempt("a");
        let second = attempt("b");
        let previous = receipt(
            &authority,
            "previous",
            first.clone(),
            second.clone(),
            &topology_ref('2'),
            DuplicateEffortKindV1::NotDuplicate,
        );
        let quantities = WorkDuplicateAdjudicationQuantitiesV1 {
            wall_micros: Some(100),
            token_count: None,
            cost_micros: None,
            test_count: None,
            effect_count: None,
            evidence: QuantityEvidenceClassV1::OwnerReceipt,
            effect_outcome: DuplicateEffectOutcomeV1::NotApplicable,
            coverage: CoverageStateV1::Known,
        };

        let prepared = prepare_work_duplicate_adjudication(
            PrepareWorkDuplicateAdjudicationRequestV1 {
                first_attempt: second.clone(),
                second_attempt: first.clone(),
                verdict: DuplicateEffortKindV1::SupersededOverlap,
                quantities: quantities.clone(),
                reason: "independent operator review".to_owned(),
            },
            WorkDuplicateAdjudicationEvidenceV1 {
                work_generation: id("generation.work.current"),
                topology_generation: topology_ref('3'),
            },
            Some(&previous),
            id("command.duplicate.prepared"),
            UtcMicros(20),
        )
        .unwrap();

        assert_eq!(prepared.first_attempt, first);
        assert_eq!(prepared.second_attempt, second);
        assert_eq!(prepared.expected_revision, Some(previous.revision()));
        assert_eq!(
            prepared.evidence.work_generation.as_str(),
            "generation.work.current"
        );
        assert_eq!(
            prepared.evidence.topology_generation.as_str(),
            &format!("sha256:{}", "3".repeat(64))
        );
        assert_eq!(prepared.quantities, quantities);
        assert_eq!(prepared.command_id.as_str(), "command.duplicate.prepared");
        assert_eq!(prepared.occurred_at, UtcMicros(20));
    }
}
