//! Dispatch source-edit mutations through the project-owned authority.

use std::sync::Arc;

use tracedecay_contracts::{
    ApplicationExecutionFailureClassV1, ApplicationProblem, CancellationContext, Deadline,
    LegalAction, RequestId, RetryDirective, SafeDiagnostic, SourceEditInvocationV1,
    SourceEditReconciliationInvocationV1, SourceEditRollbackInvocationV1,
};
use tracedecay_daemon_protocol::DaemonInvocationProblem;
use tracedecay_domain::UtcMicros;
use tracedecay_domain::errors::TraceDecayError;

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
        SourceEditOwnerError::Cancelled => {
            application_problem(request_id, ApplicationProblem::cancelled_before_admission())
        }
        SourceEditOwnerError::TimedOut => {
            application_problem(request_id, ApplicationProblem::timed_out_before_admission())
        }
        SourceEditOwnerError::ExecutionFailed(error) => {
            map_source_edit_execution_error(request_id, error)
        }
    }
}

const SOURCE_EDIT_EXPECTED_STATE_MISMATCH: &str = "source_edit.expected_state_mismatch";
const SOURCE_EDIT_IDEMPOTENCY_CONFLICT: &str = "source_edit.idempotency_conflict";
const SOURCE_EDIT_SYMBOL_EVIDENCE_UNAVAILABLE: &str = "source-edit-symbol-evidence-unavailable";
const SOURCE_EDIT_DIAGNOSTICS_UNAVAILABLE: &str = "source_edit_diagnostics_unavailable";

fn sanitize_safe_diagnostic_text(value: &str, limit: usize) -> String {
    let collapsed: String = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut end = trimmed.len().min(limit);
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].trim_end().to_owned()
}

fn source_edit_kernel_cause(error: &TraceDecayError) -> (String, String) {
    match error.project_route_context() {
        Some((code, _, detail)) => (code.to_owned(), detail.to_owned()),
        None => match error {
            TraceDecayError::Io(io) => (
                "source_edit.execution_failed".to_owned(),
                format!("source edit I/O failed ({})", io.kind()),
            ),
            _ => ("source_edit.execution_failed".to_owned(), error.to_string()),
        },
    }
}

fn source_edit_safe_diagnostic(
    code: String,
    message: String,
) -> Result<SafeDiagnostic, tracedecay_contracts::ApplicationContractError> {
    let code = sanitize_safe_diagnostic_text(&code, 128);
    let code = if code.is_empty() {
        "source_edit.execution_failed".to_owned()
    } else {
        code
    };
    let message = sanitize_safe_diagnostic_text(&message, 512);
    let message = if message.is_empty() {
        "Source edit execution failed".to_owned()
    } else {
        message
    };
    SafeDiagnostic::new(code, message)
}

fn source_edit_execution_problem(
    error: TraceDecayError,
) -> Result<ApplicationProblem, tracedecay_contracts::ApplicationContractError> {
    let (code, message) = source_edit_kernel_cause(&error);
    let diagnostic = source_edit_safe_diagnostic(code, message)?;
    match diagnostic.code.as_str() {
        SOURCE_EDIT_EXPECTED_STATE_MISMATCH => Ok(ApplicationProblem::stale(diagnostic)),
        SOURCE_EDIT_IDEMPOTENCY_CONFLICT => Ok(ApplicationProblem::Conflict {
            diagnostic,
            retry: RetryDirective::AfterRevalidate,
            legal_actions: vec![LegalAction::Refresh],
        }),
        SOURCE_EDIT_SYMBOL_EVIDENCE_UNAVAILABLE | SOURCE_EDIT_DIAGNOSTICS_UNAVAILABLE => {
            Ok(ApplicationProblem::unavailable(diagnostic))
        }
        _ => ApplicationProblem::execution_failed(
            ApplicationExecutionFailureClassV1::Permanent,
            diagnostic,
        ),
    }
}

