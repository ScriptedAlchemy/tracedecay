//! Feedback observations emitted for application-surface argument rejections.

use tracedecay_contracts::RequestId;
use tracedecay_contracts::feedback::observations::{
    FeedbackArgumentRejectionClassV1, FeedbackDeliveryRouteV1, FeedbackOperationV1,
    FeedbackOutcomeV1, FeedbackRejectedArgumentV1, FeedbackSourceEventV1,
};
use tracedecay_daemon_protocol::ApplicationSurfaceAdapterError;
use tracedecay_domain::canonical_sha256;
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingSurface};

use super::problems::current_micros;

pub(super) fn feedback_delivery_route(surface: BindingSurface) -> FeedbackDeliveryRouteV1 {
    match surface {
        BindingSurface::Cli => FeedbackDeliveryRouteV1::Cli,
        BindingSurface::Mcp => FeedbackDeliveryRouteV1::Mcp,
        BindingSurface::Http | BindingSurface::Dashboard => FeedbackDeliveryRouteV1::Http,
        BindingSurface::Lsp => FeedbackDeliveryRouteV1::Lsp,
    }
}

pub(super) fn feedback_surface_operation(
    operation: ApplicationSurfaceOperation,
) -> FeedbackOperationV1 {
    match operation {
        ApplicationSurfaceOperation::FeedbackDiagnostics => {
            FeedbackOperationV1::FeedbackDiagnostics
        }
        ApplicationSurfaceOperation::FeedbackGet => FeedbackOperationV1::FeedbackGet,
        ApplicationSurfaceOperation::FeedbackExpand => FeedbackOperationV1::FeedbackExpand,
        ApplicationSurfaceOperation::FeedbackList => FeedbackOperationV1::FeedbackList,
        ApplicationSurfaceOperation::FeedbackAdvisoryCycle => FeedbackOperationV1::FeedbackCycle,
        ApplicationSurfaceOperation::FeedbackImpact => FeedbackOperationV1::PrimitiveImpact,
        ApplicationSurfaceOperation::AffectedTests => FeedbackOperationV1::PrimitiveAffectedTests,
        ApplicationSurfaceOperation::TestResults => FeedbackOperationV1::PrimitiveTestResults,
        ApplicationSurfaceOperation::GitStatus
        | ApplicationSurfaceOperation::GitDiff
        | ApplicationSurfaceOperation::GitHistory
        | ApplicationSurfaceOperation::GitBlame
        | ApplicationSurfaceOperation::GitHunks
        | ApplicationSurfaceOperation::GitPreview
        | ApplicationSurfaceOperation::GitApply
        | ApplicationSurfaceOperation::GitHubStackSignalExpand
        | ApplicationSurfaceOperation::NativeIntegrationStackSnapshot
        | ApplicationSurfaceOperation::NativeIntegrationPreflight
        | ApplicationSurfaceOperation::NativeIntegrationApprove
        | ApplicationSurfaceOperation::NativeIntegrationApply
        | ApplicationSurfaceOperation::NativeIntegrationStatus
        | ApplicationSurfaceOperation::NativeIntegrationCancel
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeInventory
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeInspect
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile
        | ApplicationSurfaceOperation::CodeExactOccurrence
        | ApplicationSurfaceOperation::CodePhraseSearch
        | ApplicationSurfaceOperation::CodeSymbolSearch
        | ApplicationSurfaceOperation::CodeSignatureSearch
        | ApplicationSurfaceOperation::CodeImplementations
        | ApplicationSurfaceOperation::CodeTypeHierarchy
        | ApplicationSurfaceOperation::CodeCallers
        | ApplicationSurfaceOperation::CodeCallees
        | ApplicationSurfaceOperation::CodeFacets
        | ApplicationSurfaceOperation::CodeTimeline
        | ApplicationSurfaceOperation::CodeDeclaration
        | ApplicationSurfaceOperation::CodeDefinition
        | ApplicationSurfaceOperation::CodeTypeDefinition
        | ApplicationSurfaceOperation::CodeReferences
        | ApplicationSurfaceOperation::SessionLookup
        | ApplicationSurfaceOperation::QualifiedName
        | ApplicationSurfaceOperation::CallChain
        | ApplicationSurfaceOperation::FileDependents
        | ApplicationSurfaceOperation::SourceLines
        | ApplicationSurfaceOperation::SourceBody
        | ApplicationSurfaceOperation::SourceOutline
        | ApplicationSurfaceOperation::ModuleApi
        | ApplicationSurfaceOperation::HealthRead
        | ApplicationSurfaceOperation::HealthDelta
        | ApplicationSurfaceOperation::StorageStatus
        | ApplicationSurfaceOperation::DiagnosticsRead
        | ApplicationSurfaceOperation::ObservatoryRead
        | ApplicationSurfaceOperation::ConfigurationList
        | ApplicationSurfaceOperation::ConfigurationGet
        | ApplicationSurfaceOperation::ConfigurationSet
        | ApplicationSurfaceOperation::ConfigurationUnset
        | ApplicationSurfaceOperation::ConfigurationBatch
        | ApplicationSurfaceOperation::ConfigurationObservedState
        | ApplicationSurfaceOperation::ConfigurationProtectedPreview
        | ApplicationSurfaceOperation::ConfigurationProtectedApply
        | ApplicationSurfaceOperation::ConfigurationRollbackPreview
        | ApplicationSurfaceOperation::ConfigurationRollbackApply
        | ApplicationSurfaceOperation::ConfigurationAudit
        | ApplicationSurfaceOperation::ContextScoutStatus
        | ApplicationSurfaceOperation::ContextScoutRecent
        | ApplicationSurfaceOperation::ContextScoutExplain
        | ApplicationSurfaceOperation::ContextScoutCapability
        | ApplicationSurfaceOperation::ContextScoutBudget
        | ApplicationSurfaceOperation::ContextScoutPause
        | ApplicationSurfaceOperation::ContextScoutResume
        | ApplicationSurfaceOperation::ContextScoutCancel
        | ApplicationSurfaceOperation::ContextScoutClaim
        | ApplicationSurfaceOperation::ContextScoutDelivery
        | ApplicationSurfaceOperation::ContextScoutFeedback => FeedbackOperationV1::FeedbackCycle,
    }
}

