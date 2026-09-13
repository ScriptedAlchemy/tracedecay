//! Workflow-run admission, state transitions, and fan-out reconciliation.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tracedecay_application::work::workflow_topology::WorkflowTopologyError;
use tracedecay_contracts::{
    ApplicationProblem, LegalAction, RequestContext, RetryDirective, SafeDiagnostic,
    WorkflowCatalogAdmissionError, WorkflowCoordinationError, WorkflowRunStoragePort,
};
use tracedecay_domain::{ManifestDigest, UtcMicros};

use tracedecay_daemon_protocol::DaemonInvocationProblem;

use super::super::work_attempt_exec::WorkAttemptProcessRegistryV1;
use super::RegisteredWorkRuntime;
use super::workflow_fan_out::reconcile_workflow_fan_out;

/// Admits a workflow run from an Active definition version.
///
/// Every admission digest is derived by the daemon from its registered
/// environment: live policy/configuration digests, the shipped workflow
/// catalog digest, the pinned work topology policy digest, and the digest of
/// the provider registry built from the request's registration. A definition
/// pinned against a different environment is a typed staleness denial, and a
/// registry that cannot place the definition's entry step denies admission
/// before any event is journaled.
pub(super) fn start_workflow_run(
    registered: &RegisteredWorkRuntime,
    services: &tracedecay_application::work::RegisteredWorkflowApplicationServicesV1,
    context: &RequestContext,
    request: tracedecay_contracts::WorkflowRunStartRequest,
    input_digest: &ManifestDigest,
    observed_at: UtcMicros,
    attempt_processes: Arc<WorkAttemptProcessRegistryV1>,
    project_root: &Path,
    observability_producer: Option<
        Arc<tracedecay_application::observability::BoundedObservabilityProducerV1>,
    >,
) -> Result<tracedecay_domain::WorkflowRunProjection, DaemonInvocationProblem> {
    match services.effects().projection(&request.run_id) {
        Ok(existing) => {
            let admitted = existing
                .history()
                .first()
                .ok_or(DaemonInvocationProblem::ResetRequired)?;
            if admitted.command_id() != &request.command_id
                || admitted.input_digest() != input_digest
            {
                return Err(DaemonInvocationProblem::InvalidRequest);
            }
            return reconcile_workflow_fan_out(
                registered,
                services,
                context,
                existing,
                observed_at,
                attempt_processes,
                project_root,
                observability_producer,
            );
        }
        Err(tracedecay_contracts::WorkflowRunStorageError::NotFound) => {}
        Err(error) => return Err(workflow_run_storage_problem(error)),
    }
    let definition = services
        .definitions()
        .get(&request.definition_id, request.definition_version)
        .map_err(workflow_coordination_problem)?;
    if definition.project_id() != &context.scope().project_id {
        return Err(DaemonInvocationProblem::NotFoundOrNotAuthorized);
    }
    let disposition = services
        .definitions()
        .disposition(&request.definition_id, request.definition_version)
        .map_err(workflow_coordination_problem)?;
    if disposition.state != tracedecay_contracts::WorkflowDefinitionLifecycleState::Active {
        return Err(DaemonInvocationProblem::InvalidRequest);
    }
    let provider_registration = request.provider.clone();
    let registry = tracedecay_contracts::WorkflowProviderRegistry::new(
        registered.configuration_digest.clone(),
        vec![request.provider],
    )
    .map_err(workflow_placement_problem)?;
    let topology_cancellation = Arc::new(AtomicBool::new(context.cancellation().is_cancelled()));
    let workflow_topology = services
        .topology()
        .verified_snapshot(
            &request.definition_id,
            request.definition_version,
            topology_cancellation,
        )
        .map_err(workflow_topology_problem)?;
    let ready_steps = workflow_topology
        .ready_steps(
            &BTreeSet::new(),
            definition.steps().len(),
            Arc::new(tracedecay_graph_db::NeverCancelled),
        )
        .map_err(workflow_topology_problem)?;
    if ready_steps.is_empty() {
        return Err(DaemonInvocationProblem::ResetRequired);
    }
    let topology = &registered.work_topology_policy;
    let topology_digest = topology
        .compute_digest()
        .map_err(|_| DaemonInvocationProblem::Unavailable)?
        .0;
    let placement = tracedecay_contracts::WorkflowProviderPlacementService::new(registry.clone());
    for step_id in &ready_steps {
        placement
            .place(
                &tracedecay_contracts::WorkflowTopologyPlacementRequest {
                    run_id: request.run_id.clone(),
                    step_id: step_id.clone(),
                    configuration_digest: registered.configuration_digest.clone(),
                    topology_digest: topology_digest.clone(),
                },
                topology,
            )
            .map_err(workflow_placement_problem)?;
    }
    let admission = tracedecay_contracts::WorkflowAdmissionSnapshot {
        policy_digest: registered.policy_digest.clone(),
        configuration_digest: registered.configuration_digest.clone(),
        catalog_digest: tracedecay_contracts::work_executable_catalog_digest()
            .map_err(|_| DaemonInvocationProblem::Unavailable)?,
        topology_digest: topology_digest.clone(),
        provider_registry_digest: registry.digest().clone(),
    };
    let fan_out_plans = match request.fan_out {
        None => Vec::new(),
        Some(fan_out) => {
            if fan_out.execution_snapshot.route() != provider_registration.route()
                || fan_out.execution_snapshot.backend() != provider_registration.backend()
                || fan_out.execution_snapshot.model() != provider_registration.model()
            {
                return Err(DaemonInvocationProblem::InvalidRequest);
            }
            tracedecay_contracts::require_registered_work_topology(
                &fan_out.execution_snapshot,
                topology,
            )
            .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
            let mut fan_out_steps = ready_steps.iter().filter(|step_id| {
                definition
                    .steps()
                    .iter()
                    .find(|step| &step.step_id == *step_id)
                    .is_some_and(|step| step.fan_out.is_some())
            });
            let entry_step = fan_out_steps
                .next()
                .cloned()
                .ok_or(DaemonInvocationProblem::InvalidRequest)?;
            if fan_out_steps.next().is_some() {
                return Err(DaemonInvocationProblem::InvalidRequest);
            }
            let provider = tracedecay_contracts::WorkflowProviderAdmission {
                execution_snapshot: fan_out.execution_snapshot,
                topology_digest: topology_digest.clone(),
                provider_registry_digest: registry.digest().clone(),
                worktree_placement: topology.placement.clone(),
                reference: fan_out.reference,
                commit: fan_out.commit,
                cancellation_generation: 1,
                effect_state: fan_out.effect_state,
            };
            let plan = tracedecay_contracts::prepare_workflow_fan_out(
                &tracedecay_contracts::WorkflowFanOutRequest {
                    definition: definition.clone(),
                    run_id: request.run_id.clone(),
                    step_id: entry_step,
                    fence: fan_out.fence,
                    admitted_at: observed_at,
                    cancellation: context.cancellation().clone(),
                    max_parallel: fan_out.max_parallel,
                    failure_policy: fan_out.failure_policy,
                    provider: provider.clone(),
                    inputs: fan_out.inputs,
                },
            )
            .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
            vec![
                tracedecay_contracts::durable_workflow_fan_out_plan(
                    &plan,
                    &provider,
                    tracedecay_domain::WorkAuthority::new(
                        context.scope().project_id.clone(),
                        context.scope().repository_id.clone(),
                        context.scope().worktree_id.clone(),
                        context.actor().clone(),
                        context.grant().digest.clone(),
                    )
                    .map_err(|_| DaemonInvocationProblem::NotFoundOrNotAuthorized)?,
                )
                .map_err(|_| DaemonInvocationProblem::InvalidRequest)?,
            ]
        }
    };
    let projection = tracedecay_contracts::WorkflowRunService::new(services.effects().clone())
        .admit_with_fan_out(
            request.run_id,
            definition,
            admission,
            fan_out_plans,
            tracedecay_domain::WorkflowRunEventContext {
                command_id: request.command_id,
                input_digest: input_digest.clone(),
                occurred_at: observed_at,
            },
        )
        .map_err(workflow_run_problem)?;
    reconcile_workflow_fan_out(
        registered,
        services,
        context,
        projection,
        observed_at,
        attempt_processes,
        project_root,
        observability_producer,
    )
}

