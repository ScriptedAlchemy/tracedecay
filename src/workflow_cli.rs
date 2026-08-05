//! Closed CLI binding for daemon-owned Workflow application operations.
//!
//! The adapter decodes one strict request DTO, resolves the project-scoped
//! daemon route, and returns the daemon's canonical application outcome. It
//! owns no workflow state, scheduling, retry, provider, or persistence logic.

use std::path::PathBuf;

use serde_json::{Value, json};
use tracedecay_api::WorkflowOperation;
use tracedecay_application::{
    CancellationSignal, Deadline, TaskHandoffIssueRequest, TaskHandoffRedeemRequest,
    WorkflowDefinitionActivateRequest, WorkflowDefinitionDiffRequest, WorkflowDefinitionGetRequest,
    WorkflowDefinitionHistoryRequest, WorkflowDefinitionListRequest,
    WorkflowDefinitionRegisterRequest, WorkflowDefinitionRetireRequest,
    WorkflowDefinitionValidateRequest, WorkflowFanOutRequest, workflow_executable_binding_registry,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::OperationId;

use crate::daemon::DaemonHandshake;
use crate::daemon_client::{DaemonInvocationClient, invocation_now_micros};
use crate::daemon_contract::{
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationRequest,
    WorkflowApplicationInvocation, WorkflowApplicationOutcome,
};
use crate::errors::{Result, TraceDecayError};
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};

const WORKFLOW_CLI_DEADLINE_MICROS: i64 = 120_000_000;

fn verify_catalog_binding(operation: WorkflowOperation) -> Result<()> {
    let operation_id =
        OperationId::new(operation.operation_id_str().to_owned()).map_err(config_error)?;
    let registry = workflow_executable_binding_registry().map_err(config_error)?;
    if registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
        .is_none()
    {
        return Err(TraceDecayError::Config {
            message: format!(
                "Workflow operation {} is not advertised by this build",
                operation_id.as_str()
            ),
        });
    }
    Ok(())
}

fn decode_operation(
    operation: WorkflowOperation,
    body: Value,
) -> Result<WorkflowApplicationInvocation> {
    match operation {
        WorkflowOperation::RegisterDefinition => decode::<WorkflowDefinitionRegisterRequest>(body)
            .map(WorkflowApplicationInvocation::RegisterDefinition),
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
        WorkflowOperation::ActivateDefinition => decode::<WorkflowDefinitionActivateRequest>(body)
            .map(WorkflowApplicationInvocation::ActivateDefinition),
        WorkflowOperation::RetireDefinition => decode::<WorkflowDefinitionRetireRequest>(body)
            .map(WorkflowApplicationInvocation::RetireDefinition),
        WorkflowOperation::ExecuteFanOut => decode::<WorkflowFanOutRequest>(body)
            .map(Box::new)
            .map(WorkflowApplicationInvocation::ExecuteFanOut),
        WorkflowOperation::HandoffIssue => {
            decode::<TaskHandoffIssueRequest>(body).map(WorkflowApplicationInvocation::HandoffIssue)
        }
        WorkflowOperation::HandoffRedeem => decode::<TaskHandoffRedeemRequest>(body)
            .map(WorkflowApplicationInvocation::HandoffRedeem),
    }
}

fn outcome_matches(operation: WorkflowOperation, outcome: &WorkflowApplicationOutcome) -> bool {
    matches!(
        (operation, outcome),
        (
            WorkflowOperation::RegisterDefinition,
            WorkflowApplicationOutcome::RegisterDefinition(_)
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
            WorkflowOperation::ActivateDefinition,
            WorkflowApplicationOutcome::ActivateDefinition(_)
        ) | (
            WorkflowOperation::RetireDefinition,
            WorkflowApplicationOutcome::RetireDefinition(_)
        ) | (
            WorkflowOperation::ExecuteFanOut,
            WorkflowApplicationOutcome::ExecuteFanOut(_)
        ) | (
            WorkflowOperation::HandoffIssue,
            WorkflowApplicationOutcome::HandoffIssue(_)
        ) | (
            WorkflowOperation::HandoffRedeem,
            WorkflowApplicationOutcome::HandoffRedeem(_)
        )
    )
}

pub async fn invoke_workflow_cli(
    project_root: PathBuf,
    operation: WorkflowOperation,
    body: Value,
) -> Result<Value> {
    verify_catalog_binding(operation)?;
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
    let request = DaemonInvocationRequest::workflow_application(
        request_id.as_str(),
        decode_operation(operation, body)?,
        observed_at,
        deadline,
        cancellation.context(),
    );
    let handshake = DaemonHandshake::for_current_client(Some(project_root), None, false, false)?;
    let response = DaemonInvocationClient::for_current(handshake)?
        .invoke(request)
        .await?;
    match response.outcome {
        DaemonInvocationOutcome::WorkflowApplication { scope, outcome }
            if outcome_matches(operation, &outcome) =>
        {
            Ok(json!({
                "operation": operation.operation_key(),
                "scope": scope,
                "outcome": outcome,
            }))
        }
        DaemonInvocationOutcome::ApplicationProblem { problem } => Err(TraceDecayError::Config {
            message: format!("{}: {}", problem.canonical_code(), problem.safe_message()),
        }),
        DaemonInvocationOutcome::Problem { problem } => Err(TraceDecayError::Config {
            message: daemon_problem(problem).to_owned(),
        }),
        _ => Err(TraceDecayError::Config {
            message: "daemon returned an unexpected Workflow CLI response".to_owned(),
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

const fn daemon_problem(problem: DaemonInvocationProblem) -> &'static str {
    match problem {
        DaemonInvocationProblem::InvalidRequest => "daemon rejected the Workflow request",
        DaemonInvocationProblem::UnsupportedRevision => {
            "daemon does not support this Workflow invocation revision"
        }
        DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            "Workflow operation was not found or is not authorized"
        }
        DaemonInvocationProblem::ResetRequired => "Workflow authority requires an explicit reset",
        DaemonInvocationProblem::Unavailable => "Workflow authority is unavailable",
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

    use super::decode_operation;

    #[test]
    fn closed_binding_rejects_unknown_request_fields_before_daemon_dispatch() {
        let error = decode_operation(
            WorkflowOperation::HandoffRedeem,
            json!({"unexpected": true}),
        )
        .expect_err("strict DTO must reject unknown fields");
        assert!(error.to_string().contains("invalid typed Workflow request"));
    }
}
