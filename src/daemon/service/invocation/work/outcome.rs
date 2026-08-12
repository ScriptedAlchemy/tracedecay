//! Work invocation response, evidence, and effect construction.

use serde::Serialize;
use tracedecay_application::{
    ApplicationContractError, ApplicationOutcome, ApplicationProblem, AuthorityReceipt,
    CancellationContext, CancellationObservation, CancellationStage, CapabilityGrantSnapshot,
    Deadline, EffectId, EffectReceipt, EffectResult, EffectTermination, EvidenceAuthority,
    EvidenceCoverage, EvidenceDomain, EvidenceIdentity, EvidencePacket, IdempotencyKey,
    OperationBudgetUsage, OperationReceipt, PageState, PolicyDecisionRef, ReconciliationState,
    RequestAdmission, RequestContext, RequestId, RetryDirective, SafeDiagnostic, TemporalState,
    WorkProjectionApplicationError, WorkflowEffectTerminalV1,
};
use tracedecay_domain::{ActorId, ComponentVersion, ManifestDigest, UtcMicros, canonical_sha256};
use tracedecay_tool_catalog::{CapabilityId, EffectClass, SortContractId, UseCaseId};

use crate::daemon_contract::{
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationResponse,
    WorkApplicationOutcomeV1,
};

use super::super::current_micros;
use super::{RegisteredWorkRuntime, application_problem};

pub(super) fn offer_work_blocked_interval_receipts(
    producer: Option<&tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
    canonical_project_scope: &str,
    receipts: &[tracedecay_domain::WorkBlockedIntervalReceiptV1],
) {
    let Some(producer) = producer else {
        return;
    };
    for receipt in receipts {
        let _ = tracedecay_usecases::observability::record_work_blocked_interval_observation(
            Some(producer),
            canonical_project_scope,
            &receipt,
        );
    }
}

/// Reports a work-product graph operation exactly as its service typed it.
///
/// Every arm restates a state the service already decided; none of them
/// substitutes an empty success for an absent or unreadable graph, because an
/// empty reading and an unavailable authority are different answers to the
/// caller. Absence and denial share the concealed
/// `not_found_or_not_authorized` answer so that probing an owner cannot reveal
/// which of the two it is.
pub(super) fn work_product_problem(
    error: tracedecay_application::WorkProductApplicationErrorV1,
) -> ApplicationProblem {
    use tracedecay_application::WorkProductApplicationErrorV1 as Error;

    match error {
        Error::NotAuthorized | Error::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        Error::Cancelled => ApplicationProblem::cancelled_before_admission(),
        Error::TimedOut => ApplicationProblem::timed_out_before_admission(),
        Error::InvalidRequest => ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "work.invalid_graph_operation".to_owned(),
                message: "The Work graph request is invalid".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![tracedecay_application::LegalAction::CorrectRequest],
        },
        Error::VersionConflict => ApplicationProblem::stale(SafeDiagnostic {
            code: "work.graph_version_conflict".to_owned(),
            message: "The Work graph version does not match the request".to_owned(),
        }),
        Error::RevisionConflict => ApplicationProblem::stale(SafeDiagnostic {
            code: "work.graph_revision_conflict".to_owned(),
            message: "The Work policy, configuration, or catalog revision changed".to_owned(),
        }),
        Error::IdempotencyConflict => ApplicationProblem::Conflict {
            diagnostic: SafeDiagnostic {
                code: "work.graph_idempotency_conflict".to_owned(),
                message: "The Work graph request key was reused with different input".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![tracedecay_application::LegalAction::CorrectRequest],
        },
        Error::GraphAuthorityUnavailable
        | Error::EventAuthorityUnavailable
        | Error::EvidenceAuthorityUnavailable
        | Error::ProposalAuthorityUnavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "work.graph_authority_unavailable".to_owned(),
            message: "The Work graph authority is unavailable".to_owned(),
        }),
    }
}

