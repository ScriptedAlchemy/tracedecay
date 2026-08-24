//! Closed CLI binding for daemon-owned Workflow application operations.
//!
//! The adapter decodes one strict request DTO, resolves the project-scoped
//! daemon route, and returns the daemon's canonical application outcome. It
//! owns no workflow state, scheduling, retry, provider, or persistence logic.

use std::path::PathBuf;

use serde_json::Value;
use tracedecay_api::WorkflowOperation;
use tracedecay_application::{
    ApplicationEnvelope, ApplicationOutcome, ApplicationProblem, ApplicationProblemEnvelope,
    ApplicationResult, CancellationSignal, Deadline, LegalAction, ResultContractRef,
    RetryDirective, SafeDiagnostic, TaskHandoffIssueRequest, TaskHandoffRedeemRequest,
    WorkflowDefinitionActivateRequest, WorkflowDefinitionDiffRequest, WorkflowDefinitionGetRequest,
    WorkflowDefinitionHistoryRequest, WorkflowDefinitionListRequest,
    WorkflowDefinitionRegisterRequest, WorkflowDefinitionRejectRequest,
    WorkflowDefinitionRetireRequest, WorkflowDefinitionValidateRequest,
    workflow_executable_binding_registry,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::OperationId;

use crate::daemon::DaemonHandshake;
use crate::daemon_client::{
    DaemonInvocationClient, InvocationCancellationPolicy, invocation_now_micros,
};
use crate::daemon_contract::{
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationRequest,
    WorkflowApplicationInvocation, WorkflowApplicationOutcome,
};
use crate::errors::{Result, TraceDecayError};
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};

fn workflow_cli_deadline(operation: WorkflowOperation, observed_at: UtcMicros) -> Result<Deadline> {
    let operation_id =
        OperationId::new(operation.operation_id_str().to_owned()).map_err(config_error)?;
    let registry = workflow_executable_binding_registry().map_err(config_error)?;
    let binding = registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "Workflow operation {} is not advertised by this build",
                operation_id.as_str()
            ),
        })?;
    let maximum_micros = i64::try_from(
        std::time::Duration::from_millis(binding.deadline().maximum_millis()).as_micros(),
    )
    .map_err(|_| TraceDecayError::Config {
        message: "The canonical Workflow deadline exceeds the domain clock".to_owned(),
    })?;
    Deadline::new(UtcMicros(observed_at.0.saturating_add(maximum_micros))).map_err(config_error)
}

fn workflow_result_contract(operation: WorkflowOperation) -> Result<ResultContractRef> {
    let operation_id =
        OperationId::new(operation.operation_id_str().to_owned()).map_err(config_error)?;
    let registry = workflow_executable_binding_registry().map_err(config_error)?;
    let Some(binding) = registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
    else {
        return Err(TraceDecayError::Config {
            message: format!(
                "Workflow operation {} is not advertised by this build",
                operation_id.as_str()
            ),
        });
    };
    Ok(ResultContractRef::from_schema(
        binding.result_schema().schema_ref(),
    ))
}

