//! Dispatch source-edit mutations through the project-owned authority.

use std::sync::Arc;

use tracedecay_contracts::{
    ApplicationProblem, CancellationContext, Deadline, RequestId, RetryDirective,
    SourceEditInvocationV1, SourceEditReconciliationInvocationV1, SourceEditRollbackInvocationV1,
};
use tracedecay_domain::UtcMicros;

use crate::project_owner_registration::{ProjectSourceEditOwnerV1, SourceEditOwnerError};

use super::{
    DaemonInvocationOutcome, DaemonInvocationResponse, application_problem,
    concealed_application_problem, runtime_mounting_problem, runtime_publication_failed_problem,
};

pub(super) async fn execute_source_edit(
    request_id: String,
    owner: Option<Arc<ProjectSourceEditOwnerV1>>,
    request: SourceEditInvocationV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        return application_problem(
            request_id,
            ApplicationProblem::unavailable(tracedecay_contracts::SafeDiagnostic {
                code: "source_edit.authority_unavailable".to_owned(),
                message: "The daemon-owned source edit authority is unavailable".to_owned(),
            }),
        );
    };
    let typed_request_id = match RequestId::new(request_id.clone()) {
        Ok(request_id) => request_id,
        Err(_) => {
            return application_problem(
                request_id,
                ApplicationProblem::InvalidRequest {
                    diagnostic: tracedecay_contracts::SafeDiagnostic {
                        code: "source_edit.invalid_request_id".to_owned(),
                        message: "The source-edit request id is invalid".to_owned(),
                    },
                    retry: RetryDirective::Never,
                    legal_actions: vec![tracedecay_contracts::LegalAction::CorrectRequest],
                },
            );
        }
    };
    let cancellation =
        match super::native_integration::live_cancellation_signal(&cancellation, observed_at) {
            Ok(cancellation) => cancellation,
            Err(problem) => return application_problem(request_id, problem),
        };
    match owner
        .execute(typed_request_id, request, deadline, cancellation)
        .await
    {
        Ok(result) => DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::SourceEdit {
                scope: owner.scope(),
                result,
            },
        ),
        Err(error) => map_source_edit_error(request_id, error),
    }
}

pub(super) async fn execute_source_edit_reconcile(
    request_id: String,
    owner: Option<Arc<ProjectSourceEditOwnerV1>>,
    request: SourceEditReconciliationInvocationV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        return application_problem(
            request_id,
            ApplicationProblem::unavailable(tracedecay_contracts::SafeDiagnostic {
                code: "source_edit.authority_unavailable".to_owned(),
                message: "The daemon-owned source edit reconciliation authority is unavailable"
                    .to_owned(),
            }),
        );
    };
    let typed_request_id = match RequestId::new(request_id.clone()) {
        Ok(request_id) => request_id,
        Err(_) => {
            return application_problem(
                request_id,
                ApplicationProblem::InvalidRequest {
                    diagnostic: tracedecay_contracts::SafeDiagnostic {
                        code: "source_edit.invalid_request_id".to_owned(),
                        message: "The source-edit request id is invalid".to_owned(),
                    },
                    retry: RetryDirective::Never,
                    legal_actions: vec![tracedecay_contracts::LegalAction::CorrectRequest],
                },
            );
        }
    };
    let cancellation =
        match super::native_integration::live_cancellation_signal(&cancellation, observed_at) {
            Ok(cancellation) => cancellation,
            Err(problem) => return application_problem(request_id, problem),
        };
    match owner
        .reconcile(typed_request_id, request, deadline, cancellation)
        .await
    {
        Ok(result) => DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::SourceEdit {
                scope: owner.scope(),
                result,
            },
        ),
        Err(error) => map_source_edit_error(request_id, error),
    }
}

