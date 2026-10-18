//! Feedback observations emitted for application-surface argument rejections.

use tracedecay_contracts::RequestId;
use tracedecay_contracts::feedback::observations::{
    FeedbackArgumentRejectionClassV1, FeedbackOutcomeV1, FeedbackRejectedArgumentV1,
    FeedbackSourceEventV1,
};
use tracedecay_daemon_protocol::{
    ApplicationSurfaceAdapterError, application_delivery_route,
    application_surface_feedback_is_observable, application_surface_feedback_operation,
};
use tracedecay_domain::canonical_sha256;
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingSurface};

use super::problems::current_micros;

pub async fn observe_surface_argument_rejection(
    executor: Option<&dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
    surface: BindingSurface,
    operation: ApplicationSurfaceOperation,
    request_id: &RequestId,
    error: &ApplicationSurfaceAdapterError,
) {
    if !application_surface_feedback_is_observable(operation) {
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
                operation: application_surface_feedback_operation(operation),
                route: Some(application_delivery_route(surface)),
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
        ApplicationSurfaceAdapterError::InvalidSurfaceRequest { .. } => Some((
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
