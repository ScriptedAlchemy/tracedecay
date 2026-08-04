//! Feedback runtime lookup, feedback-cycle wiring, and the `execute_feedback*` daemon invocation handlers.

use super::*;

pub(crate) fn daemon_operation_event_authority() -> OperationEventAuthority {
    operation_event_authority()
}

pub(crate) struct DaemonFeedbackInvocationRequest {
    pub(crate) operation: DaemonInvocationOperation,
    pub(crate) request_handle: String,
    pub(crate) observed_at: UtcMicros,
    pub(crate) deadline: Deadline,
    pub(crate) cancellation: CancellationContext,
}

pub(crate) struct DaemonFeedbackInvocationResult {
    pub(crate) scope: ResolvedScope,
    pub(crate) evidence: EvidencePacket<serde_json::Value>,
}

pub(crate) type DaemonFeedbackInvocationFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<DaemonFeedbackInvocationResult, ApplicationProblem>> + Send + 'a,
    >,
>;

pub(crate) trait DaemonFeedbackInvocationPort: Send + Sync {
    fn invoke(
        &self,
        request: DaemonFeedbackInvocationRequest,
    ) -> DaemonFeedbackInvocationFuture<'_>;
}

pub(crate) struct DaemonAdvisoryCycleInvocationRequest {
    pub(crate) document_uri: String,
    pub(crate) observed_at: UtcMicros,
    pub(crate) deadline: Deadline,
    pub(crate) cancellation: CancellationContext,
}

pub(crate) type DaemonAdvisoryCycleInvocationFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<DaemonFeedbackInvocationResult, ApplicationProblem>> + Send + 'a,
    >,
>;

pub(crate) trait DaemonAdvisoryCycleInvocationPort: Send + Sync {
    fn invoke(
        &self,
        request: DaemonAdvisoryCycleInvocationRequest,
    ) -> DaemonAdvisoryCycleInvocationFuture<'_>;
}

#[derive(Clone)]
pub(crate) struct DaemonAdvisoryCycleInvocationOwner {
    pub(crate) project_id: ProjectId,
    pub(crate) service: Arc<dyn DaemonAdvisoryCycleInvocationPort>,
}

impl DaemonAdvisoryCycleInvocationOwner {
    pub(crate) fn new(
        project_id: ProjectId,
        service: Arc<dyn DaemonAdvisoryCycleInvocationPort>,
    ) -> Self {
        Self {
            project_id,
            service,
        }
    }
}

