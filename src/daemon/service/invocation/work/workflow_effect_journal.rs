//! Workflow effect journaling and receipt-to-outcome translation.

use serde::Serialize;
use tracedecay_application::{
    ApplicationContractError, ApplicationOutcome, AuthorityReceipt, Deadline, EffectId,
    EffectTermination, IdempotencyKey, PolicyDecisionRef, RequestContext, RequestId,
    TaskHandoffError, TaskHandoffGrant, TaskHandoffRedeemed, WorkflowDefinitionDisposition,
    WorkflowEffectAuthorityPortV1, WorkflowEffectIdentityV1, WorkflowEffectOperationV1,
    WorkflowEffectOutcomeV1, WorkflowEffectPreparedV1, WorkflowEffectProblemV1,
    WorkflowEffectReceiptContextV1, WorkflowEffectSuccessV1, WorkflowEffectTerminalV1,
};
use tracedecay_domain::{ComponentVersion, ManifestDigest, UtcMicros, canonical_sha256};
use tracedecay_tool_catalog::UseCaseId;

use crate::daemon_contract::{
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationResponse,
    WorkflowApplicationOutcome,
};
use crate::errors::TraceDecayError;

use super::super::current_micros;
use super::{RegisteredWorkRuntime, work_command_effect, work_effect, work_evidence_packet};