/// Mints the daemon-owned request context under which background attempt
/// execution persists transitions. The authority and scope are exactly the
/// registered runtime's; only the deadline is the runtime's own, because the
/// provider process outlives the request that started it.
pub(in crate::daemon::service::invocation) fn work_background_context(
    registered: &RegisteredWorkRuntime,
    identity: &tracedecay_domain::WorkAttemptIdentityV1,
) -> Result<RequestContext, ApplicationContractError> {
    const BACKGROUND_DEADLINE_MICROS: i64 = 86_400_000_000;
    let request_id = RequestId::new(format!(
        "work-attempt-exec-{}-{}-{}",
        identity.task_id().as_str(),
        identity.run_id().as_str(),
        identity.attempt_id().as_str()
    ))?;
    let deadline = Deadline::new(UtcMicros(
        current_micros()
            .0
            .saturating_add(BACKGROUND_DEADLINE_MICROS),
    ))?;
    let cancellation = CancellationContext::active(format!(
        "work-attempt-exec-{}",
        identity.attempt_id().as_str()
    ))?;
    RequestContext::new(
        registered.actor.clone(),
        registered.grant.scope.clone(),
        registered.grant.clone(),
        request_id,
        deadline,
        cancellation,
    )
}

/// A retained producer drain has no attempt identity to mint. It uses the
/// registered Work actor/grant context, so its durable receipt scans cannot
/// alias a caller request or an attempt execution.
pub(in crate::daemon::service::invocation) fn work_blocked_interval_recovery_context(
    actor: &ActorId,
    grant: &CapabilityGrantSnapshot,
) -> Result<RequestContext, ApplicationContractError> {
    const BACKGROUND_DEADLINE_MICROS: i64 = 86_400_000_000;
    let request_id = RequestId::new("work-blocked-interval-recovery")?;
    let deadline = Deadline::new(UtcMicros(
        current_micros()
            .0
            .saturating_add(BACKGROUND_DEADLINE_MICROS),
    ))?;
    let cancellation = CancellationContext::active("cancel.work-blocked-interval-recovery")?;
    RequestContext::new(
        actor.clone(),
        grant.scope.clone(),
        grant.clone(),
        request_id,
        deadline,
        cancellation,
    )
}

/// Maps a verified Work topology failure to the typed application problem the
/// attempt-list read reports. Absence of any Work events is the only
/// non-error state: it names an empty scope, not a failing authority.
pub(super) fn work_topology_problem(
    error: tracedecay_runtime_core::work_topology::WorkTopologyError,
) -> Result<tracedecay_application::WorkAttemptTopologyStateV1, ApplicationProblem> {
    use tracedecay_runtime_core::work_topology::WorkTopologyError;
    match error {
        WorkTopologyError::EmptyEvents => {
            Ok(tracedecay_application::WorkAttemptTopologyStateV1::Absent)
        }
        WorkTopologyError::Cancelled => Err(ApplicationProblem::Cancelled {
            stage: tracedecay_application::CancellationStage::DuringRead,
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        }),
        WorkTopologyError::BudgetExhausted => Err(ApplicationProblem::TimedOut {
            stage: tracedecay_application::CancellationStage::DuringRead,
            retry: RetryDirective::AfterDelay,
            legal_actions: Vec::new(),
        }),
        WorkTopologyError::GenerationMismatch => Err(ApplicationProblem::stale(SafeDiagnostic {
            code: "work.topology_generation_superseded".to_owned(),
            message: "The verified Work topology generation was superseded during the read"
                .to_owned(),
        })),
        WorkTopologyError::MixedAuthority
        | WorkTopologyError::NonCanonicalTasks
        | WorkTopologyError::DependencyCycle(_)
        | WorkTopologyError::Contract(_)
        | WorkTopologyError::Corrupt(_)
        | WorkTopologyError::Unavailable(_) => Err(work_topology_unavailable_problem(
            "the verified Work topology could not be served",
        )),
    }
}

