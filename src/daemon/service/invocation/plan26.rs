//! Plan 26 invocation observation/emission helpers.

use super::*;

pub(super) fn plan26_invocation_subject(
    request_id: &str,
    operation: DaemonInvocationOperation,
    route: Option<Plan26DeliveryRouteV1>,
) -> Option<ManifestDigest> {
    canonical_sha256(&(
        "tracedecay.feedback.transport-observation.v1",
        request_id,
        operation.as_str(),
        route,
    ))
    .ok()
}

pub(super) fn plan26_observable_operation(operation: DaemonInvocationOperation) -> bool {
    matches!(
        operation,
        DaemonInvocationOperation::FeedbackDiagnostics
            | DaemonInvocationOperation::FeedbackGet
            | DaemonInvocationOperation::FeedbackExpand
            | DaemonInvocationOperation::FeedbackList
            | DaemonInvocationOperation::FeedbackAdvisoryCycle
            | DaemonInvocationOperation::FeedbackImpact
            | DaemonInvocationOperation::AffectedTests
            | DaemonInvocationOperation::PrimitiveImpact
            | DaemonInvocationOperation::PrimitiveAffectedTests
            | DaemonInvocationOperation::PrimitiveTestResults
            | DaemonInvocationOperation::PrimitiveRead
    )
}

pub(super) fn plan26_feedback_operation(
    operation: DaemonInvocationOperation,
) -> Plan26FeedbackOperationV1 {
    match operation {
        DaemonInvocationOperation::FeedbackDiagnostics => {
            Plan26FeedbackOperationV1::FeedbackDiagnostics
        }
        DaemonInvocationOperation::FeedbackGet => Plan26FeedbackOperationV1::FeedbackGet,
        DaemonInvocationOperation::FeedbackExpand => Plan26FeedbackOperationV1::FeedbackExpand,
        DaemonInvocationOperation::FeedbackList => Plan26FeedbackOperationV1::FeedbackList,
        DaemonInvocationOperation::FeedbackAdvisoryCycle => {
            Plan26FeedbackOperationV1::FeedbackCycle
        }
        DaemonInvocationOperation::FeedbackImpact => Plan26FeedbackOperationV1::PrimitiveImpact,
        DaemonInvocationOperation::AffectedTests => {
            Plan26FeedbackOperationV1::PrimitiveAffectedTests
        }
        DaemonInvocationOperation::PrimitiveImpact => Plan26FeedbackOperationV1::PrimitiveImpact,
        DaemonInvocationOperation::PrimitiveAffectedTests => {
            Plan26FeedbackOperationV1::PrimitiveAffectedTests
        }
        DaemonInvocationOperation::PrimitiveTestResults => {
            Plan26FeedbackOperationV1::PrimitiveTestResults
        }
        DaemonInvocationOperation::LspOpen
        | DaemonInvocationOperation::LspFrame
        | DaemonInvocationOperation::LspPoll
        | DaemonInvocationOperation::LspAcknowledge
        | DaemonInvocationOperation::LspReconnect
        | DaemonInvocationOperation::LspDetach => Plan26FeedbackOperationV1::LspSession,
        DaemonInvocationOperation::FeedbackObserve
        | DaemonInvocationOperation::PrimitiveRead
        | DaemonInvocationOperation::CodeExactOccurrence
        | DaemonInvocationOperation::CodePhraseSearch
        | DaemonInvocationOperation::CodeCallees
        | DaemonInvocationOperation::CodeFacets
        | DaemonInvocationOperation::CodeTimeline
        | DaemonInvocationOperation::CodeDeclaration
        | DaemonInvocationOperation::CodeDefinition
        | DaemonInvocationOperation::CodeTypeDefinition
        | DaemonInvocationOperation::CodeReferences
        | DaemonInvocationOperation::Configuration
        | DaemonInvocationOperation::ContextScout
        | DaemonInvocationOperation::MultiRootScopeSetRead
        | DaemonInvocationOperation::MultiRootScopeSetCompareAndSwap
        | DaemonInvocationOperation::MultiRootExecute
        | DaemonInvocationOperation::WorkApplication
        | DaemonInvocationOperation::WorkflowApplication
        | DaemonInvocationOperation::WorkAttempt
        | DaemonInvocationOperation::SemanticEvaluateAndPublish
        | DaemonInvocationOperation::GitStatus
        | DaemonInvocationOperation::GitDiff
        | DaemonInvocationOperation::GitHistory
        | DaemonInvocationOperation::GitBlame
        | DaemonInvocationOperation::GitHunks
        | DaemonInvocationOperation::GitPreview
        | DaemonInvocationOperation::GitApply => Plan26FeedbackOperationV1::FeedbackCycle,
    }
}

