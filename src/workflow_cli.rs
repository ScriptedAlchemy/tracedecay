//! Closed CLI binding for daemon-owned Workflow application operations.
//!
//! The adapter decodes one strict request DTO, resolves the project-scoped
//! daemon route, and returns the daemon's canonical application outcome. It
//! owns no workflow state, scheduling, retry, provider, or persistence logic.

use std::path::PathBuf;

use serde_json::Value;
use tracedecay_api::WorkflowOperation;
use tracedecay_application::{
    ApplicationEnvelope, ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult,
    CancellationSignal, Deadline, LegalAction, ResultContractRef, RetryDirective, SafeDiagnostic,
    TaskHandoffGrantV1, TaskHandoffIssueRequestV1, TaskHandoffRedeemRequestV1,
    TaskHandoffRedeemedV1, WorkflowActivationV1, WorkflowDefinitionActivateRequestV1,
    WorkflowDefinitionRegisterRequestV1, WorkflowExecutionTruthV1, WorkflowFanOutRequestV1,
    workflow_executable_binding_registry,
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
/// Each variant retains the operation's catalogued result type, so the CLI
/// never converts a daemon outcome into an untyped JSON intermediary.
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
        decode(operation, body)?,
        observed_at,
        deadline,
        cancellation.context(),
    );
    let handshake = DaemonHandshake::for_current_client(Some(project_root), None, false, false)?;
    let response = DaemonInvocationClient::for_current(handshake)?
        .invoke(request)
        .await?;
    Ok(workflow_result(
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

fn decode(operation: WorkflowOperation, body: Value) -> Result<WorkflowApplicationInvocationV1> {
    match operation {
        WorkflowOperation::RegisterDefinition => {
            decode_request::<WorkflowDefinitionRegisterRequestV1>(body)
                .map(WorkflowApplicationInvocationV1::RegisterDefinition)
        }
        WorkflowOperation::ActivateDefinition => {
            decode_request::<WorkflowDefinitionActivateRequestV1>(body)
                .map(WorkflowApplicationInvocationV1::ActivateDefinition)
        }
        WorkflowOperation::ExecuteFanOut => decode_request::<WorkflowFanOutRequestV1>(body)
            .map(Box::new)
            .map(WorkflowApplicationInvocationV1::ExecuteFanOut),
        WorkflowOperation::HandoffIssue => decode_request::<TaskHandoffIssueRequestV1>(body)
            .map(WorkflowApplicationInvocationV1::HandoffIssue),
        WorkflowOperation::HandoffRedeem => decode_request::<TaskHandoffRedeemRequestV1>(body)
            .map(WorkflowApplicationInvocationV1::HandoffRedeem),
    }
}

fn workflow_result(
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
                outcome: WorkflowApplicationOutcomeV1::RegisterDefinition(outcome),
            },
        ) => WorkflowCliInvocationResult::RegisterDefinition(Ok(ApplicationEnvelope {
            contract,
            request_id,
            scope,
            outcome,
        })),
        (
            WorkflowOperation::ActivateDefinition,
            DaemonInvocationOutcome::WorkflowApplication {
                scope,
                outcome: WorkflowApplicationOutcomeV1::ActivateDefinition(outcome),
            },
        ) => WorkflowCliInvocationResult::ActivateDefinition(Ok(ApplicationEnvelope {
            contract,
            request_id,
            scope,
            outcome,
        })),
        (
            WorkflowOperation::ExecuteFanOut,
            DaemonInvocationOutcome::WorkflowApplication {
                scope,
                outcome: WorkflowApplicationOutcomeV1::ExecuteFanOut(outcome),
            },
        ) => WorkflowCliInvocationResult::ExecuteFanOut(Ok(ApplicationEnvelope {
            contract,
            request_id,
            scope,
            outcome,
        })),
        (
            WorkflowOperation::HandoffIssue,
            DaemonInvocationOutcome::WorkflowApplication {
                scope,
                outcome: WorkflowApplicationOutcomeV1::HandoffIssue(outcome),
            },
        ) => WorkflowCliInvocationResult::HandoffIssue(Ok(ApplicationEnvelope {
            contract,
            request_id,
            scope,
            outcome,
        })),
        (
            WorkflowOperation::HandoffRedeem,
            DaemonInvocationOutcome::WorkflowApplication {
                scope,
                outcome: WorkflowApplicationOutcomeV1::HandoffRedeem(outcome),
            },
        ) => WorkflowCliInvocationResult::HandoffRedeem(Ok(ApplicationEnvelope {
            contract,
            request_id,
            scope,
            outcome,
        })),
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
                code: "workflow.protocol_unavailable".to_owned(),
                message: "The Workflow application protocol is unavailable".to_owned(),
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

fn decode_request<T>(body: Value) -> Result<T>
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
    use tracedecay_application::{
        ApplicationProblem, RequestId, ResultContractRef, RetryDirective,
    };
    use tracedecay_tool_catalog::SchemaId;

    use super::{WorkflowCliInvocationResult, decode, workflow_result};

    #[test]
    fn closed_binding_rejects_unknown_request_fields_before_daemon_dispatch() {
        let error = decode(
            WorkflowOperation::HandoffRedeem,
            json!({"unexpected": true}),
        )
        .expect_err("strict DTO must reject unknown fields");
        assert!(error.to_string().contains("invalid typed Workflow request"));
    }

    #[test]
    fn workflow_result_preserves_canonical_application_problem() {
        let contract = ResultContractRef::new(
            SchemaId::new("schema.workflow.handoff_redeem.result").unwrap(),
            1,
        )
        .unwrap();
        let request_id = RequestId::new("request.cli.workflow.problem").unwrap();

        let outcome = workflow_result(
            WorkflowOperation::HandoffRedeem,
            contract.clone(),
            request_id.clone(),
            crate::daemon_contract::DaemonInvocationOutcome::ApplicationProblem {
                problem: ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
            },
        );

        let WorkflowCliInvocationResult::HandoffRedeem(Err(problem)) = outcome else {
            panic!("Workflow application problem must retain its typed operation result");
        };
        assert_eq!(problem.contract, contract);
        assert_eq!(problem.request_id, request_id);
        assert_eq!(problem.problem.code, "not_found_or_not_authorized");
    }
}