pub(super) fn apply_workflow_run_command(
    services: &tracedecay_application::work::RegisteredWorkflowApplicationServicesV1,
    run_id: &tracedecay_domain::RunId,
    expected_sequence: u64,
    command: tracedecay_domain::WorkflowRunCommand,
    command_id: tracedecay_domain::WorkCommandId,
    input_digest: &ManifestDigest,
    observed_at: UtcMicros,
) -> Result<tracedecay_domain::WorkflowRunProjection, DaemonInvocationProblem> {
    tracedecay_contracts::WorkflowRunService::new(services.effects().clone())
        .apply(
            run_id,
            expected_sequence,
            command,
            tracedecay_domain::WorkflowRunEventContext {
                command_id,
                input_digest: input_digest.clone(),
                occurred_at: observed_at,
            },
        )
        .map_err(workflow_run_problem)
}

/// Requests cooperative cancellation and, when no step is still running,
/// immediately reconciles the run to its terminal `Cancelled` state under a
/// command identity derived from the caller's, so replays settle identically.
pub(super) fn cancel_workflow_run(
    registered: &RegisteredWorkRuntime,
    services: &tracedecay_application::work::RegisteredWorkflowApplicationServicesV1,
    context: &RequestContext,
    request: tracedecay_contracts::WorkflowRunCancelRequest,
    input_digest: &ManifestDigest,
    observed_at: UtcMicros,
    attempt_processes: Arc<WorkAttemptProcessRegistryV1>,
    project_root: &Path,
    observability_producer: Option<
        Arc<tracedecay_application::observability::BoundedObservabilityProducerV1>,
    >,
) -> Result<tracedecay_domain::WorkflowRunProjection, DaemonInvocationProblem> {
    let reconcile_command_id = tracedecay_domain::WorkCommandId::try_from(format!(
        "{}.reconcile",
        request.command_id.as_str()
    ))
    .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
    let cancelling = apply_workflow_run_command(
        services,
        &request.run_id,
        request.expected_sequence,
        tracedecay_domain::WorkflowRunCommand::RequestCancellation,
        request.command_id,
        input_digest,
        observed_at,
    )?;
    if !cancelling.fan_out_plans().is_empty() {
        return reconcile_workflow_fan_out(
            registered,
            services,
            context,
            cancelling,
            observed_at,
            attempt_processes,
            project_root,
            observability_producer,
        );
    }
    let any_step_running = cancelling.definition().steps().iter().any(|step| {
        cancelling.step(&step.step_id).is_some_and(|projected| {
            projected.status() == tracedecay_domain::WorkflowStepStatus::Running
        })
    });
    if any_step_running {
        return Ok(cancelling);
    }
    apply_workflow_run_command(
        services,
        &request.run_id,
        cancelling.sequence(),
        tracedecay_domain::WorkflowRunCommand::ReconcileCancelled,
        reconcile_command_id,
        input_digest,
        observed_at,
    )
}

