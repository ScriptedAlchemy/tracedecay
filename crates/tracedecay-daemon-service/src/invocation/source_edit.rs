//! Dispatch source-edit mutations through the project-owned authority.

use std::sync::Arc;

use tracedecay_contracts::{
    ApplicationProblem, CancellationContext, Deadline, RequestId, RetryDirective,
    SourceEditInvocationV1, SourceEditReconciliationInvocationV1, SourceEditRollbackInvocationV1,
};
use tracedecay_domain::UtcMicros;
use tracedecay_domain::errors::TraceDecayError;

use crate::project_owner_registration::ProjectSourceEditOwnerV1;

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
        .execute_invocation(typed_request_id, request, deadline, cancellation)
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
        .reconcile_invocation(typed_request_id, request, deadline, cancellation)
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
        .rollback_invocation(typed_request_id, request, deadline, cancellation)
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

fn map_source_edit_error(request_id: String, error: TraceDecayError) -> DaemonInvocationResponse {
    let message = error.to_string();
    if message.contains("warming") {
        return runtime_mounting_problem(request_id);
    }
    if message.contains("failed to publish") {
        return runtime_publication_failed_problem(request_id);
    }
    if message.contains("not found or is not authorized") {
        return concealed_application_problem(request_id);
    }
    if message.contains("invocation contract is invalid") {
        return application_problem(
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
        );
    }
    concealed_application_problem(request_id)
}