impl<R, P, A> DaemonFeedbackInvocationPort for DaemonFeedbackReadOwnerV1<R, P, A>
where
    R: FeedbackReadRequestAuthority + Send + Sync,
    P: FeedbackReadPort + Send + Sync,
    A: FeedbackRouteAuthorizationPort + Send + Sync,
{
    fn invoke(
        &self,
        request: DaemonFeedbackInvocationRequest,
    ) -> DaemonFeedbackInvocationFuture<'_> {
        Box::pin(async move {
            let result = match request.operation {
                DaemonInvocationOperation::FeedbackImpact => {
                    DaemonFeedbackReadOwnerV1::invoke_projection_with_controls(
                        self,
                        FeedbackCanonicalProjectionKindV1::Impact,
                        &request.request_handle,
                        request.observed_at,
                        request.deadline,
                        request.cancellation,
                    )
                    .await
                }
                DaemonInvocationOperation::AffectedTests => {
                    DaemonFeedbackReadOwnerV1::invoke_projection_with_controls(
                        self,
                        FeedbackCanonicalProjectionKindV1::AffectedTests,
                        &request.request_handle,
                        request.observed_at,
                        request.deadline,
                        request.cancellation,
                    )
                    .await
                }
                operation @ (DaemonInvocationOperation::FeedbackDiagnostics
                | DaemonInvocationOperation::FeedbackGet
                | DaemonInvocationOperation::FeedbackExpand
                | DaemonInvocationOperation::FeedbackList) => {
                    let operation = match operation {
                        DaemonInvocationOperation::FeedbackDiagnostics => {
                            FeedbackReadOperationV1::Diagnostics
                        }
                        DaemonInvocationOperation::FeedbackGet => FeedbackReadOperationV1::Get,
                        DaemonInvocationOperation::FeedbackExpand => {
                            FeedbackReadOperationV1::Expand
                        }
                        DaemonInvocationOperation::FeedbackList => FeedbackReadOperationV1::List,
                        _ => unreachable!("feedback operation was exhaustively matched"),
                    };
                    DaemonFeedbackReadOwnerV1::invoke_with_controls(
                        self,
                        operation,
                        &request.request_handle,
                        request.observed_at,
                        request.deadline,
                        request.cancellation,
                    )
                    .await
                }
                _ => {
                    return Err(ApplicationProblem::InvalidRequest {
                        diagnostic: SafeDiagnostic {
                            code: "feedback.invalid_operation".to_owned(),
                            message: "The feedback read operation is invalid".to_owned(),
                        },
                        retry: RetryDirective::Never,
                        legal_actions: Vec::new(),
                    });
                }
            }
            .map_err(feedback_owner_problem)?;
            match result {
                FeedbackReadInvocationResultV1::Diagnostics(result) => {
                    feedback_invocation_result(result)
                }
                FeedbackReadInvocationResultV1::Get(result) => feedback_invocation_result(result),
                FeedbackReadInvocationResultV1::Expand(result) => {
                    feedback_invocation_result(result)
                }
                FeedbackReadInvocationResultV1::List(result) => feedback_invocation_result(result),
                FeedbackReadInvocationResultV1::Impact(result) => {
                    feedback_invocation_result(result)
                }
                FeedbackReadInvocationResultV1::AffectedTests(result) => {
                    feedback_invocation_result(result)
                }
            }
        })
    }
}

#[derive(Clone)]
pub(crate) struct DaemonFeedbackInvocationOwner {
    pub(crate) project_id: ProjectId,
    pub(crate) service: Arc<dyn DaemonFeedbackInvocationPort>,
}

impl DaemonFeedbackInvocationOwner {
    pub(crate) fn new(
        project_id: ProjectId,
        service: Arc<dyn DaemonFeedbackInvocationPort>,
    ) -> Self {
        Self {
            project_id,
            service,
        }
    }
}

pub(super) fn feedback_invocation_result<T>(
    result: ApplicationResult<T>,
) -> Result<DaemonFeedbackInvocationResult, ApplicationProblem>
where
    T: Serialize,
{
    feedback_invocation_result_with(result, serde_json::to_value)
}

fn feedback_invocation_result_with<T>(
    result: ApplicationResult<T>,
    encode: impl FnOnce(T) -> Result<serde_json::Value, serde_json::Error>,
) -> Result<DaemonFeedbackInvocationResult, ApplicationProblem> {
    let application = result.map_err(|problem| problem.problem.into_source())?;
    let evidence = match application.outcome {
        ApplicationOutcome::Evidence(packet) => packet,
        ApplicationOutcome::Preview(_) | ApplicationOutcome::Effect(_) => {
            return Err(ApplicationProblem::unavailable(SafeDiagnostic {
                code: "feedback.invalid_owner_result".to_owned(),
                message: "The feedback read owner returned an invalid outcome".to_owned(),
            }));
        }
    };
    let payload = evidence.payload.map(encode).transpose().map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "feedback.result_encoding_failed".to_owned(),
            message: "The feedback read result could not be encoded".to_owned(),
        })
    })?;
    Ok(DaemonFeedbackInvocationResult {
        scope: application.scope,
        evidence: EvidencePacket {
            temporal: evidence.temporal,
            authority: evidence.authority,
            evidence_authorities: evidence.evidence_authorities,
            coverage: evidence.coverage,
            omissions: evidence.omissions,
            scores: evidence.scores,
            contributions: evidence.contributions,
            page: evidence.page,
            execution: evidence.execution,
            payload,
        },
    })
}