pub(super) fn workflow_run_problem(
    error: tracedecay_contracts::WorkflowRunServiceError,
) -> DaemonInvocationProblem {
    match error {
        tracedecay_contracts::WorkflowRunServiceError::PolicyDigestMismatch
        | tracedecay_contracts::WorkflowRunServiceError::ConfigurationDigestMismatch
        | tracedecay_contracts::WorkflowRunServiceError::CatalogDigestMismatch
        | tracedecay_contracts::WorkflowRunServiceError::State(_) => {
            DaemonInvocationProblem::InvalidRequest
        }
        tracedecay_contracts::WorkflowRunServiceError::Storage(error) => {
            workflow_run_storage_problem(error)
        }
    }
}

pub(super) fn workflow_coordination_problem(
    error: WorkflowCoordinationError,
) -> DaemonInvocationProblem {
    match error {
        tracedecay_contracts::WorkflowCoordinationError::AuthorityUnavailable(_) => {
            DaemonInvocationProblem::Unavailable
        }
        // A catalog that could not be composed is an unavailable authority,
        // not a caller mistake; only a definition the live catalog actually
        // refused is an invalid request.
        tracedecay_contracts::WorkflowCoordinationError::CatalogAdmissionDenied(
            tracedecay_contracts::WorkflowCatalogAdmissionError::CatalogUnavailable(_),
        ) => DaemonInvocationProblem::Unavailable,
        tracedecay_contracts::WorkflowCoordinationError::DefinitionNotFound
        | tracedecay_contracts::WorkflowCoordinationError::ScopeMismatch => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        tracedecay_contracts::WorkflowCoordinationError::InvalidDefinition
        | tracedecay_contracts::WorkflowCoordinationError::CatalogAdmissionDenied(_)
        | tracedecay_contracts::WorkflowCoordinationError::ImmutableDefinitionConflict
        | tracedecay_contracts::WorkflowCoordinationError::IllegalLifecycleTransition
        | tracedecay_contracts::WorkflowCoordinationError::LifecycleRevisionConflict => {
            DaemonInvocationProblem::InvalidRequest
        }
    }
}