fn decode_workflow_invocation(
    operation: WorkflowOperation,
    body: Value,
) -> Result<WorkflowApplicationInvocation> {
    match operation {
        WorkflowOperation::RegisterDefinition => decode::<WorkflowDefinitionRegisterRequest>(body)
            .map(WorkflowApplicationInvocation::RegisterDefinition),
        WorkflowOperation::ActivateDefinition => decode::<WorkflowDefinitionActivateRequest>(body)
            .map(WorkflowApplicationInvocation::ActivateDefinition),
        WorkflowOperation::RetireDefinition => decode::<WorkflowDefinitionRetireRequest>(body)
            .map(WorkflowApplicationInvocation::RetireDefinition),
        WorkflowOperation::RejectDefinition => decode::<WorkflowDefinitionRejectRequest>(body)
            .map(WorkflowApplicationInvocation::RejectDefinition),
        WorkflowOperation::ValidateDefinition => decode::<WorkflowDefinitionValidateRequest>(body)
            .map(WorkflowApplicationInvocation::ValidateDefinition),
        WorkflowOperation::GetDefinition => decode::<WorkflowDefinitionGetRequest>(body)
            .map(WorkflowApplicationInvocation::GetDefinition),
        WorkflowOperation::ListDefinitions => decode::<WorkflowDefinitionListRequest>(body)
            .map(WorkflowApplicationInvocation::ListDefinitions),
        WorkflowOperation::DefinitionHistory => decode::<WorkflowDefinitionHistoryRequest>(body)
            .map(WorkflowApplicationInvocation::DefinitionHistory),
        WorkflowOperation::DiffDefinition => decode::<WorkflowDefinitionDiffRequest>(body)
            .map(WorkflowApplicationInvocation::DiffDefinition),
        WorkflowOperation::HandoffIssue => {
            decode::<TaskHandoffIssueRequest>(body).map(WorkflowApplicationInvocation::HandoffIssue)
        }
        WorkflowOperation::HandoffRedeem => decode::<TaskHandoffRedeemRequest>(body)
            .map(WorkflowApplicationInvocation::HandoffRedeem),
        WorkflowOperation::StartRun => {
            decode::<tracedecay_application::WorkflowRunStartRequest>(body)
                .map(|request| WorkflowApplicationInvocation::StartRun(Box::new(request)))
        }
        WorkflowOperation::PauseRun => {
            decode::<tracedecay_application::WorkflowRunPauseRequest>(body)
                .map(WorkflowApplicationInvocation::PauseRun)
        }
        WorkflowOperation::ResumeRun => {
            decode::<tracedecay_application::WorkflowRunResumeRequest>(body)
                .map(WorkflowApplicationInvocation::ResumeRun)
        }
        WorkflowOperation::CancelRun => {
            decode::<tracedecay_application::WorkflowRunCancelRequest>(body)
                .map(WorkflowApplicationInvocation::CancelRun)
        }
        WorkflowOperation::GetRun => decode::<tracedecay_application::WorkflowRunGetRequest>(body)
            .map(WorkflowApplicationInvocation::GetRun),
    }
}

fn workflow_outcome_matches(
    operation: WorkflowOperation,
    outcome: &WorkflowApplicationOutcome,
) -> bool {
    matches!(
        (operation, outcome),
        (
            WorkflowOperation::RegisterDefinition,
            WorkflowApplicationOutcome::RegisterDefinition(_)
        ) | (
            WorkflowOperation::ActivateDefinition,
            WorkflowApplicationOutcome::ActivateDefinition(_)
        ) | (
            WorkflowOperation::RetireDefinition,
            WorkflowApplicationOutcome::RetireDefinition(_)
        ) | (
            WorkflowOperation::RejectDefinition,
            WorkflowApplicationOutcome::RejectDefinition(_)
        ) | (
            WorkflowOperation::ValidateDefinition,
            WorkflowApplicationOutcome::ValidateDefinition(_)
        ) | (
            WorkflowOperation::GetDefinition,
            WorkflowApplicationOutcome::GetDefinition(_)
        ) | (
            WorkflowOperation::ListDefinitions,
            WorkflowApplicationOutcome::ListDefinitions(_)
        ) | (
            WorkflowOperation::DefinitionHistory,
            WorkflowApplicationOutcome::DefinitionHistory(_)
        ) | (
            WorkflowOperation::DiffDefinition,
            WorkflowApplicationOutcome::DiffDefinition(_)
        ) | (
            WorkflowOperation::HandoffIssue,
            WorkflowApplicationOutcome::HandoffIssue(_)
        ) | (
            WorkflowOperation::HandoffRedeem,
            WorkflowApplicationOutcome::HandoffRedeem(_)
        ) | (
            WorkflowOperation::StartRun,
            WorkflowApplicationOutcome::StartRun(_)
        ) | (
            WorkflowOperation::PauseRun,
            WorkflowApplicationOutcome::PauseRun(_)
        ) | (
            WorkflowOperation::ResumeRun,
            WorkflowApplicationOutcome::ResumeRun(_)
        ) | (
            WorkflowOperation::CancelRun,
            WorkflowApplicationOutcome::CancelRun(_)
        ) | (
            WorkflowOperation::GetRun,
            WorkflowApplicationOutcome::GetRun(_)
        )
    )
}