pub(super) fn emit_plan26_invocation_event(
    observations: Option<&Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>>,
    subject: Option<&ManifestDigest>,
    observed_at: UtcMicros,
    event: Plan26FeedbackSourceEventV1,
) {
    if let (Some(observations), Some(subject)) = (observations, subject) {
        observations.observe_source_event_for_subject(subject.clone(), observed_at, event);
    }
}

fn plan26_response_outcome(response: &DaemonInvocationResponse) -> Plan26FeedbackOutcomeV1 {
    match &response.outcome {
        DaemonInvocationOutcome::GitRead { .. }
        | DaemonInvocationOutcome::GitPreview { .. }
        | DaemonInvocationOutcome::GitApply { .. }
        | DaemonInvocationOutcome::Configuration { .. }
        | DaemonInvocationOutcome::ContextScout { .. }
        | DaemonInvocationOutcome::MultiRootScopeSetRead { .. }
        | DaemonInvocationOutcome::MultiRootScopeSetCompareAndSwap { .. }
        | DaemonInvocationOutcome::MultiRootQueryPage { .. }
        | DaemonInvocationOutcome::WorkApplication { .. }
        | DaemonInvocationOutcome::WorkflowApplication { .. }
        | DaemonInvocationOutcome::WorkAttempt { .. }
        | DaemonInvocationOutcome::SemanticEvaluatedProfilePublished { .. }
        | DaemonInvocationOutcome::ObservationAccepted
        | DaemonInvocationOutcome::LspOpened { .. }
        | DaemonInvocationOutcome::LspAcknowledged { .. }
        | DaemonInvocationOutcome::LspReconnected { .. }
        | DaemonInvocationOutcome::LspDetached => Plan26FeedbackOutcomeV1::Completed,
        DaemonInvocationOutcome::Feedback { result, .. }
        | DaemonInvocationOutcome::Primitive { result, .. }
        | DaemonInvocationOutcome::CallableCode { result, .. } => {
            match result.execution().termination {
                OperationTermination::Completed => Plan26FeedbackOutcomeV1::Completed,
                OperationTermination::Cancelled => Plan26FeedbackOutcomeV1::Cancelled,
                OperationTermination::TimedOut => Plan26FeedbackOutcomeV1::TimedOut,
                OperationTermination::Failed | OperationTermination::EffectUnknown => {
                    Plan26FeedbackOutcomeV1::Failed
                }
                OperationTermination::Unavailable => Plan26FeedbackOutcomeV1::Unavailable,
                OperationTermination::Partial => Plan26FeedbackOutcomeV1::Partial,
            }
        }
        DaemonInvocationOutcome::LspFrameAccepted { backpressured, .. } => {
            if *backpressured {
                Plan26FeedbackOutcomeV1::AtCapacity
            } else {
                Plan26FeedbackOutcomeV1::Accepted
            }
        }
        DaemonInvocationOutcome::LspFrame { closed, .. } => {
            if *closed {
                Plan26FeedbackOutcomeV1::Disconnected
            } else {
                Plan26FeedbackOutcomeV1::Completed
            }
        }
        DaemonInvocationOutcome::ApplicationProblem { problem } => match problem.kind() {
            ApplicationProblemKind::InvalidRequest => Plan26FeedbackOutcomeV1::Rejected,
            ApplicationProblemKind::NotFoundOrNotAuthorized => Plan26FeedbackOutcomeV1::Denied,
            ApplicationProblemKind::Conflict | ApplicationProblemKind::Stale => {
                Plan26FeedbackOutcomeV1::Stale
            }
            ApplicationProblemKind::Unsupported | ApplicationProblemKind::Unavailable => {
                Plan26FeedbackOutcomeV1::Unavailable
            }
            ApplicationProblemKind::Saturated => Plan26FeedbackOutcomeV1::AtCapacity,
            ApplicationProblemKind::Cancelled => Plan26FeedbackOutcomeV1::Cancelled,
            ApplicationProblemKind::TimedOut => Plan26FeedbackOutcomeV1::TimedOut,
        },
        DaemonInvocationOutcome::Problem { problem } => match problem {
            DaemonInvocationProblem::InvalidRequest
            | DaemonInvocationProblem::UnsupportedRevision => Plan26FeedbackOutcomeV1::Rejected,
            DaemonInvocationProblem::NotFoundOrNotAuthorized => Plan26FeedbackOutcomeV1::Denied,
            DaemonInvocationProblem::Unavailable => Plan26FeedbackOutcomeV1::Unavailable,
        },
    }
}