/// Admits one definition's environment pins against the live registered
/// daemon environment.
///
/// Run admission compares the pinned policy and configuration digests against
/// the registered environment, so a definition activated against different
/// pins can never start. Validation and activation therefore admit the same
/// pins and name the live digest, exactly as the catalog pin does: a typed
/// denial is the only channel through which a caller can learn the digests
/// the daemon derives from its own project-open snapshot.
pub(super) fn admit_workflow_environment_pins(
    registered: &RegisteredWorkRuntime,
    definition: &tracedecay_domain::WorkflowDefinition,
) -> Result<(), SafeDiagnostic> {
    let mismatch = |pin: &str, expected: &ManifestDigest, observed: &ManifestDigest| SafeDiagnostic {
        code: format!("workflow.{pin}.pin_mismatch"),
        message: format!(
            "pinned_{pin}_digest expected {expected}, observed {observed}; register a new immutable definition version with the live registered {pin} digest"
        ),
    };
    if definition.pinned_policy_digest() != &registered.policy_digest {
        return Err(mismatch(
            "policy",
            &registered.policy_digest,
            definition.pinned_policy_digest(),
        ));
    }
    if definition.pinned_configuration_digest() != &registered.configuration_digest {
        return Err(mismatch(
            "configuration",
            &registered.configuration_digest,
            definition.pinned_configuration_digest(),
        ));
    }
    Ok(())
}

