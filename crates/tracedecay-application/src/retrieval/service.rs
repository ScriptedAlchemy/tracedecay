#![allow(
    clippy::result_large_err,
    reason = "the sealed problem envelope is the canonical pre-admission boundary contract"
)]

use tracedecay_domain::UtcMicros;
use tracedecay_policy::authorization::SourceAuthorizationEvaluator;

use crate::authorization::{AuthorizationAdmission, AuthorizationPort, AuthorizationService};
use crate::context::{RequestAdmission, RequestContext};
use crate::handlers::ApplicationOperation;
use crate::result::{
    ApplicationEnvelope, ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult,
    CancellationObservation, CancellationStage, CoverageCompleteness, EvidencePacket,
    FreshnessState, Omission, OmissionReason, OperationReceipt, OperationTermination,
    RetrievalEvidence, SafeDiagnostic,
};

use super::{
    AffectedTestsRequest, AffectedTestsResult, AffectedTestsRetrievalPort, GraphCallersRequest,
    GraphCallersResult, GraphRetrievalPort, RetrievalPortContext, RetrievalPortOutcome,
    SourceLinesRequest, SourceLinesResult, SourceRetrievalPort, SymbolRetrievalPort,
    SymbolSearchRequest, SymbolSearchResult,
};

/// Direct typed service for the PR9-composed symbol search lane.
pub struct SymbolSearchService<P, A, E> {
    port: P,
    authorization: AuthorizationService<A, E>,
    operation: ApplicationOperation,
}

impl<P, A, E> SymbolSearchService<P, A, E>
where
    P: SymbolRetrievalPort,
    A: AuthorizationPort,
    E: SourceAuthorizationEvaluator,
{
    pub fn new(
        port: P,
        authorization: AuthorizationService<A, E>,
        operation: ApplicationOperation,
    ) -> Self {
        Self {
            port,
            authorization,
            operation,
        }
    }

    pub fn execute(
        &self,
        context: &RequestContext,
        request: SymbolSearchRequest,
        observed_at: UtcMicros,
    ) -> ApplicationResult<SymbolSearchResult> {
        let admission = match self
            .authorization
            .admit(context, &self.operation, observed_at)
        {
            Ok(admission) => admission,
            Err(problem) => return problem_envelope(context, &self.operation, problem),
        };
        let outcome = self.port.symbol_search(
            &RetrievalPortContext {
                request: context,
                operation: &self.operation,
            },
            &request,
        );
        evidence_envelope(
            context,
            &self.operation,
            &self.authorization,
            &admission,
            outcome,
            observed_at,
        )
    }
}

/// Direct typed service for bounded source-line retrieval.
pub struct SourceLinesService<P, A, E> {
    port: P,
    authorization: AuthorizationService<A, E>,
    operation: ApplicationOperation,
}

impl<P, A, E> SourceLinesService<P, A, E>
where
    P: SourceRetrievalPort,
    A: AuthorizationPort,
    E: SourceAuthorizationEvaluator,
{
    pub fn new(
        port: P,
        authorization: AuthorizationService<A, E>,
        operation: ApplicationOperation,
    ) -> Self {
        Self {
            port,
            authorization,
            operation,
        }
    }

    pub fn execute(
        &self,
        context: &RequestContext,
        request: SourceLinesRequest,
        observed_at: UtcMicros,
    ) -> ApplicationResult<SourceLinesResult> {
        let admission = match self
            .authorization
            .admit(context, &self.operation, observed_at)
        {
            Ok(admission) => admission,
            Err(problem) => return problem_envelope(context, &self.operation, problem),
        };
        let outcome = self.port.source_lines(
            &RetrievalPortContext {
                request: context,
                operation: &self.operation,
            },
            &request,
        );
        evidence_envelope(
            context,
            &self.operation,
            &self.authorization,
            &admission,
            outcome,
            observed_at,
        )
    }
}

