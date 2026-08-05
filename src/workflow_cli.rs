//! Closed CLI binding for daemon-owned Workflow application operations.
//!
//! The adapter decodes one strict request DTO, resolves the project-scoped
//! daemon route, and reconstructs only admitted Workflow effect outcomes as
//! canonical application envelopes. It owns no workflow state, scheduling,
//! retry, provider, or persistence logic.

use std::path::PathBuf;

use serde_json::Value;
use tracedecay_api::WorkflowOperation;
use tracedecay_application::{
    ApplicationEnvelope, ApplicationOutcome, ApplicationProblem, ApplicationProblemEnvelope,
    ApplicationResult, CancellationSignal, Deadline, LegalAction, ResultContractRef,
    RetryDirective, SafeDiagnostic, TaskHandoffGrantV1, TaskHandoffIssueRequestV1,
    TaskHandoffRedeemRequestV1, TaskHandoffRedeemedV1, WorkflowActivationV1,
    WorkflowDefinitionActivateRequestV1, WorkflowDefinitionRegisterRequestV1,
    WorkflowExecutionTruthV1, WorkflowFanOutRequestV1, workflow_executable_binding_registry,
};
use tracedecay_domain::{UtcMicros, WorkflowDefinitionV1};
use tracedecay_tool_catalog::{ExecutableBindingV1, OperationId};

use crate::daemon::DaemonHandshake;
use crate::daemon_client::{DaemonInvocationClient, invocation_now_micros};
use crate::daemon_contract::{
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationRequest,
    WorkflowApplicationInvocationV1, WorkflowApplicationOutcomeV1,
};
use crate::errors::{Result, TraceDecayError};
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};

/// One typed canonical application result for the selected Workflow operation.
///
/// Each variant retains the catalogued result type, so rendering stays on the
/// canonical application envelope rather than converting daemon output through
/// an untyped JSON intermediary.
pub enum WorkflowCliInvocationResult {
    RegisterDefinition(ApplicationResult<WorkflowDefinitionV1>),
    ActivateDefinition(ApplicationResult<WorkflowActivationV1>),
    ExecuteFanOut(ApplicationResult<WorkflowExecutionTruthV1>),
    HandoffIssue(ApplicationResult<TaskHandoffGrantV1>),
    HandoffRedeem(ApplicationResult<TaskHandoffRedeemedV1>),
}

pub async fn invoke_workflow_cli(
    project_root: PathBuf,
    operation: WorkflowOperation,
    body: Value,
) -> Result<WorkflowCliInvocationResult> {
    let binding = workflow_binding(operation)?;
    let request_id =
        mint_global_request_id(GlobalRequestSurface::Cli).map_err(|_| TraceDecayError::Config {
            message: "could not allocate a Workflow CLI request id".to_owned(),
        })?;
    let observed_at = invocation_now_micros();
    let deadline = Deadline::new(UtcMicros(
        observed_at.0.saturating_add(
            i64::try_from(binding.deadline().maximum_millis())
                .map_err(config_error)?
                .saturating_mul(1_000),
        ),
    ))
    .map_err(config_error)?;
    let cancellation =
        CancellationSignal::active(format!("cancellation.cli.{}", request_id.as_str()))
            .map_err(config_error)?;
    let request = DaemonInvocationRequest::workflow_application(
        request_id.as_str(),
        decode_request(operation, body)?,
        observed_at,
        deadline,
        cancellation.context(),
    );
    let handshake = DaemonHandshake::for_current_client(Some(project_root), None, false, false)?;
    let response = DaemonInvocationClient::for_current(handshake)?
        .invoke(request)
        .await?;
    Ok(reconstruct_workflow_effect(
        operation,
        ResultContractRef::from_schema(binding.result_schema().schema_ref()),
        request_id,
        response.outcome,
    ))
}

fn workflow_binding(operation: WorkflowOperation) -> Result<ExecutableBindingV1> {
    let operation_id =
        OperationId::new(operation.operation_id_str().to_owned()).map_err(config_error)?;
    let registry = workflow_executable_binding_registry().map_err(config_error)?;
    registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
        .cloned()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "Workflow operation {} is not advertised by this build",
                operation_id.as_str()
            ),
        })
}

fn decode_request(
    operation: WorkflowOperation,
    body: Value,
) -> Result<WorkflowApplicationInvocationV1> {
    match operation {
        WorkflowOperation::RegisterDefinition => {
            decode::<WorkflowDefinitionRegisterRequestV1>(body)
                .map(WorkflowApplicationInvocationV1::RegisterDefinition)
        }
        WorkflowOperation::ActivateDefinition => {
            decode::<WorkflowDefinitionActivateRequestV1>(body)
                .map(WorkflowApplicationInvocationV1::ActivateDefinition)
        }
        WorkflowOperation::ExecuteFanOut => decode::<WorkflowFanOutRequestV1>(body)
            .map(Box::new)
            .map(WorkflowApplicationInvocationV1::ExecuteFanOut),
        WorkflowOperation::HandoffIssue => decode::<TaskHandoffIssueRequestV1>(body)
            .map(WorkflowApplicationInvocationV1::HandoffIssue),
        WorkflowOperation::HandoffRedeem => decode::<TaskHandoffRedeemRequestV1>(body)
            .map(WorkflowApplicationInvocationV1::HandoffRedeem),
    }
}