pub(super) fn workflow_coordination_application_problem(
    error: &WorkflowCoordinationError,
) -> Option<ApplicationProblem> {
    let diagnostic = match error {
        WorkflowCoordinationError::InvalidDefinition => SafeDiagnostic {
            code: "workflow.definition.invalid".to_owned(),
            message: "definition failed structural validation".to_owned(),
        },
        WorkflowCoordinationError::CatalogAdmissionDenied(
            WorkflowCatalogAdmissionError::CatalogPinMismatch { pinned, current },
        ) => SafeDiagnostic {
            code: "workflow.catalog.pin_mismatch".to_owned(),
            message: format!(
                "pinned_catalog_digest expected {current}, observed {pinned}; register a new immutable definition version with the live Work executable catalog digest"
            ),
        },
        WorkflowCoordinationError::CatalogAdmissionDenied(
            WorkflowCatalogAdmissionError::UnknownOperation { step_id, operation },
        ) => SafeDiagnostic {
            code: "workflow.catalog.operation_unknown".to_owned(),
            message: format!(
                "steps[{step_id}].operation observed {operation}; expected an operation in the live Work executable catalog"
            ),
        },
        WorkflowCoordinationError::CatalogAdmissionDenied(
            WorkflowCatalogAdmissionError::OperationUnavailable { step_id, operation },
        ) => SafeDiagnostic {
            code: "workflow.catalog.operation_unavailable".to_owned(),
            message: format!(
                "steps[{step_id}].operation observed {operation}; expected a live executable binding"
            ),
        },
        WorkflowCoordinationError::ImmutableDefinitionConflict => SafeDiagnostic {
            code: "workflow.definition.immutable_conflict".to_owned(),
            message:
                "definition_id and definition_version already identify different immutable content"
                    .to_owned(),
        },
        WorkflowCoordinationError::IllegalLifecycleTransition => SafeDiagnostic {
            code: "workflow.lifecycle.illegal_transition".to_owned(),
            message: "lifecycle operation is not legal from the observed definition state"
                .to_owned(),
        },
        WorkflowCoordinationError::LifecycleRevisionConflict => SafeDiagnostic {
            code: "workflow.lifecycle.revision_conflict".to_owned(),
            message:
                "expected_revision does not match the observed definition disposition revision"
                    .to_owned(),
        },
        WorkflowCoordinationError::CatalogAdmissionDenied(
            WorkflowCatalogAdmissionError::CatalogUnavailable(_),
        )
        | WorkflowCoordinationError::ScopeMismatch
        | WorkflowCoordinationError::DefinitionNotFound
        | WorkflowCoordinationError::AuthorityUnavailable(_) => return None,
    };
    Some(ApplicationProblem::InvalidRequest {
        diagnostic,
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    })
}

pub(super) fn workflow_run_storage_problem(
    error: tracedecay_contracts::WorkflowRunStorageError,
) -> DaemonInvocationProblem {
    match error {
        tracedecay_contracts::WorkflowRunStorageError::NotFound => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        tracedecay_contracts::WorkflowRunStorageError::VersionConflict
        | tracedecay_contracts::WorkflowRunStorageError::IdempotencyConflict => {
            DaemonInvocationProblem::InvalidRequest
        }
        tracedecay_contracts::WorkflowRunStorageError::InvalidHistory => {
            DaemonInvocationProblem::ResetRequired
        }
        tracedecay_contracts::WorkflowRunStorageError::Unavailable => {
            DaemonInvocationProblem::Unavailable
        }
    }
}

fn workflow_placement_problem(
    error: tracedecay_contracts::WorkflowProviderPlacementError,
) -> DaemonInvocationProblem {
    match error {
        tracedecay_contracts::WorkflowProviderPlacementError::InvalidRegistry
        | tracedecay_contracts::WorkflowProviderPlacementError::ConfigurationDigestMismatch
        | tracedecay_contracts::WorkflowProviderPlacementError::TopologyDigestMismatch
        | tracedecay_contracts::WorkflowProviderPlacementError::InvalidTopology => {
            DaemonInvocationProblem::InvalidRequest
        }
        tracedecay_contracts::WorkflowProviderPlacementError::Unavailable => {
            DaemonInvocationProblem::Unavailable
        }
    }
}

fn workflow_topology_problem(error: WorkflowTopologyError) -> DaemonInvocationProblem {
    match error {
        WorkflowTopologyError::Contract(_) => DaemonInvocationProblem::InvalidRequest,
        WorkflowTopologyError::GenerationMismatch | WorkflowTopologyError::Corrupt(_) => {
            DaemonInvocationProblem::ResetRequired
        }
        WorkflowTopologyError::Cancelled
        | WorkflowTopologyError::BudgetExhausted
        | WorkflowTopologyError::Unavailable(_) => DaemonInvocationProblem::Unavailable,
    }
}
