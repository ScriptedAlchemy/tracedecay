//! Workflow effect journaling and receipt-to-outcome translation.

use serde::Serialize;
use tracedecay_contracts::{
    ApplicationContractError, ApplicationOutcome, ApplicationProblem, ApplicationProblemDetailV1,
    AuthorityReceipt, Deadline, EffectId, EffectTermination, IdempotencyKey, LegalAction,
    PolicyDecisionRef, RequestContext, RequestId, RetryDirective, SafeDiagnostic, TaskHandoffError,
    TaskHandoffGrant, TaskHandoffRedeemed, WorkflowCoordinationError,
    WorkflowDefinitionDisposition, WorkflowEffectAuthorityErrorV1, WorkflowEffectAuthorityPortV1,
    WorkflowEffectIdentityV1, WorkflowEffectOperationV1, WorkflowEffectOutcomeV1,
    WorkflowEffectPreparedV1, WorkflowEffectProblemV1, WorkflowEffectReceiptContextV1,
    WorkflowEffectSuccessV1, WorkflowEffectTerminalV1,
};
use tracedecay_domain::{
    ComponentVersion, ManifestDigest, UtcMicros, canonical_sha256, sha256_hex_suffix,
};
use tracedecay_tool_catalog::UseCaseId;

use tracedecay_daemon_protocol::{
    DaemonInvocationOutcome, DaemonInvocationResponse, WorkflowApplicationOutcome,
};
use tracedecay_domain::errors::TraceDecayError;

use super::workflow_run_control::{
    workflow_coordination_problem, workflow_final_conflict, workflow_invalid_request,
    workflow_not_found, workflow_reset_required, workflow_runtime_unavailable,
};
use super::{RegisteredWorkRuntime, work_command_effect, work_effect, work_evidence_packet};
use tracedecay_contracts::now_micros;