fn map_source_edit_execution_error(
    request_id: String,
    error: TraceDecayError,
) -> DaemonInvocationResponse {
    match source_edit_execution_problem(error) {
        Ok(problem) => application_problem(request_id, problem),
        Err(_) => DaemonInvocationResponse::problem(
            request_id,
            DaemonInvocationProblem::ApplicationContractViolation,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_contracts::ApplicationProblemKind;

    fn classified_problem(error: SourceEditOwnerError) -> ApplicationProblem {
        match map_source_edit_error("request.source-edit.classify".to_owned(), error).outcome {
            DaemonInvocationOutcome::ApplicationProblem { problem } => problem,
            outcome => {
                panic!("source-edit refusals must stay application problems, got {outcome:?}")
            }
        }
    }

    fn classified_kind(error: SourceEditOwnerError) -> ApplicationProblemKind {
        classified_problem(error).kind()
    }

    fn kernel_digest_mismatch() -> SourceEditOwnerError {
        SourceEditOwnerError::ExecutionFailed(TraceDecayError::project_route(
            "source_edit.expected_state_mismatch",
            true,
            "source edit candidate state changed while its exact preview was captured",
        ))
    }

    fn kernel_idempotency_conflict() -> SourceEditOwnerError {
        SourceEditOwnerError::ExecutionFailed(TraceDecayError::project_route(
            "source_edit.idempotency_conflict",
            false,
            "source edit idempotency key conflicts with a prior input",
        ))
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

        let reworded = SourceEditOwnerError::ExecutionFailed(TraceDecayError::Config {
            message: "warming failed to publish not found or is not authorized invocation contract is invalid"
                .to_owned(),
        });
        assert_eq!(
            classified_kind(reworded),
            ApplicationProblemKind::ExecutionFailed,
            "message text must not reclassify a typed ExecutionFailed refusal"
        );
    }

    #[test]
    fn source_edit_kernel_stale_keeps_reason_code_and_retryability() {
        let problem = classified_problem(kernel_digest_mismatch());
        assert_eq!(problem.kind(), ApplicationProblemKind::Stale);
        assert_eq!(problem.reason_code(), SOURCE_EDIT_EXPECTED_STATE_MISMATCH);
        assert_eq!(problem.retry(), RetryDirective::AfterRevalidate);
        let error = problem.into_trace_decay_error();
        let (reason_code, retryable, _) = error
            .project_route_context()
            .expect("digest mismatch must stay a typed project-route error");
        assert_eq!(reason_code, SOURCE_EDIT_EXPECTED_STATE_MISMATCH);
        assert!(retryable);
    }

    #[test]
    fn source_edit_kernel_conflict_keeps_reason_code_and_retryability() {
        let problem = classified_problem(kernel_idempotency_conflict());
        assert_eq!(problem.kind(), ApplicationProblemKind::Conflict);
        assert_eq!(problem.reason_code(), SOURCE_EDIT_IDEMPOTENCY_CONFLICT);
        assert_eq!(problem.retry(), RetryDirective::AfterRevalidate);
        let error = problem.into_trace_decay_error();
        let (reason_code, retryable, _) = error
            .project_route_context()
            .expect("idempotency conflict must stay a typed project-route error");
        assert_eq!(reason_code, SOURCE_EDIT_IDEMPOTENCY_CONFLICT);
        assert!(retryable);
    }

    #[test]
    fn source_edit_cancelled_request_is_cancelled_before_admission() {
        let problem = classified_problem(SourceEditOwnerError::Cancelled);
        assert_eq!(problem.kind(), ApplicationProblemKind::Cancelled);
        assert_eq!(problem.retry(), RetryDirective::Never);
        assert_eq!(problem.reason_code(), "cancelled");
    }

    #[test]
    fn source_edit_elapsed_deadline_is_timed_out_before_admission() {
        let problem = classified_problem(SourceEditOwnerError::TimedOut);
        assert_eq!(problem.kind(), ApplicationProblemKind::TimedOut);
        assert_eq!(problem.retry(), RetryDirective::Never);
        assert_eq!(problem.reason_code(), "timed_out");
    }

    #[test]
    fn source_edit_multiline_kernel_cause_survives_as_a_safe_diagnostic() {
        let problem = classified_problem(SourceEditOwnerError::ExecutionFailed(
            TraceDecayError::Config {
                message: "sqlx execute failed\nUNIQUE constraint\nwhile writing the journal"
                    .to_owned(),
            },
        ));
        assert_eq!(problem.kind(), ApplicationProblemKind::ExecutionFailed);
        assert_eq!(problem.reason_code(), "source_edit.execution_failed");
        let message = problem.safe_message();
        assert!(
            message.contains("sqlx execute failed") && message.contains("UNIQUE constraint"),
            "sanitized diagnostic must keep the kernel cause, got {message:?}"
        );
        assert!(
            !message.chars().any(char::is_control),
            "safe diagnostic must not retain control characters"
        );
    }

    #[test]
    fn source_edit_io_failure_omits_filesystem_paths() {
        let problem = classified_problem(SourceEditOwnerError::ExecutionFailed(
            TraceDecayError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "/secret/path/db.sqlite",
            )),
        ));
        let message = problem.safe_message();
        assert!(
            message.contains("source edit I/O failed") && message.contains("permission denied"),
            "I/O failures must name the kind, got {message:?}"
        );
        assert!(
            !message.contains("/secret") && !message.contains("db.sqlite"),
            "safe diagnostics must not carry filesystem paths, got {message:?}"
        );
    }
}