fn reconstruct_workflow_effect(
    operation: WorkflowOperation,
    contract: ResultContractRef,
    request_id: tracedecay_application::RequestId,
    outcome: DaemonInvocationOutcome,
) -> WorkflowCliInvocationResult {
    match (operation, outcome) {
        (
            WorkflowOperation::RegisterDefinition,
            DaemonInvocationOutcome::WorkflowApplication {
                scope,
                outcome:
                    WorkflowApplicationOutcomeV1::RegisterDefinition(ApplicationOutcome::Effect(effect)),
            },
        ) => WorkflowCliInvocationResult::RegisterDefinition(Ok(ApplicationEnvelope::effect(
            contract, request_id, scope, effect,
        ))),
        (
            WorkflowOperation::ActivateDefinition,
            DaemonInvocationOutcome::WorkflowApplication {
                scope,
                outcome:
                    WorkflowApplicationOutcomeV1::ActivateDefinition(ApplicationOutcome::Effect(effect)),
            },
        ) => WorkflowCliInvocationResult::ActivateDefinition(Ok(ApplicationEnvelope::effect(
            contract, request_id, scope, effect,
        ))),
        (
            WorkflowOperation::ExecuteFanOut,
            DaemonInvocationOutcome::WorkflowApplication {
                scope,
                outcome:
                    WorkflowApplicationOutcomeV1::ExecuteFanOut(ApplicationOutcome::Effect(effect)),
            },
        ) => WorkflowCliInvocationResult::ExecuteFanOut(Ok(ApplicationEnvelope::effect(
            contract, request_id, scope, effect,
        ))),
        (
            WorkflowOperation::HandoffIssue,
            DaemonInvocationOutcome::WorkflowApplication {
                scope,
                outcome:
                    WorkflowApplicationOutcomeV1::HandoffIssue(ApplicationOutcome::Effect(effect)),
            },
        ) => WorkflowCliInvocationResult::HandoffIssue(Ok(ApplicationEnvelope::effect(
            contract, request_id, scope, effect,
        ))),
        (
            WorkflowOperation::HandoffRedeem,
            DaemonInvocationOutcome::WorkflowApplication {
                scope,
                outcome:
                    WorkflowApplicationOutcomeV1::HandoffRedeem(ApplicationOutcome::Effect(effect)),
            },
        ) => WorkflowCliInvocationResult::HandoffRedeem(Ok(ApplicationEnvelope::effect(
            contract, request_id, scope, effect,
        ))),
        (_, DaemonInvocationOutcome::ApplicationProblem { problem }) => {
            workflow_problem(operation, contract, request_id, problem)
        }
        (_, DaemonInvocationOutcome::Problem { problem }) => {
            workflow_problem(operation, contract, request_id, daemon_problem(problem))
        }
        _ => workflow_problem(
            operation,
            contract,
            request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "workflow.protocol_invalid_outcome".to_owned(),
                message: "The Workflow daemon returned a non-effect outcome".to_owned(),
            }),
        ),
    }
}

fn workflow_problem(
    operation: WorkflowOperation,
    contract: ResultContractRef,
    request_id: tracedecay_application::RequestId,
    problem: ApplicationProblem,
) -> WorkflowCliInvocationResult {
    let problem = ApplicationProblemEnvelope::new(contract, request_id, problem);
    match operation {
        WorkflowOperation::RegisterDefinition => {
            WorkflowCliInvocationResult::RegisterDefinition(Err(problem))
        }
        WorkflowOperation::ActivateDefinition => {
            WorkflowCliInvocationResult::ActivateDefinition(Err(problem))
        }
        WorkflowOperation::ExecuteFanOut => {
            WorkflowCliInvocationResult::ExecuteFanOut(Err(problem))
        }
        WorkflowOperation::HandoffIssue => WorkflowCliInvocationResult::HandoffIssue(Err(problem)),
        WorkflowOperation::HandoffRedeem => {
            WorkflowCliInvocationResult::HandoffRedeem(Err(problem))
        }
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

fn daemon_problem(problem: DaemonInvocationProblem) -> ApplicationProblem {
    match problem {
        DaemonInvocationProblem::InvalidRequest | DaemonInvocationProblem::UnsupportedRevision => {
            ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic {
                    code: "workflow.invalid_request".to_owned(),
                    message: "The Workflow application request is invalid".to_owned(),
                },
                retry: RetryDirective::Never,
                legal_actions: vec![LegalAction::CorrectRequest],
            }
        }
        DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        DaemonInvocationProblem::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "workflow.unavailable".to_owned(),
            message: "The Workflow application runtime is unavailable".to_owned(),
        }),
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

    use super::decode_request;

    #[test]
    fn closed_binding_rejects_unknown_request_fields_before_daemon_dispatch() {
        let error = decode_request(
            WorkflowOperation::HandoffRedeem,
            json!({"unexpected": true}),
        )
        .expect_err("strict DTO must reject unknown fields");
        assert!(error.to_string().contains("invalid typed Workflow request"));
    }
}