pub(super) fn work_topology_unavailable_problem(message: &str) -> ApplicationProblem {
    ApplicationProblem::unavailable(SafeDiagnostic {
        code: "work.topology_unavailable".to_owned(),
        message: message.to_owned(),
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn work_projection_problem(error: WorkProjectionApplicationError) -> ApplicationProblem {
    match error {
        WorkProjectionApplicationError::Admission(problem) => problem,
        WorkProjectionApplicationError::InvalidPageSize => ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "work.invalid_page_size".to_owned(),
                message: "The Work projection page size is invalid".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![tracedecay_application::LegalAction::CorrectRequest],
        },
        WorkProjectionApplicationError::Port(
            tracedecay_application::WorkProjectionPortError::StaleCursor,
        ) => ApplicationProblem::stale(SafeDiagnostic {
            code: "work.stale_cursor".to_owned(),
            message: "The Work projection cursor is stale".to_owned(),
        }),
        WorkProjectionApplicationError::Port(
            tracedecay_application::WorkProjectionPortError::Unavailable,
        ) => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "work.projection_unavailable".to_owned(),
            message: "The Work projection authority is unavailable".to_owned(),
        }),
        WorkProjectionApplicationError::Port(
            tracedecay_application::WorkProjectionPortError::NotFoundOrNotAuthorized,
        ) => ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn complete_work_read<T>(
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
    wrap: fn(ApplicationOutcome<T>) -> WorkApplicationOutcomeV1,
) -> DaemonInvocationResponse
where
    T: Serialize,
{
    let result = match result {
        Ok(result) => result,
        Err(problem) => return application_problem(request_id, problem),
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
        DaemonInvocationOutcome::WorkApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn complete_work_effect<T>(
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
    wrap: fn(ApplicationOutcome<T>) -> WorkApplicationOutcomeV1,
) -> DaemonInvocationResponse
where
    T: Serialize,
{
    let result = match result {
        Ok(result) => result,
        Err(problem) => return application_problem(request_id, problem),
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
        Ok(effect) => wrap(effect),
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::WorkApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn work_command_effect<T>(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: T,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<ApplicationOutcome<T>, ApplicationContractError>
where
    T: Serialize,
{
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
    let suffix = input_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(ApplicationContractError::Inconsistent {
            field: "Work input digest",
        })?
        .to_owned();
    let idempotency_key = IdempotencyKey::new(format!("work.{operation_key}.{suffix}"))?;
    let expected_state = canonical_sha256(&(
        "tracedecay.work.expected-state.v1",
        operation_key,
        &input_digest,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "Work expected state",
    })?;
    let committed_state =
        canonical_sha256(&("tracedecay.work.committed-state.v1", operation_key, &result)).map_err(
            |_| ApplicationContractError::Inconsistent {
                field: "Work committed state",
            },
        )?;
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )?;
    let receipt = EffectReceipt {
        operation: use_case,
        request_id,
        actor: registered.actor.clone(),
        scope: context.scope().clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: idempotency_key.clone(),
        input_digest,
        expected_state: expected_state.clone(),
        policy_digest: authority.policy.digest.clone(),
        configuration_digest: registered.configuration_digest.clone(),
        catalog_digest: canonical_sha256(&("tracedecay.work.catalog.v1", operation_key)).map_err(
            |_| ApplicationContractError::Inconsistent {
                field: "Work catalog digest",
            },
        )?,
        privacy_digest: canonical_sha256(&(
            "tracedecay.work.privacy.v1",
            context.scope(),
            context.grant().disclosure,
        ))
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "Work privacy digest",
        })?,
        outcome: EffectTermination::Completed,
        committed_state: Some(committed_state),
        external_proof: None,
    };
    Ok(ApplicationOutcome::Effect(EffectResult::new(
        EffectId::new(format!("effect.work.{operation_key}.{suffix}"))?,
        EffectClass::Administrative,
        idempotency_key,
        authority,
        expected_state,
        execution,
        ReconciliationState::Reconciled,
        receipt,
        Some(result),
    )?))
}

pub(in crate::daemon::service::invocation) fn work_request_context(
    registered: &RegisteredWorkRuntime,
    request_id: &str,
    capability: &str,
    use_case: &str,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<(RequestContext, RequestId, UseCaseId), DaemonInvocationProblem> {
    let capability =
        CapabilityId::new(capability).map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let use_case = UseCaseId::new(use_case).map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let canonical_request_id =
        RequestId::new(request_id).map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
    let context = RequestContext::new(
        registered.actor.clone(),
        registered.grant.scope.clone(),
        registered.grant.clone(),
        canonical_request_id.clone(),
        deadline,
        cancellation,
    )
    .map_err(|_| DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
    if context.admission_at(observed_at) != RequestAdmission::Admitted
        || !context.allows(&capability, &use_case)
    {
        return Err(DaemonInvocationProblem::NotFoundOrNotAuthorized);
    }
    Ok((context, canonical_request_id, use_case))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn work_evidence_packet<T>(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    _request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: T,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<EvidencePacket<T>, ApplicationContractError>
where
    T: Serialize,
{
    let policy_digest = canonical_sha256(&(
        "tracedecay.daemon.work-read-policy.v1",
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
                field: "Work read policy evaluator",
            }
        })?,
    )?;
    let authority = AuthorityReceipt::from_context(context, policy, observed_at)?;
    let suffix = input_digest.as_str().strip_prefix("sha256:").ok_or(
        ApplicationContractError::Inconsistent {
            field: "Work read input digest",
        },
    )?;
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )?;
    Ok(EvidencePacket {
        temporal: TemporalState::current(execution.ended_at),
        authority,
        evidence_authorities: vec![EvidenceAuthority {
            evidence_id: EvidenceIdentity::new(format!("evidence.work.{operation_key}.{suffix}"))?,
            source_kind: "work_projection".to_owned(),
            producer: operation_key.to_owned(),
            scope: context.scope().clone(),
            revision: ComponentVersion::new("tracedecay.work-projection.v1")?,
            horizon: Some(execution.ended_at),
        }],
        coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Operational], 1, 1, 1)?,
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new(format!("sort.work.{operation_key}.v1"))?,
            1,
            Some(1),
            1,
        )?,
        execution,
        payload: Some(result),
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn work_effect<T>(
    terminal: &WorkflowEffectTerminalV1,
    result: Option<T>,
    termination: EffectTermination,
) -> Result<ApplicationOutcome<T>, ApplicationContractError>
where
    T: Serialize,
{
    let identity = terminal.identity();
    let receipt_context = identity.receipt_context();
    let committed_state = result
        .as_ref()
        .map(|result| {
            canonical_sha256(&(
                "tracedecay.work.committed-state.v1",
                identity.operation().as_str(),
                result,
            ))
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "Work committed state",
            })
        })
        .transpose()?;
    let execution = OperationReceipt {
        started_at: identity.started_at(),
        ended_at: terminal.ended_at(),
        effective_deadline: identity.deadline().clone(),
        cancellation: workflow_effect_terminal_observation(termination, terminal.ended_at()),
        budget: OperationBudgetUsage::default(),
        termination: termination.into(),
    };
    execution.validate()?;
    let receipt = EffectReceipt {
        operation: receipt_context.operation().clone(),
        request_id: identity.request_id().clone(),
        actor: identity.actor().clone(),
        scope: identity.scope().clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: identity.idempotency_key().clone(),
        input_digest: identity.input_digest().clone(),
        expected_state: receipt_context.expected_state().clone(),
        policy_digest: receipt_context.authority().policy.digest.clone(),
        configuration_digest: receipt_context.configuration_digest().clone(),
        catalog_digest: receipt_context.catalog_digest().clone(),
        privacy_digest: receipt_context.privacy_digest().clone(),
        outcome: termination,
        committed_state,
        external_proof: None,
    };
    Ok(ApplicationOutcome::Effect(EffectResult::new(
        receipt_context.effect_id().clone(),
        EffectClass::Administrative,
        identity.idempotency_key().clone(),
        receipt_context.authority().clone(),
        receipt_context.expected_state().clone(),
        execution,
        ReconciliationState::Reconciled,
        receipt,
        result,
    )?))
}

fn workflow_effect_terminal_observation(
    termination: EffectTermination,
    observed_at: UtcMicros,
) -> Option<CancellationObservation> {
    (termination == EffectTermination::TimedOut).then_some(CancellationObservation {
        stage: CancellationStage::BeforeEffect,
        observed_at,
    })
}

#[cfg(test)]
mod workflow_effect_receipt_tests {
    use tracedecay_application::{CancellationObservation, CancellationStage, EffectTermination};
    use tracedecay_domain::UtcMicros;

    use super::workflow_effect_terminal_observation;

    #[test]
    fn deadline_before_mutation_is_not_labeled_in_flight() {
        let observed_at = UtcMicros(42);
        assert_eq!(
            workflow_effect_terminal_observation(EffectTermination::TimedOut, observed_at),
            Some(CancellationObservation {
                stage: CancellationStage::BeforeEffect,
                observed_at,
            })
        );
        assert_eq!(
            workflow_effect_terminal_observation(EffectTermination::Completed, observed_at),
            None
        );
    }
}