#[allow(clippy::too_many_arguments)]
pub(super) fn complete_workflow_run_effect(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: Result<tracedecay_domain::WorkflowRunProjection, ApplicationProblem>,
    observed_at: UtcMicros,
    deadline: Deadline,
    wrap: fn(
        ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>,
    ) -> WorkflowApplicationOutcome,
) -> DaemonInvocationResponse {
    let result = match result {
        Ok(result) => result,
        Err(problem) => return DaemonInvocationResponse::application_problem(request_id, problem),
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
            return DaemonInvocationResponse::application_problem(
                request_id,
                workflow_runtime_unavailable(),
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
    result: Result<T, ApplicationProblem>,
    observed_at: UtcMicros,
    deadline: Deadline,
    wrap: fn(ApplicationOutcome<T>) -> WorkflowApplicationOutcome,
) -> DaemonInvocationResponse
where
    T: Serialize,
{
    let result = match result {
        Ok(result) => result,
        Err(problem) => return DaemonInvocationResponse::application_problem(request_id, problem),
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
            return DaemonInvocationResponse::application_problem(
                request_id,
                workflow_runtime_unavailable(),
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
            return DaemonInvocationResponse::application_problem(
                request_id,
                workflow_invalid_request(),
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
            return DaemonInvocationResponse::application_problem(
                request_id,
                workflow_runtime_unavailable(),
            );
        }
    };
    let receipt_binding_digest = match receipt_context.binding_digest() {
        Ok(digest) => digest,
        Err(_) => {
            return DaemonInvocationResponse::application_problem(
                request_id,
                workflow_runtime_unavailable(),
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
            return DaemonInvocationResponse::application_problem(
                request_id,
                workflow_runtime_unavailable(),
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
            return DaemonInvocationResponse::application_problem(
                request_id,
                workflow_runtime_unavailable(),
            );
        }
    };
    let prepared = if identity.deadline().is_elapsed_at(now_micros()) {
        WorkflowEffectPreparedV1::problem(
            identity.input_digest().clone(),
            WorkflowEffectProblemV1::TimedOut,
        )
    } else {
        prepared
    };
    let record = match authority.execute_effect(&identity, &prepared, now_micros()) {
        Ok(record) => record,
        Err(WorkflowEffectAuthorityErrorV1::ResetRequired) => {
            return DaemonInvocationResponse::application_problem(
                request_id,
                workflow_reset_required(),
            );
        }
        Err(
            WorkflowEffectAuthorityErrorV1::IdentityConflict
            | WorkflowEffectAuthorityErrorV1::InvalidTransition
            | WorkflowEffectAuthorityErrorV1::Unavailable(_),
        ) => {
            return DaemonInvocationResponse::application_problem(
                request_id,
                workflow_runtime_unavailable(),
            );
        }
    };
    let Some(terminal) = record.terminal() else {
        return DaemonInvocationResponse::application_problem(
            request_id,
            workflow_runtime_unavailable(),
        );
    };
    // `execute_effect` has durably published this terminal before returning
    // it. Wake project recovery even if response translation below fails.
    registered.durable_write_signal.bump();
    let outcome = match workflow_effect_outcome(terminal) {
        Ok(outcome) => outcome,
        Err(problem) => return DaemonInvocationResponse::application_problem(request_id, problem),
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
        sha256_hex_suffix(digest.as_str()).ok_or(ApplicationContractError::Inconsistent {
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
    let suffix =
        sha256_hex_suffix(input_digest.as_str()).ok_or(ApplicationContractError::Inconsistent {
            field: "Work input digest",
        })?;
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

pub(super) fn workflow_storage_problem(error: &TraceDecayError) -> ApplicationProblem {
    match error {
        tracedecay_domain::errors::TraceDecayError::ResetRequired { authority, .. }
            if authority == "workflow" =>
        {
            workflow_reset_required()
        }
        _ => workflow_runtime_unavailable(),
    }
}

/// Journals a refusal decided before the effect ran, or returns the problem
/// to answer unjournaled when it is not a final answer for this request.
pub(super) fn prepared_refusal(
    input_digest: &ManifestDigest,
    problem: ApplicationProblem,
) -> Result<WorkflowEffectPreparedV1, ApplicationProblem> {
    WorkflowEffectProblemV1::refused(problem)
        .map(|problem| WorkflowEffectPreparedV1::problem(input_digest.clone(), problem))
}

fn handoff_token_conflict() -> ApplicationProblem {
    workflow_final_conflict(
        "workflow.handoff.token_conflict",
        "another handoff already uses this secret; issue the handoff with a new secret",
        LegalAction::CorrectRequest,
    )
}

fn handoff_replayed() -> ApplicationProblem {
    workflow_final_conflict(
        "workflow.handoff.replayed",
        "this handoff was already redeemed; ask its issuer for a new handoff",
        LegalAction::Reauthorize,
    )
}

fn handoff_expired() -> ApplicationProblem {
    ApplicationProblem::Stale {
        diagnostic: SafeDiagnostic {
            code: "workflow.handoff.expired".to_owned(),
            message:
                "this handoff expired before it was redeemed; ask its issuer for a new handoff"
                    .to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::Reauthorize],
        detail: None,
    }
}

/// The typed answer for a journaled refusal. A timed-out effect is not a
/// refusal: it is answered as an effect whose termination is `timed_out`.
fn workflow_effect_refusal(problem: &WorkflowEffectProblemV1) -> Option<ApplicationProblem> {
    Some(match problem {
        WorkflowEffectProblemV1::Refused(problem) => problem.clone(),
        WorkflowEffectProblemV1::NotFoundOrNotAuthorized => workflow_not_found(),
        WorkflowEffectProblemV1::DefinitionContentConflict => {
            workflow_coordination_problem(WorkflowCoordinationError::ImmutableDefinitionConflict)
        }
        WorkflowEffectProblemV1::LifecycleRevisionStale {
            requested_revision,
            current_revision,
        } => ApplicationProblem::from_detail(ApplicationProblemDetailV1::StalePrecondition {
            field: "expected_revision".to_owned(),
            requested: *requested_revision,
            current: *current_revision,
        }),
        WorkflowEffectProblemV1::IllegalLifecycleTransition {
            current_state,
            current_revision,
        } => ApplicationProblem::conflict(
            "workflow.lifecycle.illegal_transition",
            format!(
                "the definition disposition is {state} at revision {current_revision}; this lifecycle operation has no transition from {state}",
                state = current_state.as_str()
            ),
        ),
        WorkflowEffectProblemV1::HandoffTokenConflict => handoff_token_conflict(),
        WorkflowEffectProblemV1::HandoffReplayed => handoff_replayed(),
        WorkflowEffectProblemV1::HandoffExpired => handoff_expired(),
        WorkflowEffectProblemV1::TimedOut => return None,
    })
}

fn workflow_effect_outcome(
    terminal: &WorkflowEffectTerminalV1,
) -> Result<WorkflowApplicationOutcome, ApplicationProblem> {
    let unavailable = |_| workflow_runtime_unavailable();
    match terminal.outcome() {
        WorkflowEffectOutcomeV1::Problem(problem) => {
            if let Some(problem) = workflow_effect_refusal(problem) {
                return Err(problem);
            }
            let termination = EffectTermination::TimedOut;
            match terminal.identity().operation() {
                WorkflowEffectOperationV1::RegisterDefinition => work_effect::<
                    tracedecay_domain::WorkflowDefinition,
                >(
                    terminal, None, termination
                )
                .map(WorkflowApplicationOutcome::RegisterDefinition)
                .map_err(unavailable),
                WorkflowEffectOperationV1::ActivateDefinition => {
                    work_effect::<WorkflowDefinitionDisposition>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::ActivateDefinition)
                        .map_err(unavailable)
                }
                WorkflowEffectOperationV1::RetireDefinition => {
                    work_effect::<WorkflowDefinitionDisposition>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::RetireDefinition)
                        .map_err(unavailable)
                }
                WorkflowEffectOperationV1::RejectDefinition => {
                    work_effect::<WorkflowDefinitionDisposition>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::RejectDefinition)
                        .map_err(unavailable)
                }
                WorkflowEffectOperationV1::HandoffIssue => {
                    work_effect::<TaskHandoffGrant>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::HandoffIssue)
                        .map_err(unavailable)
                }
                WorkflowEffectOperationV1::HandoffRedeem => {
                    work_effect::<TaskHandoffRedeemed>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::HandoffRedeem)
                        .map_err(unavailable)
                }
            }
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionRegistered(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::RegisterDefinition)
            .map_err(unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionActivated(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::ActivateDefinition)
            .map_err(unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionRetired(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::RetireDefinition)
            .map_err(unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionRejected(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::RejectDefinition)
            .map_err(unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::HandoffIssued(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::HandoffIssue)
            .map_err(unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::HandoffRedeemed(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::HandoffRedeem)
            .map_err(unavailable)
        }
    }
}

/// Handoff denial shares the concealed not-found answer with absence, so a
/// probe cannot learn whether a scope exists for another actor.
pub(super) fn task_handoff_refusal(error: TaskHandoffError) -> ApplicationProblem {
    let invalid = ApplicationProblem::invalid_request;
    match error {
        TaskHandoffError::AuthorityUnavailable(_) => workflow_runtime_unavailable(),
        TaskHandoffError::Missing
        | TaskHandoffError::ScopeMismatch
        | TaskHandoffError::Unauthorized => workflow_not_found(),
        TaskHandoffError::InvalidToken => invalid(
            "workflow.handoff.invalid_secret",
            "secret is not a valid handoff bearer secret",
        ),
        TaskHandoffError::InvalidScope => invalid(
            "workflow.handoff.invalid_scope",
            "scope failed structural validation",
        ),
        TaskHandoffError::InvalidFrontier => invalid(
            "workflow.handoff.invalid_frontier",
            "frontier must name the scope's task_id and be issued by its from_actor_id",
        ),
        TaskHandoffError::InvalidExpiry => invalid(
            "workflow.handoff.invalid_expiry",
            "handoff lifetime could not be derived from the issue time",
        ),
        TaskHandoffError::Conflict => handoff_token_conflict(),
        TaskHandoffError::Expired => handoff_expired(),
        TaskHandoffError::Replay => handoff_replayed(),
    }
}
