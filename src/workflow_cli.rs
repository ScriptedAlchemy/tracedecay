//! Closed CLI binding for daemon-owned Workflow application operations.
//!
//! The adapter decodes one strict request DTO, resolves the project-scoped
//! daemon route, and returns the daemon's canonical application outcome. It
//! owns no workflow state, scheduling, retry, provider, or persistence logic.

use std::path::PathBuf;

use serde_json::{Value, json};
use tracedecay_api::WorkflowOperation;
use tracedecay_application::{
    CancellationSignal, Deadline, TaskHandoffIssueRequestV1, TaskHandoffRedeemRequestV1,
    WorkflowDefinitionActivateRequestV1, WorkflowDefinitionRegisterRequestV1,
    WorkflowFanOutRequestV1, workflow_executable_binding_registry,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::OperationId;

use crate::daemon::DaemonHandshake;
use crate::daemon_client::{DaemonInvocationClient, invocation_now_micros};
use crate::daemon_contract::{
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationRequest,
    WorkflowApplicationInvocationV1, WorkflowApplicationOutcomeV1,
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

    fn verify_catalog_binding(self) -> Result<()> {
        let operation_id = OperationId::new(self.canonical().operation_id_str().to_owned())
            .map_err(config_error)?;
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

    fn decode(self, body: Value) -> Result<WorkflowApplicationInvocationV1> {
        match self {
            Self::RegisterDefinition => decode::<WorkflowDefinitionRegisterRequestV1>(body)
                .map(WorkflowApplicationInvocationV1::RegisterDefinition),
            Self::ActivateDefinition => decode::<WorkflowDefinitionActivateRequestV1>(body)
                .map(WorkflowApplicationInvocationV1::ActivateDefinition),
            Self::ExecuteFanOut => decode::<WorkflowFanOutRequestV1>(body)
                .map(Box::new)
                .map(WorkflowApplicationInvocationV1::ExecuteFanOut),
            Self::HandoffIssue => decode::<TaskHandoffIssueRequestV1>(body)
                .map(WorkflowApplicationInvocationV1::HandoffIssue),
            Self::HandoffRedeem => decode::<TaskHandoffRedeemRequestV1>(body)
                .map(WorkflowApplicationInvocationV1::HandoffRedeem),
        }
    }

    fn matches(self, outcome: &WorkflowApplicationOutcomeV1) -> bool {
        matches!(
            (self, outcome),
            (
                Self::RegisterDefinition,
                WorkflowApplicationOutcomeV1::RegisterDefinition(_)
            ) | (
                Self::ActivateDefinition,
                WorkflowApplicationOutcomeV1::ActivateDefinition(_)
            ) | (
                Self::ExecuteFanOut,
                WorkflowApplicationOutcomeV1::ExecuteFanOut(_)
            ) | (
                Self::HandoffIssue,
                WorkflowApplicationOutcomeV1::HandoffIssue(_)
            ) | (
                Self::HandoffRedeem,
                WorkflowApplicationOutcomeV1::HandoffRedeem(_)
            )
        )
    }
}

pub async fn invoke_workflow_cli(
    project_root: PathBuf,
    operation: WorkflowCliOperation,
    body: Value,
) -> Result<Value> {
    operation.verify_catalog_binding()?;
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
        operation.decode(body)?,
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
            if operation.matches(&outcome) =>
        {
            Ok(json!({
                "operation": operation.as_str(),
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

    use super::WorkflowCliOperation;

    #[test]
    fn closed_binding_rejects_unknown_request_fields_before_daemon_dispatch() {
        let error = WorkflowCliOperation::HandoffRedeem
            .decode(json!({"unexpected": true}))
            .expect_err("strict DTO must reject unknown fields");
        assert!(error.to_string().contains("invalid typed Workflow request"));
    }
}