/// Direct typed service for graph callers. It invokes one graph port only.
pub struct GraphCallersService<P, A, E> {
    port: P,
    authorization: AuthorizationService<A, E>,
    operation: ApplicationOperation,
}

impl<P, A, E> GraphCallersService<P, A, E>
where
    P: GraphRetrievalPort,
    A: AuthorizationPort,
    E: SourceAuthorizationEvaluator,
{
    pub fn new(
        port: P,
        authorization: AuthorizationService<A, E>,
        operation: ApplicationOperation,
    ) -> Self {
        Self {
            port,
            authorization,
            operation,
        }
    }

    pub fn execute(
        &self,
        context: &RequestContext,
        request: GraphCallersRequest,
        observed_at: UtcMicros,
    ) -> ApplicationResult<GraphCallersResult> {
        let admission = match self
            .authorization
            .admit(context, &self.operation, observed_at)
        {
            Ok(admission) => admission,
            Err(problem) => return problem_envelope(context, &self.operation, problem),
        };
        let outcome = self.port.graph_callers(
            &RetrievalPortContext {
                request: context,
                operation: &self.operation,
            },
            &request,
        );
        evidence_envelope(
            context,
            &self.operation,
            &self.authorization,
            &admission,
            outcome,
            observed_at,
        )
    }
}

/// Direct typed service for Plan-05/PR9 affected-test evidence.
pub struct AffectedTestsService<P, A, E> {
    port: P,
    authorization: AuthorizationService<A, E>,
    operation: ApplicationOperation,
}

impl<P, A, E> AffectedTestsService<P, A, E>
where
    P: AffectedTestsRetrievalPort,
    A: AuthorizationPort,
    E: SourceAuthorizationEvaluator,
{
    pub fn new(
        port: P,
        authorization: AuthorizationService<A, E>,
        operation: ApplicationOperation,
    ) -> Self {
        Self {
            port,
            authorization,
            operation,
        }
    }

    pub fn execute(
        &self,
        context: &RequestContext,
        request: AffectedTestsRequest,
        observed_at: UtcMicros,
    ) -> ApplicationResult<AffectedTestsResult> {
        let admission = match self
            .authorization
            .admit(context, &self.operation, observed_at)
        {
            Ok(admission) => admission,
            Err(problem) => return problem_envelope(context, &self.operation, problem),
        };
        let outcome = self.port.affected_tests(
            &RetrievalPortContext {
                request: context,
                operation: &self.operation,
            },
            &request,
        );
        evidence_envelope(
            context,
            &self.operation,
            &self.authorization,
            &admission,
            outcome,
            observed_at,
        )
    }
}

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
    let (mut termination, mut evidence) = match outcome {
        RetrievalPortOutcome::Completed(evidence) => (OperationTermination::Completed, evidence),
        RetrievalPortOutcome::Partial(evidence) => (OperationTermination::Partial, evidence),
        RetrievalPortOutcome::Cancelled(evidence) => (OperationTermination::Cancelled, evidence),
        RetrievalPortOutcome::TimedOut(evidence) => (OperationTermination::TimedOut, evidence),
        RetrievalPortOutcome::Failed(evidence) => (OperationTermination::Failed, evidence),
        RetrievalPortOutcome::Unavailable(evidence) => (OperationTermination::Failed, evidence),
    };
    let mut authority = admission.receipt().clone();
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
                match authorization.recheck_publication(
                    context,
                    operation,
                    admission,
                    evidence.finished_at,
                ) {
                    Ok(rechecked) => {
                        authority = rechecked;
                        None
                    }
                    Err(_) => Some((OperationTermination::Failed, OmissionReason::Redacted, None)),
                }
            }
        },
    };
    if let Some((override_termination, reason, cancellation)) = terminal_override {
        termination = override_termination;
        suppress_unpublished_evidence(&mut evidence, reason, cancellation);
    }
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