pub async fn invoke_workflow_cli(
    project_root: PathBuf,
    operation: WorkflowOperation,
    body: Value,
) -> Result<ApplicationResult<Value>> {
    let result_contract = workflow_result_contract(operation)?;
    let request_id =
        mint_global_request_id(GlobalRequestSurface::Cli).map_err(|_| TraceDecayError::Config {
            message: "could not allocate a Workflow CLI request id".to_owned(),
        })?;
    let observed_at = invocation_now_micros();
    let deadline = workflow_cli_deadline(operation, observed_at)?;
    let cancellation =
        CancellationSignal::active(format!("cancellation.cli.{}", request_id.as_str()))
            .map_err(config_error)?;
    let invocation = match decode_workflow_invocation(operation, body) {
        Ok(invocation) => invocation,
        Err(_) => {
            return Ok(Err(workflow_problem(
                result_contract,
                request_id,
                invalid_workflow_request(),
            )?));
        }
    };
    let request = DaemonInvocationRequest::workflow_application(
        request_id.as_str(),
        invocation,
        observed_at,
        deadline.clone(),
        cancellation.context(),
    );
    let handshake = DaemonHandshake::for_current_client(Some(project_root), None, false, false)?;
    let response = match DaemonInvocationClient::for_current(handshake)?
        .invoke_controlled(
            request,
            deadline,
            cancellation,
            InvocationCancellationPolicy::AuthoritativeEffect,
        )
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return Ok(Err(workflow_problem(
                result_contract,
                request_id,
                error.into_application_problem(),
            )?));
        }
    };
    match response.outcome {
        DaemonInvocationOutcome::WorkflowApplication { scope, outcome }
            if workflow_outcome_matches(operation, &outcome) =>
        {
            Ok(Ok(ApplicationEnvelope {
                contract: result_contract,
                request_id,
                scope,
                outcome: erase_workflow_outcome(outcome)?,
            }))
        }
        DaemonInvocationOutcome::ApplicationProblem { problem } => {
            Ok(Err(workflow_problem(result_contract, request_id, problem)?))
        }
        DaemonInvocationOutcome::Problem { problem } => Ok(Err(workflow_problem(
            result_contract,
            request_id,
            daemon_application_problem(problem),
        )?)),
        _ => Ok(Err(workflow_problem(
            result_contract,
            request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "workflow_response_unavailable".to_owned(),
                message: "The daemon returned no canonical Workflow result".to_owned(),
            }),
        )?)),
    }
}

