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

pub(super) async fn execute_feedback_advisory_cycle(
    wire_request_id: String,
) -> DaemonInvocationResponse {
    application_problem(
        wire_request_id,
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "feedback.advisory_cycle_quarantined".to_owned(),
            message: "The advisory feedback cycle is quarantined".to_owned(),
        }),
    )
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