pub(super) fn plan26_rejected_argument(
    response: &DaemonInvocationResponse,
) -> Option<(Plan26RejectedArgumentV1, Plan26ArgumentRejectionClassV1)> {
    match &response.outcome {
        DaemonInvocationOutcome::Problem { problem } => {
            plan26_invocation_problem_rejected_argument(*problem)
        }
        DaemonInvocationOutcome::ApplicationProblem { problem }
            if problem.kind() == ApplicationProblemKind::InvalidRequest =>
        {
            Some((
                Plan26RejectedArgumentV1::RequestBody,
                Plan26ArgumentRejectionClassV1::InvalidShape,
            ))
        }
        _ => None,
    }
}

pub(super) const fn plan26_invocation_problem_rejected_argument(
    problem: DaemonInvocationProblem,
) -> Option<(Plan26RejectedArgumentV1, Plan26ArgumentRejectionClassV1)> {
    match problem {
        DaemonInvocationProblem::InvalidRequest => Some((
            Plan26RejectedArgumentV1::RequestBody,
            Plan26ArgumentRejectionClassV1::InvalidShape,
        )),
        DaemonInvocationProblem::UnsupportedRevision => Some((
            Plan26RejectedArgumentV1::Lifecycle,
            Plan26ArgumentRejectionClassV1::Unsupported,
        )),
        DaemonInvocationProblem::NotFoundOrNotAuthorized | DaemonInvocationProblem::Unavailable => {
            None
        }
    }
}

pub(super) fn observe_plan26_invocation_response(
    observations: Option<&Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>>,
    subject: Option<&ManifestDigest>,
    operation: DaemonInvocationOperation,
    route: Option<Plan26DeliveryRouteV1>,
    started_at: UtcMicros,
    response: &DaemonInvocationResponse,
) {
    let observed_at = current_micros();
    let outcome = plan26_response_outcome(response);
    let duration_micros = u64::try_from(observed_at.0.saturating_sub(started_at.0)).ok();
    if let Some(route) = route {
        emit_plan26_invocation_event(
            observations,
            subject,
            observed_at,
            Plan26FeedbackSourceEventV1::Delivery {
                operation: plan26_feedback_operation(operation),
                route,
                outcome,
                item_count: match &response.outcome {
                    DaemonInvocationOutcome::Feedback { result, .. }
                    | DaemonInvocationOutcome::Primitive { result, .. }
                    | DaemonInvocationOutcome::CallableCode { result, .. } => {
                        result.page().returned.try_into().unwrap_or(u32::MAX)
                    }
                    _ => 0,
                },
                duration_micros,
            },
        );
    }
    if matches!(
        outcome,
        Plan26FeedbackOutcomeV1::Cancelled | Plan26FeedbackOutcomeV1::TimedOut
    ) {
        emit_plan26_invocation_event(
            observations,
            subject,
            observed_at,
            Plan26FeedbackSourceEventV1::Cancellation {
                operation: plan26_feedback_operation(operation),
                outcome,
            },
        );
    }
    if let Some((argument, rejection)) = plan26_rejected_argument(response) {
        emit_plan26_invocation_event(
            observations,
            subject,
            observed_at,
            Plan26FeedbackSourceEventV1::SurfaceArgumentRejected {
                operation: plan26_feedback_operation(operation),
                route,
                argument,
                rejection,
                schema_revision: 1,
                outcome,
            },
        );
    }
    if let DaemonInvocationOutcome::Feedback { result, .. }
    | DaemonInvocationOutcome::Primitive { result, .. }
    | DaemonInvocationOutcome::CallableCode { result, .. } = &response.outcome
    {
        let omitted = result.page().total.map_or_else(
            || u64::from(result.page().cursor.is_some()),
            |total| total.saturating_sub(result.page().returned),
        );
        if omitted > 0 || result.page().cursor.is_some() {
            emit_plan26_invocation_event(
                observations,
                subject,
                observed_at,
                Plan26FeedbackSourceEventV1::Truncation {
                    operation: plan26_feedback_operation(operation),
                    returned_count: result.page().returned.try_into().unwrap_or(u32::MAX),
                    omitted_count: omitted.try_into().unwrap_or(u32::MAX),
                },
            );
        }
    }
    if operation == DaemonInvocationOperation::FeedbackExpand {
        emit_plan26_invocation_event(
            observations,
            subject,
            observed_at,
            Plan26FeedbackSourceEventV1::AnchorExpansion {
                operation: Plan26AnchorOperationV1::HandleExpansion,
                outcome,
                returned_count: match &response.outcome {
                    DaemonInvocationOutcome::Feedback { result, .. } => {
                        result.page().returned.try_into().unwrap_or(u32::MAX)
                    }
                    _ => 0,
                },
                duration_micros,
            },
        );
    }
}

pub(super) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

/// Retained name for this module's many call sites; the saturating clamp is the
/// shared one so it cannot drift from every other runtime that stamps "now".
pub(super) fn current_micros() -> UtcMicros {
    now_micros()
}