fn erase_workflow_outcome(
    outcome: WorkflowApplicationOutcome,
) -> Result<ApplicationOutcome<Value>> {
    let outcome = match outcome {
        WorkflowApplicationOutcome::RegisterDefinition(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcome::ActivateDefinition(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcome::RetireDefinition(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcome::RejectDefinition(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcome::ValidateDefinition(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcome::GetDefinition(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcome::ListDefinitions(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcome::DefinitionHistory(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcome::DiffDefinition(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcome::HandoffIssue(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcome::HandoffRedeem(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcome::StartRun(outcome)
        | WorkflowApplicationOutcome::PauseRun(outcome)
        | WorkflowApplicationOutcome::ResumeRun(outcome)
        | WorkflowApplicationOutcome::CancelRun(outcome)
        | WorkflowApplicationOutcome::GetRun(outcome) => serde_json::to_value(outcome),
    }?;
    serde_json::from_value(outcome).map_err(Into::into)
}

fn workflow_problem(
    result_contract: ResultContractRef,
    request_id: tracedecay_application::RequestId,
    problem: ApplicationProblem,
) -> Result<ApplicationProblemEnvelope> {
    ApplicationProblemEnvelope::new(result_contract, request_id, problem).map_err(config_error)
}

fn invalid_workflow_request() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "invalid_workflow_request".to_owned(),
            message: "The Workflow request does not match its operation contract".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    }
}

fn daemon_application_problem(problem: DaemonInvocationProblem) -> ApplicationProblem {
    match problem {
        DaemonInvocationProblem::InvalidRequest => invalid_workflow_request(),
        DaemonInvocationProblem::UnsupportedRevision => ApplicationProblem::Unsupported {
            diagnostic: SafeDiagnostic {
                code: "unsupported_workflow_revision".to_owned(),
                message: "The daemon does not support this Workflow revision".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::CorrectRequest],
        },
        DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        DaemonInvocationProblem::ResetRequired => {
            ApplicationProblem::reset_required(SafeDiagnostic {
                code: "workflow_authority_reset_required".to_owned(),
                message: "The owning Workflow authority requires an explicit reset".to_owned(),
            })
        }
        DaemonInvocationProblem::ApplicationContractViolation => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "workflow_application_contract_violation".to_owned(),
                message: "The Workflow result violated its canonical contract".to_owned(),
            })
        }
        DaemonInvocationProblem::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "workflow_authority_unavailable".to_owned(),
            message: "The owning Workflow authority is unavailable".to_owned(),
        }),
    }
}

fn decode<T>(body: Value) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(body).map_err(|error| TraceDecayError::Config {
        message: format!("invalid typed Workflow request: {error}"),
    })
}

#[cfg(test)]
mod reset_problem_tests {
    use super::*;

    #[test]
    fn daemon_workflow_reset_remains_a_typed_cli_problem() {
        let problem = daemon_application_problem(DaemonInvocationProblem::ResetRequired);
        let ApplicationProblem::ResetRequired {
            diagnostic,
            retry,
            legal_actions,
        } = problem
        else {
            panic!("workflow reset must remain a typed reset-required problem");
        };
        assert_eq!(diagnostic.code, "workflow_authority_reset_required");
        assert_eq!(
            diagnostic.message,
            "The owning Workflow authority requires an explicit reset"
        );
        assert_eq!(retry, RetryDirective::Never);
        assert_eq!(legal_actions, vec![LegalAction::Reset]);
    }
}

fn config_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_api::WorkflowOperation;
    use tracedecay_domain::UtcMicros;
    use tracedecay_tool_catalog::OperationId;

    use super::{
        decode_workflow_invocation, workflow_cli_deadline, workflow_executable_binding_registry,
    };

    #[test]
    fn closed_binding_rejects_unknown_request_fields_before_daemon_dispatch() {
        let error = decode_workflow_invocation(
            WorkflowOperation::HandoffRedeem,
            json!({"unexpected": true}),
        )
        .expect_err("strict DTO must reject unknown fields");
        assert!(error.to_string().contains("invalid typed Workflow request"));
    }

    #[test]
    fn cli_deadline_uses_the_executable_registry_ceiling() {
        let observed_at = UtcMicros(1_000_000);
        let operation = WorkflowOperation::RegisterDefinition;
        let deadline = workflow_cli_deadline(operation, observed_at)
            .expect("registry-derived Workflow CLI deadline");
        let operation_id = OperationId::new(operation.operation_id_str().to_owned()).unwrap();
        let maximum_millis = workflow_executable_binding_registry()
            .unwrap()
            .get(&operation_id)
            .and_then(|availability| availability.binding())
            .unwrap()
            .deadline()
            .maximum_millis();
        let maximum_micros =
            i64::try_from(std::time::Duration::from_millis(maximum_millis).as_micros()).unwrap();
        assert_eq!(
            deadline.expires_at,
            UtcMicros(observed_at.0.saturating_add(maximum_micros))
        );
        assert_eq!(maximum_millis, 30_000);
        assert_ne!(maximum_micros, 120_000_000);
    }
}