fn feedback_owner_problem(error: FeedbackReadOwnerErrorV1) -> ApplicationProblem {
    match error {
        FeedbackReadOwnerErrorV1::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        FeedbackReadOwnerErrorV1::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "feedback.owner_unavailable".to_owned(),
            message: "The feedback read owner is unavailable".to_owned(),
        }),
    }
}

pub(super) fn feedback_scope_matches(
    expected: Option<&ResolvedScope>,
    project_root: Option<&Path>,
    owner: Option<&DaemonFeedbackInvocationOwner>,
) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    let (Some(project_root), Some(owner)) = (project_root, owner) else {
        return false;
    };
    crate::daemon::project_open_owners::resolved_scope_for_project(project_root, &owner.project_id)
        .is_ok_and(|scope| &scope == expected)
}

pub(super) async fn execute_feedback(
    wire_request_id: String,
    owner: Option<DaemonFeedbackInvocationOwner>,
    operation: DaemonInvocationOperation,
    request_handle: String,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        // No feedback owner registered means the feedback read service is
        // absent, not that a specific handle is hidden. Report the fail-closed
        // infrastructure truth (Unavailable); concealment semantics only apply
        // once the service exists and a caller names an unknown handle.
        return application_problem(
            wire_request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "feedback.owner_unavailable".to_owned(),
                message: "The feedback read owner is unavailable".to_owned(),
            }),
        );
    };
    if RequestId::new(wire_request_id.clone()).is_err() {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
    }
    let result = owner
        .service
        .invoke(DaemonFeedbackInvocationRequest {
            operation,
            request_handle,
            observed_at,
            deadline,
            cancellation,
        })
        .await;
    match result {
        Ok(result) if result.scope.project_id == owner.project_id => {
            DaemonInvocationResponse::with_outcome(
                wire_request_id,
                DaemonInvocationOutcome::Feedback {
                    scope: result.scope,
                    result: DaemonFeedbackResult::from_application(result.evidence),
                },
            )
        }
        Ok(_) => concealed_application_problem(wire_request_id),
        Err(problem) => application_problem(wire_request_id, problem),
    }
}

