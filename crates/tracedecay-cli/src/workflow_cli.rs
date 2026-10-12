//! Closed CLI binding for daemon-owned Workflow application operations.
//!
//! The adapter decodes one strict request DTO, resolves the project-scoped
//! daemon route, and returns the daemon's canonical application outcome. It
//! owns no workflow state, scheduling, retry, provider, or persistence logic.

use serde_json::Value;
use tracedecay_api::WorkflowOperation;
use tracedecay_contracts::{
    ApplicationOutcome, ResultContractRef, TaskHandoffIssueRequest, TaskHandoffRedeemRequest,
    WorkflowDefinitionActivateRequest, WorkflowDefinitionDiffRequest, WorkflowDefinitionGetRequest,
    WorkflowDefinitionHistoryRequest, WorkflowDefinitionListRequest,
    WorkflowDefinitionRegisterRequest, WorkflowDefinitionRejectRequest,
    WorkflowDefinitionRetireRequest, WorkflowDefinitionValidateRequest,
    workflow_executable_binding_registry,
};
use tracedecay_tool_catalog::OperationId;

use tracedecay_daemon_protocol::{WorkflowApplicationInvocation, WorkflowApplicationOutcome};
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::application_cli::{WORKFLOW, config_error};

pub(crate) fn workflow_catalog(operation: WorkflowOperation) -> Result<(ResultContractRef, u64)> {
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
    Ok((
        ResultContractRef::from_schema(binding.result_schema().schema_ref()),
        binding.deadline().maximum_millis(),
    ))
}

pub(crate) fn decode_workflow_invocation(
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
            decode::<tracedecay_contracts::WorkflowRunStartRequest>(body)
                .map(|request| WorkflowApplicationInvocation::StartRun(Box::new(request)))
        }
        WorkflowOperation::PauseRun => {
            decode::<tracedecay_contracts::WorkflowRunPauseRequest>(body)
                .map(WorkflowApplicationInvocation::PauseRun)
        }
        WorkflowOperation::ResumeRun => {
            decode::<tracedecay_contracts::WorkflowRunResumeRequest>(body)
                .map(WorkflowApplicationInvocation::ResumeRun)
        }
        WorkflowOperation::CancelRun => {
            decode::<tracedecay_contracts::WorkflowRunCancelRequest>(body)
                .map(WorkflowApplicationInvocation::CancelRun)
        }
        WorkflowOperation::GetRun => decode::<tracedecay_contracts::WorkflowRunGetRequest>(body)
            .map(WorkflowApplicationInvocation::GetRun),
    }
}

pub(crate) fn workflow_outcome_matches(
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

pub(crate) fn erase_workflow_outcome(
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

fn decode<T>(body: Value) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    WORKFLOW.decode(body)
}

#[cfg(test)]
mod reset_problem_tests {
    use tracedecay_contracts::{ApplicationProblem, LegalAction, RetryDirective};
    use tracedecay_daemon_protocol::DaemonInvocationProblem;

    use super::WORKFLOW;

    #[test]
    fn daemon_workflow_reset_remains_a_typed_cli_problem() {
        let problem = WORKFLOW.daemon_problem(DaemonInvocationProblem::ResetRequired);
        let ApplicationProblem::ResetRequired {
            diagnostic,
            retry,
            legal_actions,
            ..
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

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_api::WorkflowOperation;
    use tracedecay_domain::UtcMicros;
    use tracedecay_tool_catalog::OperationId;

    use super::{
        WORKFLOW, decode_workflow_invocation, workflow_catalog,
        workflow_executable_binding_registry,
    };
    use crate::application_cli::deadline_after;
    use tracedecay_contracts::Deadline;
    use tracedecay_domain::errors::Result;

    fn workflow_cli_deadline(
        operation: WorkflowOperation,
        observed_at: UtcMicros,
    ) -> Result<Deadline> {
        let (_, maximum_millis) = workflow_catalog(operation)?;
        deadline_after(WORKFLOW, maximum_millis, observed_at)
    }

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