pub(super) async fn execute_source_edit_rollback(
    request_id: String,
    owner: Option<Arc<ProjectSourceEditOwnerV1>>,
    request: SourceEditRollbackInvocationV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        return application_problem(
            request_id,
            ApplicationProblem::unavailable(tracedecay_contracts::SafeDiagnostic {
                code: "source_edit.authority_unavailable".to_owned(),
                message: "The daemon-owned source edit rollback authority is unavailable"
                    .to_owned(),
            }),
        );
    };
    let typed_request_id = match RequestId::new(request_id.clone()) {
        Ok(request_id) => request_id,
        Err(_) => {
            return application_problem(
                request_id,
                ApplicationProblem::InvalidRequest {
                    diagnostic: tracedecay_contracts::SafeDiagnostic {
                        code: "source_edit.invalid_request_id".to_owned(),
                        message: "The source-edit request id is invalid".to_owned(),
                    },
                    retry: RetryDirective::Never,
                    legal_actions: vec![tracedecay_contracts::LegalAction::CorrectRequest],
                },
            );
        }
    };
    let cancellation =
        match super::native_integration::live_cancellation_signal(&cancellation, observed_at) {
            Ok(cancellation) => cancellation,
            Err(problem) => return application_problem(request_id, problem),
        };
    match owner
        .rollback(typed_request_id, request, deadline, cancellation)
        .await
    {
        Ok(result) => DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::SourceEdit {
                scope: owner.scope(),
                result,
            },
        ),
        Err(error) => map_source_edit_error(request_id, error),
    }
}

fn map_source_edit_error(
    request_id: String,
    error: SourceEditOwnerError,
) -> DaemonInvocationResponse {
    match error {
        SourceEditOwnerError::Warming => runtime_mounting_problem(request_id),
        SourceEditOwnerError::PublicationFailed => runtime_publication_failed_problem(request_id),
        SourceEditOwnerError::NotAuthorized => concealed_application_problem(request_id),
        SourceEditOwnerError::InvalidContract => application_problem(
            request_id,
            ApplicationProblem::InvalidRequest {
                diagnostic: tracedecay_contracts::SafeDiagnostic {
                    code: "source_edit.invalid_request".to_owned(),
                    message: "The source-edit request does not match its invocation contract"
                        .to_owned(),
                },
                retry: RetryDirective::Never,
                legal_actions: vec![tracedecay_contracts::LegalAction::CorrectRequest],
            },
        ),
        SourceEditOwnerError::Other(_) => concealed_application_problem(request_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_contracts::ApplicationProblemKind;
    use tracedecay_domain::errors::TraceDecayError;

    fn classified_kind(error: SourceEditOwnerError) -> ApplicationProblemKind {
        match map_source_edit_error("request.source-edit.classify".to_owned(), error).outcome {
            DaemonInvocationOutcome::ApplicationProblem { problem } => problem.kind(),
            outcome => {
                panic!("source-edit refusals must stay application problems, got {outcome:?}")
            }
        }
    }

    #[test]
    fn source_edit_refusal_classification_follows_typed_state_not_message_text() {
        assert_eq!(
            classified_kind(SourceEditOwnerError::Warming),
            ApplicationProblemKind::Unavailable
        );
        assert_eq!(
            classified_kind(SourceEditOwnerError::PublicationFailed),
            ApplicationProblemKind::ExecutionFailed
        );
        assert_eq!(
            classified_kind(SourceEditOwnerError::NotAuthorized),
            ApplicationProblemKind::NotFoundOrNotAuthorized
        );
        assert_eq!(
            classified_kind(SourceEditOwnerError::InvalidContract),
            ApplicationProblemKind::InvalidRequest
        );

        let reworded = SourceEditOwnerError::Other(TraceDecayError::Config {
            message: "warming failed to publish not found or is not authorized invocation contract is invalid"
                .to_owned(),
        });
        assert_eq!(
            classified_kind(reworded),
            ApplicationProblemKind::NotFoundOrNotAuthorized,
            "message text must not reclassify a typed Other refusal"
        );
    }
}
