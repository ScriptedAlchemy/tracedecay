//! Invocation observation and feedback-emission helpers.

use super::*;

pub(super) fn invocation_observation_subject(
    request_id: &str,
    operation: DaemonInvocationOperation,
    route: Option<FeedbackDeliveryRouteV1>,
) -> Option<ManifestDigest> {
    canonical_sha256(&(
        "tracedecay.feedback.transport-observation.v1",
        request_id,
        operation.as_str(),
        route,
    ))
    .ok()
}

pub(super) fn is_observable_operation(operation: DaemonInvocationOperation) -> bool {
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

pub(super) fn feedback_observation_operation(
    operation: DaemonInvocationOperation,
) -> FeedbackOperationV1 {
    match operation {
        DaemonInvocationOperation::FeedbackDiagnostics => FeedbackOperationV1::FeedbackDiagnostics,
        DaemonInvocationOperation::FeedbackGet => FeedbackOperationV1::FeedbackGet,
        DaemonInvocationOperation::FeedbackExpand => FeedbackOperationV1::FeedbackExpand,
        DaemonInvocationOperation::FeedbackList => FeedbackOperationV1::FeedbackList,
        DaemonInvocationOperation::FeedbackAdvisoryCycle => FeedbackOperationV1::FeedbackCycle,
        DaemonInvocationOperation::FeedbackImpact => FeedbackOperationV1::PrimitiveImpact,
        DaemonInvocationOperation::AffectedTests => FeedbackOperationV1::PrimitiveAffectedTests,
        DaemonInvocationOperation::PrimitiveImpact => FeedbackOperationV1::PrimitiveImpact,
        DaemonInvocationOperation::PrimitiveAffectedTests => {
            FeedbackOperationV1::PrimitiveAffectedTests
        }
        DaemonInvocationOperation::PrimitiveTestResults => {
            FeedbackOperationV1::PrimitiveTestResults
        }
        DaemonInvocationOperation::LspOpen
        | DaemonInvocationOperation::LspFrame
        | DaemonInvocationOperation::LspPoll
        | DaemonInvocationOperation::LspAcknowledge
        | DaemonInvocationOperation::LspReconnect
        | DaemonInvocationOperation::LspDetach => FeedbackOperationV1::LspSession,
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
        | DaemonInvocationOperation::HandoffApplication
        | DaemonInvocationOperation::SemanticEvaluateAndPublish
        | DaemonInvocationOperation::GitStatus
        | DaemonInvocationOperation::GitDiff
        | DaemonInvocationOperation::GitHistory
        | DaemonInvocationOperation::GitBlame
        | DaemonInvocationOperation::GitHunks
        | DaemonInvocationOperation::GitPreview
        | DaemonInvocationOperation::GitApply
        | DaemonInvocationOperation::NativeIntegrationStackSnapshot
        | DaemonInvocationOperation::NativeIntegrationPreflight
        | DaemonInvocationOperation::NativeIntegrationApprove
        | DaemonInvocationOperation::NativeIntegrationApply
        | DaemonInvocationOperation::NativeIntegrationStatus
        | DaemonInvocationOperation::NativeIntegrationCancel => FeedbackOperationV1::FeedbackCycle,
    }
}

pub(super) fn emit_invocation_observation(
    observations: Option<&Arc<dyn FeedbackObservationEmitterV1 + Send + Sync>>,
    subject: Option<&ManifestDigest>,
    observed_at: UtcMicros,
    event: FeedbackSourceEventV1,
) {
    if let (Some(observations), Some(subject)) = (observations, subject) {
        observations.observe_source_event_for_subject(subject.clone(), observed_at, event);
    }
}

fn invocation_response_outcome(response: &DaemonInvocationResponse) -> FeedbackOutcomeV1 {
    match &response.outcome {
        DaemonInvocationOutcome::GitRead { .. }
        | DaemonInvocationOutcome::GitPreview { .. }
        | DaemonInvocationOutcome::GitApply { .. }
        | DaemonInvocationOutcome::NativeIntegration { .. }
        | DaemonInvocationOutcome::Configuration { .. }
        | DaemonInvocationOutcome::ContextScout { .. }
        | DaemonInvocationOutcome::MultiRootScopeSetRead { .. }
        | DaemonInvocationOutcome::MultiRootScopeSetCompareAndSwap { .. }
        | DaemonInvocationOutcome::MultiRootQueryPage { .. }
        | DaemonInvocationOutcome::WorkApplication { .. }
        | DaemonInvocationOutcome::WorkflowApplication { .. }
        | DaemonInvocationOutcome::HandoffApplication { .. }
        | DaemonInvocationOutcome::SemanticEvaluatedProfilePublished { .. }
        | DaemonInvocationOutcome::ObservationAccepted
        | DaemonInvocationOutcome::LspOpened { .. }
        | DaemonInvocationOutcome::LspAcknowledged { .. }
        | DaemonInvocationOutcome::LspReconnected { .. }
        | DaemonInvocationOutcome::LspDetached => FeedbackOutcomeV1::Completed,
        DaemonInvocationOutcome::Feedback { result, .. }
        | DaemonInvocationOutcome::Primitive { result, .. }
        | DaemonInvocationOutcome::CallableCode { result, .. } => {
            match result.execution().termination {
                OperationTermination::Completed => FeedbackOutcomeV1::Completed,
                OperationTermination::Cancelled => FeedbackOutcomeV1::Cancelled,
                OperationTermination::TimedOut => FeedbackOutcomeV1::TimedOut,
                OperationTermination::Failed | OperationTermination::EffectUnknown => {
                    FeedbackOutcomeV1::Failed
                }
                OperationTermination::Unavailable => FeedbackOutcomeV1::Unavailable,
                OperationTermination::Partial => FeedbackOutcomeV1::Partial,
            }
        }
        DaemonInvocationOutcome::LspFrameAccepted { backpressured, .. } => {
            if *backpressured {
                FeedbackOutcomeV1::AtCapacity
            } else {
                FeedbackOutcomeV1::Accepted
            }
        }
        DaemonInvocationOutcome::LspFrame { closed, .. } => {
            if *closed {
                FeedbackOutcomeV1::Disconnected
            } else {
                FeedbackOutcomeV1::Completed
            }
        }
        DaemonInvocationOutcome::ApplicationProblem { problem } => match problem.kind() {
            ApplicationProblemKind::InvalidRequest => FeedbackOutcomeV1::Rejected,
            ApplicationProblemKind::NotFoundOrNotAuthorized => FeedbackOutcomeV1::Denied,
            ApplicationProblemKind::Conflict | ApplicationProblemKind::Stale => {
                FeedbackOutcomeV1::Stale
            }
            ApplicationProblemKind::Unsupported | ApplicationProblemKind::Unavailable => {
                FeedbackOutcomeV1::Unavailable
            }
            ApplicationProblemKind::Saturated => FeedbackOutcomeV1::AtCapacity,
            ApplicationProblemKind::Cancelled => FeedbackOutcomeV1::Cancelled,
            ApplicationProblemKind::TimedOut => FeedbackOutcomeV1::TimedOut,
        },
        DaemonInvocationOutcome::Problem { problem } => match problem {
            DaemonInvocationProblem::InvalidRequest
            | DaemonInvocationProblem::UnsupportedRevision => FeedbackOutcomeV1::Rejected,
            DaemonInvocationProblem::NotFoundOrNotAuthorized => FeedbackOutcomeV1::Denied,
            DaemonInvocationProblem::ResetRequired | DaemonInvocationProblem::Unavailable => {
                FeedbackOutcomeV1::Unavailable
            }
        },
    }
}

pub(super) fn invocation_rejected_argument(
    response: &DaemonInvocationResponse,
) -> Option<(FeedbackRejectedArgumentV1, FeedbackArgumentRejectionClassV1)> {
    match &response.outcome {
        DaemonInvocationOutcome::Problem { problem } => {
            invocation_problem_rejected_argument(*problem)
        }
        DaemonInvocationOutcome::ApplicationProblem { problem }
            if problem.kind() == ApplicationProblemKind::InvalidRequest =>
        {
            Some((
                FeedbackRejectedArgumentV1::RequestBody,
                FeedbackArgumentRejectionClassV1::InvalidShape,
            ))
        }
        _ => None,
    }
}

pub(super) const fn invocation_problem_rejected_argument(
    problem: DaemonInvocationProblem,
) -> Option<(FeedbackRejectedArgumentV1, FeedbackArgumentRejectionClassV1)> {
    match problem {
        DaemonInvocationProblem::InvalidRequest => Some((
            FeedbackRejectedArgumentV1::RequestBody,
            FeedbackArgumentRejectionClassV1::InvalidShape,
        )),
        DaemonInvocationProblem::UnsupportedRevision => Some((
            FeedbackRejectedArgumentV1::Lifecycle,
            FeedbackArgumentRejectionClassV1::Unsupported,
        )),
        DaemonInvocationProblem::NotFoundOrNotAuthorized
        | DaemonInvocationProblem::ResetRequired
        | DaemonInvocationProblem::Unavailable => None,
    }
}

pub(super) fn observe_invocation_response(
    observations: Option<&Arc<dyn FeedbackObservationEmitterV1 + Send + Sync>>,
    subject: Option<&ManifestDigest>,
    operation: DaemonInvocationOperation,
    route: Option<FeedbackDeliveryRouteV1>,
    started_at: UtcMicros,
    response: &DaemonInvocationResponse,
) {
    let observed_at = current_micros();
    let outcome = invocation_response_outcome(response);
    let duration_micros = u64::try_from(observed_at.0.saturating_sub(started_at.0)).ok();
    if let Some(route) = route {
        emit_invocation_observation(
            observations,
            subject,
            observed_at,
            FeedbackSourceEventV1::Delivery {
                operation: feedback_observation_operation(operation),
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
        FeedbackOutcomeV1::Cancelled | FeedbackOutcomeV1::TimedOut
    ) {
        emit_invocation_observation(
            observations,
            subject,
            observed_at,
            FeedbackSourceEventV1::Cancellation {
                operation: feedback_observation_operation(operation),
                outcome,
            },
        );
    }
    if let Some((argument, rejection)) = invocation_rejected_argument(response) {
        emit_invocation_observation(
            observations,
            subject,
            observed_at,
            FeedbackSourceEventV1::SurfaceArgumentRejected {
                operation: feedback_observation_operation(operation),
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
            emit_invocation_observation(
                observations,
                subject,
                observed_at,
                FeedbackSourceEventV1::Truncation {
                    operation: feedback_observation_operation(operation),
                    returned_count: result.page().returned.try_into().unwrap_or(u32::MAX),
                    omitted_count: omitted.try_into().unwrap_or(u32::MAX),
                },
            );
        }
    }
    if operation == DaemonInvocationOperation::FeedbackExpand {
        emit_invocation_observation(
            observations,
            subject,
            observed_at,
            FeedbackSourceEventV1::AnchorExpansion {
                operation: FeedbackAnchorOperationV1::HandleExpansion,
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