#[allow(clippy::too_many_arguments)]
pub(super) fn complete_workflow_run_effect(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: Result<tracedecay_domain::WorkflowRunProjection, DaemonInvocationProblem>,
    observed_at: UtcMicros,
    deadline: Deadline,
    wrap: fn(
        ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>,
    ) -> WorkflowApplicationOutcome,
) -> DaemonInvocationResponse {
    let result = match result {
        Ok(result) => result,
        Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
    };
    let outcome = match work_command_effect(
        registered,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        result,
        observed_at,
        deadline,
    ) {
        Ok(outcome) => wrap(outcome),
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::WorkflowApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn complete_workflow_read<T>(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: Result<T, DaemonInvocationProblem>,
    observed_at: UtcMicros,
    deadline: Deadline,
    wrap: fn(ApplicationOutcome<T>) -> WorkflowApplicationOutcome,
) -> DaemonInvocationResponse
where
    T: Serialize,
{
    let result = match result {
        Ok(result) => result,
        Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
    };
    let outcome = match work_evidence_packet(
        registered,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        result,
        observed_at,
        deadline,
    ) {
        Ok(evidence) => wrap(ApplicationOutcome::Evidence(evidence)),
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::WorkflowApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_journaled_workflow_effect(
    registered: &RegisteredWorkRuntime,
    authority: &impl WorkflowEffectAuthorityPortV1,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    prepared: WorkflowEffectPreparedV1,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> DaemonInvocationResponse {
    let operation = match workflow_effect_operation(operation_key) {
        Some(operation) => operation,
        None => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
    };
    let receipt_context = match workflow_effect_receipt_context(
        registered,
        context,
        operation_key,
        use_case,
        &input_digest,
        observed_at,
    ) {
        Ok(receipt_context) => receipt_context,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let receipt_binding_digest = match receipt_context.binding_digest() {
        Ok(digest) => digest,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let idempotency_key = match workflow_effect_idempotency_key(
        operation,
        operation_key,
        &canonical_request_id,
        context,
        &input_digest,
        &receipt_binding_digest,
    ) {
        Ok(key) => key,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let identity = match WorkflowEffectIdentityV1::new(
        operation,
        idempotency_key,
        canonical_request_id,
        context.actor().clone(),
        context.scope().clone(),
        input_digest,
        observed_at,
        deadline,
        receipt_context,
    ) {
        Ok(identity) => identity,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let prepared = if identity.deadline().is_elapsed_at(current_micros()) {
        WorkflowEffectPreparedV1::problem(
            identity.input_digest().clone(),
            WorkflowEffectProblemV1::TimedOut,
        )
    } else {
        prepared
    };
    let record = match authority.execute_effect(&identity, &prepared, current_micros()) {
        Ok(record) => record,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let Some(terminal) = record.terminal() else {
        return DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable);
    };
    let outcome = match workflow_effect_outcome(terminal) {
        Ok(outcome) => outcome,
        Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
    };
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::WorkflowApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

fn workflow_effect_idempotency_key(
    operation: WorkflowEffectOperationV1,
    operation_key: &str,
    request_id: &RequestId,
    context: &RequestContext,
    input_digest: &ManifestDigest,
    receipt_binding_digest: &ManifestDigest,
) -> Result<IdempotencyKey, ApplicationContractError> {
    if operation == WorkflowEffectOperationV1::HandoffRedeem {
        return WorkflowEffectIdentityV1::handoff_redeem_idempotency_key(
            request_id,
            context.actor(),
            context.scope(),
            receipt_binding_digest,
        );
    }
    let digest = canonical_sha256(&(
        "tracedecay.daemon.workflow-effect-idempotency.v1",
        operation_key,
        input_digest,
        context.actor(),
        context.scope(),
        receipt_binding_digest,
    ))?;
    let suffix =
        digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or(ApplicationContractError::Inconsistent {
                field: "Workflow effect idempotency digest",
            })?;
    IdempotencyKey::new(format!("workflow.{operation_key}.{suffix}"))
}

fn workflow_effect_receipt_context(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: &ManifestDigest,
    observed_at: UtcMicros,
) -> Result<WorkflowEffectReceiptContextV1, ApplicationContractError> {
    let policy_digest = canonical_sha256(&(
        "tracedecay.daemon.work-policy.v1",
        &registered.policy_digest,
        &registered.grant.digest,
        operation_key,
        &use_case,
    ))?;
    let policy = PolicyDecisionRef::new(
        format!("policy.daemon.work.{operation_key}.v1"),
        1,
        policy_digest,
        ComponentVersion::new("tracedecay.daemon.work-policy.v1").map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "Work policy evaluator",
            }
        })?,
    )?;
    let authority = AuthorityReceipt::from_context(context, policy, observed_at)?;
    let suffix = input_digest.as_str().strip_prefix("sha256:").ok_or(
        ApplicationContractError::Inconsistent {
            field: "Work input digest",
        },
    )?;
    let expected_state = canonical_sha256(&(
        "tracedecay.work.expected-state.v1",
        operation_key,
        input_digest,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "Work expected state",
    })?;
    let catalog_digest =
        canonical_sha256(&("tracedecay.work.catalog.v1", operation_key)).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "Work catalog digest",
            }
        })?;
    let privacy_digest = canonical_sha256(&(
        "tracedecay.work.privacy.v1",
        context.scope(),
        context.grant().disclosure,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "Work privacy digest",
    })?;
    Ok(WorkflowEffectReceiptContextV1::new(
        use_case,
        EffectId::new(format!("effect.work.{operation_key}.{suffix}"))?,
        authority,
        expected_state,
        registered.configuration_digest.clone(),
        catalog_digest,
        privacy_digest,
    ))
}

fn workflow_effect_operation(operation_key: &str) -> Option<WorkflowEffectOperationV1> {
    match operation_key {
        "register_definition" => Some(WorkflowEffectOperationV1::RegisterDefinition),
        "activate_definition" => Some(WorkflowEffectOperationV1::ActivateDefinition),
        "retire_definition" => Some(WorkflowEffectOperationV1::RetireDefinition),
        "reject_definition" => Some(WorkflowEffectOperationV1::RejectDefinition),
        "handoff_issue" => Some(WorkflowEffectOperationV1::HandoffIssue),
        "handoff_redeem" => Some(WorkflowEffectOperationV1::HandoffRedeem),
        _ => None,
    }
}

pub(super) fn workflow_storage_problem(error: &TraceDecayError) -> DaemonInvocationProblem {
    match error {
        crate::errors::TraceDecayError::ResetRequired { authority, .. }
            if authority == "workflow" =>
        {
            DaemonInvocationProblem::ResetRequired
        }
        _ => DaemonInvocationProblem::Unavailable,
    }
}

pub(super) fn workflow_effect_problem(problem: DaemonInvocationProblem) -> WorkflowEffectProblemV1 {
    match problem {
        DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            WorkflowEffectProblemV1::NotFoundOrNotAuthorized
        }
        DaemonInvocationProblem::InvalidRequest
        | DaemonInvocationProblem::UnsupportedRevision
        | DaemonInvocationProblem::ResetRequired => WorkflowEffectProblemV1::InvalidRequest,
        DaemonInvocationProblem::ApplicationContractViolation
        | DaemonInvocationProblem::Unavailable => WorkflowEffectProblemV1::Conflict,
    }
}

fn workflow_effect_daemon_problem(problem: WorkflowEffectProblemV1) -> DaemonInvocationProblem {
    match problem {
        WorkflowEffectProblemV1::NotFoundOrNotAuthorized => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        WorkflowEffectProblemV1::InvalidRequest
        | WorkflowEffectProblemV1::Conflict
        | WorkflowEffectProblemV1::TimedOut => DaemonInvocationProblem::InvalidRequest,
    }
}

fn workflow_effect_outcome(
    terminal: &WorkflowEffectTerminalV1,
) -> Result<WorkflowApplicationOutcome, DaemonInvocationProblem> {
    match terminal.outcome() {
        WorkflowEffectOutcomeV1::Problem(WorkflowEffectProblemV1::TimedOut) => {
            let termination = EffectTermination::TimedOut;
            match terminal.identity().operation() {
                WorkflowEffectOperationV1::RegisterDefinition => work_effect::<
                    tracedecay_domain::WorkflowDefinition,
                >(
                    terminal, None, termination
                )
                .map(WorkflowApplicationOutcome::RegisterDefinition)
                .map_err(|_| DaemonInvocationProblem::Unavailable),
                WorkflowEffectOperationV1::ActivateDefinition => {
                    work_effect::<WorkflowDefinitionDisposition>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::ActivateDefinition)
                        .map_err(|_| DaemonInvocationProblem::Unavailable)
                }
                WorkflowEffectOperationV1::RetireDefinition => {
                    work_effect::<WorkflowDefinitionDisposition>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::RetireDefinition)
                        .map_err(|_| DaemonInvocationProblem::Unavailable)
                }
                WorkflowEffectOperationV1::RejectDefinition => {
                    work_effect::<WorkflowDefinitionDisposition>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::RejectDefinition)
                        .map_err(|_| DaemonInvocationProblem::Unavailable)
                }
                WorkflowEffectOperationV1::HandoffIssue => {
                    work_effect::<TaskHandoffGrant>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::HandoffIssue)
                        .map_err(|_| DaemonInvocationProblem::Unavailable)
                }
                WorkflowEffectOperationV1::HandoffRedeem => {
                    work_effect::<TaskHandoffRedeemed>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::HandoffRedeem)
                        .map_err(|_| DaemonInvocationProblem::Unavailable)
                }
            }
        }
        WorkflowEffectOutcomeV1::Problem(problem) => Err(workflow_effect_daemon_problem(*problem)),
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionRegistered(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::RegisterDefinition)
            .map_err(|_| DaemonInvocationProblem::Unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionActivated(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::ActivateDefinition)
            .map_err(|_| DaemonInvocationProblem::Unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionRetired(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::RetireDefinition)
            .map_err(|_| DaemonInvocationProblem::Unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionRejected(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::RejectDefinition)
            .map_err(|_| DaemonInvocationProblem::Unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::HandoffIssued(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::HandoffIssue)
            .map_err(|_| DaemonInvocationProblem::Unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::HandoffRedeemed(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::HandoffRedeem)
            .map_err(|_| DaemonInvocationProblem::Unavailable)
        }
    }
}

pub(super) fn task_handoff_problem(error: TaskHandoffError) -> DaemonInvocationProblem {
    match error {
        TaskHandoffError::AuthorityUnavailable(_) => DaemonInvocationProblem::Unavailable,
        TaskHandoffError::Missing | TaskHandoffError::ScopeMismatch => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        TaskHandoffError::InvalidToken
        | TaskHandoffError::InvalidScope
        | TaskHandoffError::InvalidFrontier
        | TaskHandoffError::Unauthorized
        | TaskHandoffError::InvalidExpiry
        | TaskHandoffError::Conflict
        | TaskHandoffError::Expired
        | TaskHandoffError::Replay => DaemonInvocationProblem::InvalidRequest,
    }
}
