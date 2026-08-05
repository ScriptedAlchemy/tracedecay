//! Closed CLI binding for daemon-owned Workflow application operations.
//!
//! The adapter decodes one strict request DTO, resolves the project-scoped
//! daemon route, and returns the daemon's canonical application outcome. It
//! owns no workflow state, scheduling, retry, provider, or persistence logic.

use std::path::PathBuf;

use serde_json::Value;
use tracedecay_api::WorkflowOperation;
use tracedecay_application::{
<<<<<<< HEAD
    ApplicationEnvelope, ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult,
    CancellationSignal, Deadline, LegalAction, ResultContractRef, RetryDirective, SafeDiagnostic,
    TaskHandoffGrantV1, TaskHandoffIssueRequestV1, TaskHandoffRedeemRequestV1,
    TaskHandoffRedeemedV1, WorkflowActivationV1, WorkflowDefinitionActivateRequestV1,
    WorkflowDefinitionRegisterRequestV1, WorkflowExecutionTruthV1, WorkflowFanOutRequestV1,
    workflow_executable_binding_registry,
=======
    ApplicationEnvelope, ApplicationOutcome, ApplicationProblem, ApplicationProblemEnvelope,
    ApplicationResult, CancellationSignal, Deadline, LegalAction, ResultContractRef,
    RetryDirective, SafeDiagnostic, TaskHandoffIssueRequestV1, TaskHandoffRedeemRequestV1,
    WorkflowDefinitionActivateRequestV1, WorkflowDefinitionRegisterRequestV1,
    WorkflowFanOutRequestV1, workflow_executable_binding_registry,
>>>>>>> 5c9cc38c0dd1b9707cedef63faee1b21189cd510
};
use tracedecay_domain::{UtcMicros, WorkflowDefinitionV1};
use tracedecay_tool_catalog::{ExecutableBindingV1, OperationId};

use crate::daemon::DaemonHandshake;
use crate::daemon_client::{
    DaemonInvocationClient, InvocationCancellationPolicy, invocation_now_micros,
};
use crate::daemon_contract::{
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationRequest,
    WorkflowApplicationInvocationV1, WorkflowApplicationOutcomeV1,
};
use crate::errors::{Result, TraceDecayError};
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};

<<<<<<< HEAD
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
=======
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
>>>>>>> 5c9cc38c0dd1b9707cedef63faee1b21189cd510
}

pub async fn invoke_workflow_cli(
    project_root: PathBuf,
    operation: WorkflowOperation,
    body: Value,
<<<<<<< HEAD
) -> Result<WorkflowCliInvocationResult> {
    let binding = workflow_binding(operation)?;
=======
) -> Result<ApplicationResult<Value>> {
    let result_contract = operation.result_contract()?;
>>>>>>> 5c9cc38c0dd1b9707cedef63faee1b21189cd510
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
<<<<<<< HEAD
        decode(operation, body)?,
=======
        invocation,
>>>>>>> 5c9cc38c0dd1b9707cedef63faee1b21189cd510
        observed_at,
        deadline.clone(),
        cancellation.context(),
    );
    let handshake = DaemonHandshake::for_current_client(Some(project_root), None, false, false)?;
<<<<<<< HEAD
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
=======
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
    outcome: WorkflowApplicationOutcomeV1,
) -> Result<ApplicationOutcome<Value>> {
    let outcome = match outcome {
        WorkflowApplicationOutcomeV1::RegisterDefinition(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcomeV1::ActivateDefinition(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcomeV1::ExecuteFanOut(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcomeV1::HandoffIssue(outcome) => serde_json::to_value(outcome),
        WorkflowApplicationOutcomeV1::HandoffRedeem(outcome) => serde_json::to_value(outcome),
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
>>>>>>> 5c9cc38c0dd1b9707cedef63faee1b21189cd510
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

<<<<<<< HEAD
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

=======
>>>>>>> 5c9cc38c0dd1b9707cedef63faee1b21189cd510
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
