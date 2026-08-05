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
    WorkflowDefinitionActivateRequest, WorkflowDefinitionRegisterRequest, WorkflowFanOutRequest,
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

const WORKFLOW_CLI_DEADLINE_MICROS: i64 = 120_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowCliOperation {
    RegisterDefinition,
    ActivateDefinition,
    ExecuteFanOut,
    HandoffIssue,
    HandoffRedeem,
}

impl WorkflowCliOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RegisterDefinition => "register_definition",
            Self::ActivateDefinition => "activate_definition",
            Self::ExecuteFanOut => "execute_fan_out",
            Self::HandoffIssue => "handoff_issue",
            Self::HandoffRedeem => "handoff_redeem",
        }
    }

    const fn canonical(self) -> WorkflowOperation {
        match self {
            Self::RegisterDefinition => WorkflowOperation::RegisterDefinition,
            Self::ActivateDefinition => WorkflowOperation::ActivateDefinition,
            Self::ExecuteFanOut => WorkflowOperation::ExecuteFanOut,
            Self::HandoffIssue => WorkflowOperation::HandoffIssue,
            Self::HandoffRedeem => WorkflowOperation::HandoffRedeem,
        }
    }

    fn result_contract(self) -> Result<ResultContractRef> {
        let operation_id = OperationId::new(self.canonical().operation_id_str().to_owned())
            .map_err(config_error)?;
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

    fn decode(self, body: Value) -> Result<WorkflowApplicationInvocation> {
        match self {
            Self::RegisterDefinition => decode::<WorkflowDefinitionRegisterRequest>(body)
                .map(WorkflowApplicationInvocation::RegisterDefinition),
            Self::ActivateDefinition => decode::<WorkflowDefinitionActivateRequest>(body)
                .map(WorkflowApplicationInvocation::ActivateDefinition),
            Self::ExecuteFanOut => decode::<WorkflowFanOutRequest>(body)
                .map(Box::new)
                .map(WorkflowApplicationInvocation::ExecuteFanOut),
            Self::HandoffIssue => decode::<TaskHandoffIssueRequest>(body)
                .map(WorkflowApplicationInvocation::HandoffIssue),
            Self::HandoffRedeem => decode::<TaskHandoffRedeemRequest>(body)
                .map(WorkflowApplicationInvocation::HandoffRedeem),
        }
    }

    fn matches(self, outcome: &WorkflowApplicationOutcome) -> bool {
        matches!(
            (self, outcome),
            (
                Self::RegisterDefinition,
                WorkflowApplicationOutcome::RegisterDefinition(_)
            ) | (
                Self::ActivateDefinition,
                WorkflowApplicationOutcome::ActivateDefinition(_)
            ) | (
                Self::ExecuteFanOut,
                WorkflowApplicationOutcome::ExecuteFanOut(_)
            ) | (
                Self::HandoffIssue,
                WorkflowApplicationOutcome::HandoffIssue(_)
            ) | (
                Self::HandoffRedeem,
                WorkflowApplicationOutcome::HandoffRedeem(_)
            )
        )
    }
}

pub async fn invoke_workflow_cli(
    project_root: PathBuf,
    operation: WorkflowCliOperation,
    body: Value,
) -> Result<ApplicationResult<Value>> {
    let result_contract = operation.result_contract()?;
    let request_id =
        mint_global_request_id(GlobalRequestSurface::Cli).map_err(|_| TraceDecayError::Config {
            message: "could not allocate a Workflow CLI request id".to_owned(),
        })?;
    let observed_at = invocation_now_micros();
    let deadline = Deadline::new(UtcMicros(
        observed_at.0.saturating_add(WORKFLOW_CLI_DEADLINE_MICROS),
    ))
    .map_err(config_error)?;
    let cancellation =
        CancellationSignal::active(format!("cancellation.cli.{}", request_id.as_str()))
            .map_err(config_error)?;
    let invocation = match operation.decode(body) {
        Ok(invocation) => invocation,
        Err(_) => {
            return Ok(Err(workflow_problem(
                result_contract,
                request_id,
                invalid_workflow_request(),
            )));
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
            )));
        }
    };
    match response.outcome {
        DaemonInvocationOutcome::WorkflowApplication { scope, outcome }
            if operation.matches(&outcome) =>
        {
            Ok(Ok(ApplicationEnvelope {
                contract: result_contract,
                request_id,
                scope,
                outcome: erase_workflow_outcome(outcome)?,
            }))
        }
        DaemonInvocationOutcome::ApplicationProblem { problem } => {
            Ok(Err(workflow_problem(result_contract, request_id, problem)))
        }
        DaemonInvocationOutcome::Problem { problem } => Ok(Err(workflow_problem(
            result_contract,
            request_id,
            daemon_application_problem(problem),
        ))),
        _ => Ok(Err(workflow_problem(
            result_contract,
            request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "workflow_response_unavailable".to_owned(),
                message: "The daemon returned no canonical Workflow result".to_owned(),
            }),
        ))),
    }
}

fn erase_workflow_outcome(
    outcome: WorkflowApplicationOutcome,
) -> Result<ApplicationOutcome<Value>> {
    let outcome = match outcome {
        WorkflowApplicationOutcome::RegisterDefinition(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcome::ActivateDefinition(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcome::ExecuteFanOut(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcome::HandoffIssue(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcome::HandoffRedeem(outcome) => serde_json::to_value(outcome),
    }?;
    serde_json::from_value(outcome).map_err(Into::into)
}

fn workflow_problem(
    result_contract: ResultContractRef,
    request_id: tracedecay_application::RequestId,
    problem: ApplicationProblem,
) -> ApplicationProblemEnvelope {
    ApplicationProblemEnvelope::new(result_contract, request_id, problem)
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

fn config_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::WorkflowCliOperation;

    #[test]
    fn closed_binding_rejects_unknown_request_fields_before_daemon_dispatch() {
        let error = WorkflowCliOperation::HandoffRedeem
            .decode(json!({"unexpected": true}))
            .expect_err("strict DTO must reject unknown fields");
        assert!(error.to_string().contains("invalid typed Workflow request"));
    }
}
