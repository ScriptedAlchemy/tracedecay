#![allow(
    clippy::result_large_err,
    reason = "the sealed problem envelope is the canonical pre-admission boundary contract"
)]

use std::future::Future;

use tracedecay_domain::UtcMicros;
use tracedecay_policy::authorization::SourceAuthorizationEvaluator;

use crate::authorization::{AuthorizationAdmission, AuthorizationPort, AuthorizationService};
use crate::context::{RequestAdmission, RequestContext};
use crate::handlers::ApplicationOperation;
use crate::result::{
    ApplicationEnvelope, ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult,
    AuthorityReceipt, CancellationObservation, CancellationStage, CoverageCompleteness,
    EvidencePacket, FreshnessState, Omission, OmissionReason, OperationReceipt,
    OperationTermination, RetrievalEvidence, SafeDiagnostic,
};

use super::RetrievalPortOutcome;

pub(super) fn problem_envelope<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
    problem: ApplicationProblem,
) -> ApplicationResult<T> {
    Err(ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        context.request_id().clone(),
        problem,
    ))
}

pub(super) fn evidence_envelope<T, A, E>(
    context: &RequestContext,
    operation: &ApplicationOperation,
    authorization: &AuthorizationService<A, E>,
    admission: &AuthorizationAdmission,
    outcome: RetrievalPortOutcome<T>,
    started_at: UtcMicros,
) -> ApplicationResult<T>
where
    A: AuthorizationPort,
    E: SourceAuthorizationEvaluator,
{
    let mut prepared = prepare_evidence_for_publication(context, outcome);
    let mut authority = admission.receipt().clone();
    if prepared.requires_recheck {
        match authorization.recheck_publication(
            context,
            operation,
            admission,
            prepared.evidence.finished_at,
        ) {
            Ok(rechecked) => authority = rechecked,
            Err(_) => prepared.deny_publication(),
        }
    }
    finish_evidence_envelope(context, operation, authority, prepared, started_at)
}

pub(super) async fn evidence_envelope_with_async_publication_recheck<T, F, Fut>(
    context: &RequestContext,
    operation: &ApplicationOperation,
    admission_receipt: &AuthorityReceipt,
    outcome: RetrievalPortOutcome<T>,
    started_at: UtcMicros,
    recheck_publication: F,
) -> ApplicationResult<T>
where
    F: FnOnce(UtcMicros) -> Fut,
    Fut: Future<Output = Result<AuthorityReceipt, ApplicationProblem>>,
{
    let mut prepared = prepare_evidence_for_publication(context, outcome);
    let mut authority = admission_receipt.clone();
    if prepared.requires_recheck {
        match recheck_publication(prepared.evidence.finished_at).await {
            Ok(rechecked) => authority = rechecked,
            Err(_) => prepared.deny_publication(),
        }
    }
    finish_evidence_envelope(context, operation, authority, prepared, started_at)
}

struct PreparedEvidence<T> {
    termination: OperationTermination,
    evidence: RetrievalEvidence<T>,
    requires_recheck: bool,
}

impl<T> PreparedEvidence<T> {
    fn deny_publication(&mut self) {
        self.termination = OperationTermination::Failed;
        suppress_unpublished_evidence(&mut self.evidence, OmissionReason::Redacted, None);
        self.requires_recheck = false;
    }
}