pub(crate) fn advisory_cycle_invocation_result(
    context: &RequestContext,
    started_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
    outcome: AdvisoryCycleOutcome,
) -> Result<DaemonFeedbackInvocationResult, ApplicationProblem> {
    let ended_at = current_micros();
    let policy_digest = canonical_sha256(&(
        "tracedecay.daemon.feedback-advisory-policy",
        context.scope(),
        context.grant().digest.as_str(),
    ))
    .map_err(|_| advisory_cycle_contract_problem())?;
    let policy = PolicyDecisionRef::new(
        "policy.daemon.feedback-advisory",
        1,
        policy_digest,
        ComponentVersion::new("tracedecay.daemon.feedback-advisory")
            .map_err(|_| advisory_cycle_contract_problem())?,
    )
    .map_err(|_| advisory_cycle_contract_problem())?;
    let authority = AuthorityReceipt::from_context(context, policy, ended_at)
        .map_err(|_| advisory_cycle_contract_problem())?;

    let (termination, coverage, omissions, page, payload) = match outcome {
        AdvisoryCycleOutcome::Completed {
            cycle,
            contributions: _,
            observation_input: _,
        } => {
            let cycle_termination = cycle.execution.cycle.termination;
            let termination = match cycle_termination {
                FeedbackCycleTerminationV1::Clean | FeedbackCycleTerminationV1::DuplicateNoop => {
                    OperationTermination::Completed
                }
                FeedbackCycleTerminationV1::BudgetExceeded => OperationTermination::TimedOut,
                FeedbackCycleTerminationV1::Cancelled => OperationTermination::Cancelled,
                FeedbackCycleTerminationV1::DaemonUnavailable => OperationTermination::Unavailable,
                FeedbackCycleTerminationV1::Blocked
                | FeedbackCycleTerminationV1::IncompleteCoverage
                | FeedbackCycleTerminationV1::StaleReplanRequired
                | FeedbackCycleTerminationV1::UserStop => OperationTermination::Partial,
            };
            let total = cycle.execution.cycle.total_findings;
            let returned = cycle.execution.cycle.returned_findings;
            let completeness = if termination == OperationTermination::Completed {
                CoverageCompleteness::Complete
            } else {
                CoverageCompleteness::Partial
            };
            let coverage = EvidenceCoverage {
                requested_domains: vec![EvidenceDomain::Diagnostic],
                visited: Some(total),
                eligible: Some(total),
                returned,
                completeness,
                domains: vec![CoverageDomainState {
                    domain: EvidenceDomain::Diagnostic,
                    completeness,
                }],
            };
            coverage
                .validate()
                .map_err(|_| advisory_cycle_contract_problem())?;
            let omission_reason = match termination {
                OperationTermination::TimedOut => Some(OmissionReason::TimedOut),
                OperationTermination::Cancelled => Some(OmissionReason::Cancelled),
                OperationTermination::Unavailable => Some(OmissionReason::Unavailable),
                OperationTermination::Partial => Some(OmissionReason::Unsupported),
                OperationTermination::Completed
                | OperationTermination::Failed
                | OperationTermination::EffectUnknown => None,
            };
            let omissions = omission_reason
                .map(|reason| {
                    vec![Omission {
                        domain: EvidenceDomain::Diagnostic,
                        count: cycle
                            .execution
                            .cycle
                            .omitted_findings
                            .max(u64::from(returned == total)),
                        reason,
                    }]
                })
                .unwrap_or_default();
            let finding_handles = cycle
                .finding_handles
                .iter()
                .map(|finding| {
                    serde_json::json!({
                        "finding_id": finding.finding_id,
                        "retrieval_anchor_id": finding.retrieval_anchor_id,
                        "get_handle": finding.get_handle,
                        "expansion_handle": finding.expansion_handle,
                    })
                })
                .collect::<Vec<_>>();
            let payload = serde_json::json!({
                "cycle": cycle.execution.cycle,
                "finding_handles": finding_handles,
            });
            let page = PageState::first_page(
                SortContractId::new("sort.application.feedback.advisory-cycle.stable")
                    .map_err(|_| advisory_cycle_contract_problem())?,
                1,
                Some(1),
                1,
            )
            .map_err(|_| advisory_cycle_contract_problem())?;
            (termination, coverage, omissions, page, Some(payload))
        }
        AdvisoryCycleOutcome::Cancelled { contributions: _ } => {
            let coverage = incomplete_advisory_cycle_coverage();
            let page = empty_advisory_cycle_page()?;
            (
                OperationTermination::Cancelled,
                coverage,
                vec![Omission {
                    domain: EvidenceDomain::Diagnostic,
                    count: 1,
                    reason: OmissionReason::Cancelled,
                }],
                page,
                None,
            )
        }
        AdvisoryCycleOutcome::TimedOut { contributions: _ } => {
            let coverage = incomplete_advisory_cycle_coverage();
            let page = empty_advisory_cycle_page()?;
            (
                OperationTermination::TimedOut,
                coverage,
                vec![Omission {
                    domain: EvidenceDomain::Diagnostic,
                    count: 1,
                    reason: OmissionReason::TimedOut,
                }],
                page,
                None,
            )
        }
    };
    let cancellation = match termination {
        OperationTermination::Cancelled => Some(CancellationObservation {
            stage: CancellationStage::DuringRead,
            observed_at: match cancellation.state {
                CancellationState::Cancelled { requested_at } => requested_at,
                CancellationState::Active => ended_at,
            },
        }),
        OperationTermination::TimedOut => Some(CancellationObservation {
            stage: CancellationStage::DuringRead,
            observed_at: deadline.expires_at,
        }),
        OperationTermination::Completed
        | OperationTermination::Failed
        | OperationTermination::Unavailable
        | OperationTermination::Partial
        | OperationTermination::EffectUnknown => None,
    };
    let execution = OperationReceipt {
        started_at,
        ended_at,
        effective_deadline: deadline,
        cancellation,
        budget: OperationBudgetUsage::default(),
        termination,
    };
    execution
        .validate()
        .map_err(|_| advisory_cycle_contract_problem())?;
    Ok(DaemonFeedbackInvocationResult {
        scope: context.scope().clone(),
        evidence: EvidencePacket {
            temporal: TemporalState::current(ended_at),
            authority,
            evidence_authorities: Vec::new(),
            coverage,
            omissions,
            scores: Vec::new(),
            contributions: Vec::new(),
            page,
            execution,
            payload,
        },
    })
}