pub(super) fn feedback_surface_is_observable(operation: ApplicationSurfaceOperation) -> bool {
    matches!(
        operation,
        ApplicationSurfaceOperation::FeedbackDiagnostics
            | ApplicationSurfaceOperation::FeedbackGet
            | ApplicationSurfaceOperation::FeedbackExpand
            | ApplicationSurfaceOperation::FeedbackList
            | ApplicationSurfaceOperation::FeedbackAdvisoryCycle
            | ApplicationSurfaceOperation::FeedbackImpact
            | ApplicationSurfaceOperation::AffectedTests
            | ApplicationSurfaceOperation::TestResults
            | ApplicationSurfaceOperation::SessionLookup
            | ApplicationSurfaceOperation::QualifiedName
            | ApplicationSurfaceOperation::CallChain
            | ApplicationSurfaceOperation::FileDependents
            | ApplicationSurfaceOperation::SourceLines
            | ApplicationSurfaceOperation::SourceBody
            | ApplicationSurfaceOperation::SourceOutline
            | ApplicationSurfaceOperation::ModuleApi
            | ApplicationSurfaceOperation::HealthRead
            | ApplicationSurfaceOperation::HealthDelta
            | ApplicationSurfaceOperation::StorageStatus
            | ApplicationSurfaceOperation::DiagnosticsRead
    )
}

pub async fn observe_surface_argument_rejection(
    executor: Option<&dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
    surface: BindingSurface,
    operation: ApplicationSurfaceOperation,
    request_id: &RequestId,
    error: &ApplicationSurfaceAdapterError,
) {
    if !feedback_surface_is_observable(operation) {
        return;
    }
    let Some((argument, rejection, outcome)) = surface_rejection_metadata(error) else {
        return;
    };
    let (Some(executor), Ok(subject_digest), Ok(observed_at)) = (
        executor,
        canonical_sha256(&(
            "tracedecay.feedback.surface-rejection.v1",
            request_id.as_str(),
            surface,
            operation,
        )),
        current_micros(),
    ) else {
        return;
    };
    let _ = executor
        .observe_feedback(
            subject_digest,
            observed_at,
            FeedbackSourceEventV1::SurfaceArgumentRejected {
                operation: feedback_surface_operation(operation),
                route: Some(feedback_delivery_route(surface)),
                argument,
                rejection,
                schema_revision: 1,
                outcome,
            },
        )
        .await;
}

pub(super) fn surface_rejection_metadata(
    error: &ApplicationSurfaceAdapterError,
) -> Option<(
    FeedbackRejectedArgumentV1,
    FeedbackArgumentRejectionClassV1,
    FeedbackOutcomeV1,
)> {
    match error {
        ApplicationSurfaceAdapterError::InvalidRequestHandle => Some((
            FeedbackRejectedArgumentV1::RequestHandle,
            FeedbackArgumentRejectionClassV1::InvalidShape,
            FeedbackOutcomeV1::Rejected,
        )),
        ApplicationSurfaceAdapterError::InvalidSurfaceRequest => Some((
            FeedbackRejectedArgumentV1::RequestBody,
            FeedbackArgumentRejectionClassV1::InvalidShape,
            FeedbackOutcomeV1::Rejected,
        )),
        ApplicationSurfaceAdapterError::UnknownOrNotAuthorized => Some((
            FeedbackRejectedArgumentV1::Operation,
            FeedbackArgumentRejectionClassV1::Unauthorized,
            FeedbackOutcomeV1::Denied,
        )),
        ApplicationSurfaceAdapterError::Catalog(_)
        | ApplicationSurfaceAdapterError::Contract(_)
        | ApplicationSurfaceAdapterError::Identifier(_)
        | ApplicationSurfaceAdapterError::CatalogValidation(_)
        | ApplicationSurfaceAdapterError::DaemonUnavailable
        | ApplicationSurfaceAdapterError::DaemonUnreachable { .. } => None,
    }
}