fn prepare_evidence_for_publication<T>(
    context: &RequestContext,
    outcome: RetrievalPortOutcome<T>,
) -> PreparedEvidence<T> {
    let (mut termination, mut evidence) = match outcome {
        RetrievalPortOutcome::Completed(evidence) => (OperationTermination::Completed, evidence),
        RetrievalPortOutcome::Partial(evidence) => (OperationTermination::Partial, evidence),
        RetrievalPortOutcome::Cancelled(evidence) => (OperationTermination::Cancelled, evidence),
        RetrievalPortOutcome::TimedOut(evidence) => (OperationTermination::TimedOut, evidence),
        RetrievalPortOutcome::Failed(evidence) => (OperationTermination::Failed, evidence),
        RetrievalPortOutcome::Unavailable(evidence) => {
            (OperationTermination::Unavailable, evidence)
        }
    };
    let mut requires_recheck = false;
    let terminal_override = match termination {
        OperationTermination::Cancelled => Some((
            OperationTermination::Cancelled,
            OmissionReason::Cancelled,
            evidence
                .cancellation
                .clone()
                .or(Some(CancellationObservation {
                    stage: CancellationStage::DuringRead,
                    observed_at: evidence.finished_at,
                })),
        )),
        OperationTermination::TimedOut => Some((
            OperationTermination::TimedOut,
            OmissionReason::TimedOut,
            evidence
                .cancellation
                .clone()
                .or(Some(CancellationObservation {
                    stage: CancellationStage::DuringRead,
                    observed_at: evidence.finished_at,
                })),
        )),
        _ => match context.admission_at(evidence.finished_at) {
            RequestAdmission::Cancelled => Some((
                OperationTermination::Cancelled,
                OmissionReason::Cancelled,
                Some(CancellationObservation {
                    stage: CancellationStage::DuringRead,
                    observed_at: evidence.finished_at,
                }),
            )),
            RequestAdmission::TimedOut => Some((
                OperationTermination::TimedOut,
                OmissionReason::TimedOut,
                Some(CancellationObservation {
                    stage: CancellationStage::DuringRead,
                    observed_at: evidence.finished_at,
                }),
            )),
            RequestAdmission::Admitted => {
                requires_recheck = true;
                None
            }
        },
    };
    if let Some((override_termination, reason, cancellation)) = terminal_override {
        termination = override_termination;
        suppress_unpublished_evidence(&mut evidence, reason, cancellation);
    }
    PreparedEvidence {
        termination,
        evidence,
        requires_recheck,
    }
}

fn finish_evidence_envelope<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
    authority: AuthorityReceipt,
    prepared: PreparedEvidence<T>,
    started_at: UtcMicros,
) -> ApplicationResult<T> {
    let PreparedEvidence {
        termination,
        evidence,
        ..
    } = prepared;
    let execution = OperationReceipt {
        started_at,
        ended_at: evidence.finished_at,
        effective_deadline: context.deadline().clone(),
        cancellation: evidence.cancellation.clone(),
        budget: evidence.budget,
        termination,
    };
    let packet = match EvidencePacket::from_retrieval(evidence, authority, execution) {
        Ok(packet) => packet,
        Err(_) => {
            return problem_envelope(
                context,
                operation,
                ApplicationProblem::unavailable(
                    SafeDiagnostic::new(
                        "application.retrieval.invalid-port-evidence",
                        "The retrieval result could not be verified.",
                    )
                    .expect("static safe diagnostic is valid"),
                ),
            );
        }
    };
    Ok(ApplicationEnvelope::evidence(
        operation.result_contract().clone(),
        context.request_id().clone(),
        context.scope().clone(),
        packet,
    ))
}

fn suppress_unpublished_evidence<T>(
    evidence: &mut RetrievalEvidence<T>,
    reason: OmissionReason,
    cancellation: Option<CancellationObservation>,
) {
    evidence.payload = None;
    evidence.temporal.freshness = FreshnessState::Unknown;
    evidence.evidence_authorities.clear();
    evidence.coverage.visited = None;
    evidence.coverage.eligible = None;
    evidence.coverage.returned = 0;
    evidence.coverage.completeness = CoverageCompleteness::Unknown;
    for domain in &mut evidence.coverage.domains {
        domain.completeness = CoverageCompleteness::Unknown;
    }
    evidence.omissions = evidence
        .coverage
        .requested_domains
        .iter()
        .copied()
        .map(|domain| Omission {
            domain,
            count: 0,
            reason,
        })
        .collect();
    evidence.scores.clear();
    evidence.contributions.clear();
    evidence.page.total = None;
    evidence.page.returned = 0;
    evidence.page.cursor = None;
    evidence.page.expires_at = None;
    evidence.cancellation = cancellation;
}