fn incomplete_advisory_cycle_coverage() -> EvidenceCoverage {
    EvidenceCoverage {
        requested_domains: vec![EvidenceDomain::Diagnostic],
        visited: None,
        eligible: None,
        returned: 0,
        completeness: CoverageCompleteness::Partial,
        domains: vec![CoverageDomainState {
            domain: EvidenceDomain::Diagnostic,
            completeness: CoverageCompleteness::Partial,
        }],
    }
}

fn empty_advisory_cycle_page() -> Result<PageState, ApplicationProblem> {
    PageState::first_page(
        SortContractId::new("sort.application.feedback.advisory-cycle.stable")
            .map_err(|_| advisory_cycle_contract_problem())?,
        1,
        Some(0),
        0,
    )
    .map_err(|_| advisory_cycle_contract_problem())
}

fn advisory_cycle_contract_problem() -> ApplicationProblem {
    ApplicationProblem::unavailable(SafeDiagnostic {
        code: "feedback.advisory-cycle.contract".to_owned(),
        message: "The advisory feedback cycle returned an invalid application result".to_owned(),
    })
}

pub(super) async fn execute_feedback_advisory_cycle(
    wire_request_id: String,
    owner: Option<DaemonAdvisoryCycleInvocationOwner>,
    document_uri: String,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        return application_problem(
            wire_request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "feedback.advisory-cycle.unavailable".to_owned(),
                message: "The advisory feedback cycle authority is unavailable".to_owned(),
            }),
        );
    };
    if cancellation.is_cancelled() {
        return application_problem(
            wire_request_id,
            ApplicationProblem::cancelled_before_admission(),
        );
    }
    if deadline.is_elapsed_at(observed_at) || deadline.is_elapsed_at(current_micros()) {
        return application_problem(
            wire_request_id,
            ApplicationProblem::timed_out_before_admission(),
        );
    }
    match owner
        .service
        .invoke(DaemonAdvisoryCycleInvocationRequest {
            document_uri,
            observed_at,
            deadline,
            cancellation,
        })
        .await
    {
        Ok(result) if result.scope.project_id == owner.project_id => {
            DaemonInvocationResponse::with_outcome(
                wire_request_id,
                DaemonInvocationOutcome::Feedback {
                    scope: result.scope,
                    result: DaemonFeedbackResult::from_application(result.evidence),
                },
            )
        }
        Ok(_) => concealed_application_problem(wire_request_id),
        Err(problem) => application_problem(wire_request_id, problem),
    }
}

impl DaemonInvocationService {
    pub(crate) async fn feedback_runtime(
        &self,
        project_root: Option<&Path>,
    ) -> Option<Arc<Pr12FeedbackRuntime>> {
        self.project_runtimes
            .read::<RegisteredFeedbackRuntime, _, _>(project_root?, |registered| {
                registered.runtime.clone()
            })
            .await
    }

    #[cfg(all(test, not(windows)))]
    pub(crate) async fn feedback_cycle(
        &self,
        project_root: Option<&Path>,
    ) -> Option<Arc<Pr12FeedbackCycleRuntime>> {
        self.project_runtimes.get(project_root?).await
    }

    pub(super) async fn feedback_cycle_input(
        &self,
        project_root: Option<&Path>,
    ) -> Option<Arc<dyn FeedbackCycleRuntimePort>> {
        self.project_runtimes
            .get::<Arc<SwitchableFeedbackCycleRuntimeV1>>(project_root?)
            .await
            .map(|input| -> Arc<dyn FeedbackCycleRuntimePort> { input })
    }
}
