//! Workflow-run admission, state transitions, and fan-out reconciliation.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tracedecay_application::work::workflow_topology::WorkflowTopologyError;
use tracedecay_contracts::{
    ApplicationProblem, ApplicationProblemDetailV1, LegalAction, RequestContext, RetryDirective,
    SafeDiagnostic, WorkflowCatalogAdmissionError, WorkflowCoordinationError,
    WorkflowDefinitionLifecycleState, WorkflowRunServiceError, WorkflowRunStorageError,
    WorkflowRunStoragePort,
};
use tracedecay_domain::{ManifestDigest, UtcMicros, WorkflowRunStateError};

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
) -> Result<tracedecay_domain::WorkflowRunProjection, ApplicationProblem> {
    match services.effects().projection(&request.run_id) {
        Ok(existing) => {
            let admitted = existing
                .history()
                .first()
                .ok_or_else(workflow_reset_required)?;
            if admitted.command_id() != &request.command_id
                || admitted.input_digest() != input_digest
            {
                return Err(workflow_run_command_conflict(
                    "run_id already names a run admitted by a different command_id or input",
                ));
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
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let disposition = services
        .definitions()
        .disposition(&request.definition_id, request.definition_version)
        .map_err(workflow_coordination_problem)?;
    if disposition.state != WorkflowDefinitionLifecycleState::Active {
        return Err(definition_not_active(
            request.definition_version,
            disposition.state,
        ));
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
        return Err(workflow_reset_required());
    }
    let topology = &registered.work_topology_policy;
    let topology_digest = topology
        .compute_digest()
        .map_err(|_| workflow_runtime_unavailable())?
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
            .map_err(|_| workflow_runtime_unavailable())?,
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
                return Err(workflow_invalid_request());
            }
            tracedecay_contracts::require_registered_work_topology(
                &fan_out.execution_snapshot,
                topology,
            )
            .map_err(|_| workflow_invalid_request())?;
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
                .ok_or_else(workflow_invalid_request)?;
            if fan_out_steps.next().is_some() {
                return Err(workflow_invalid_request());
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
            .map_err(|_| workflow_invalid_request())?;
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
                    .map_err(|_| {
                        ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
                    })?,
                )
                .map_err(|_| workflow_invalid_request())?,
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
) -> Result<tracedecay_domain::WorkflowRunProjection, ApplicationProblem> {
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
        .map_err(|error| match error {
            WorkflowRunServiceError::Storage(WorkflowRunStorageError::VersionConflict) => {
                match services.effects().projection(run_id) {
                    Ok(current) => ApplicationProblem::from_detail(
                        ApplicationProblemDetailV1::StalePrecondition {
                            field: "expected_sequence".to_owned(),
                            requested: expected_sequence,
                            current: current.sequence(),
                        },
                    ),
                    Err(error) => workflow_run_storage_problem(error),
                }
            }
            error => workflow_run_problem(error),
        })
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
) -> Result<tracedecay_domain::WorkflowRunProjection, ApplicationProblem> {
    let reconcile_command_id = tracedecay_domain::WorkCommandId::try_from(format!(
        "{}.reconcile",
        request.command_id.as_str()
    ))
    .map_err(|_| workflow_invalid_request())?;
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

pub(super) fn workflow_runtime_unavailable() -> ApplicationProblem {
    ApplicationProblem::unavailable(SafeDiagnostic {
        code: "workflow.unavailable".to_owned(),
        message: "The Workflow application runtime is unavailable".to_owned(),
    })
}

pub(super) fn workflow_invalid_request() -> ApplicationProblem {
    ApplicationProblem::invalid_request(
        "workflow.invalid_request",
        "The Workflow application request is invalid",
    )
}

pub(super) fn workflow_not_found() -> ApplicationProblem {
    ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
}

pub(super) fn workflow_reset_required() -> ApplicationProblem {
    ApplicationProblem::reset_required(SafeDiagnostic {
        code: "workflow.reset_required".to_owned(),
        message: "The Workflow store requires an explicit reset".to_owned(),
    })
}

/// A shared-authority problem outside the Workflow owner, answered with the
/// Workflow family's codes.
pub(super) fn workflow_invocation_problem(problem: DaemonInvocationProblem) -> ApplicationProblem {
    match problem {
        DaemonInvocationProblem::InvalidRequest | DaemonInvocationProblem::UnsupportedRevision => {
            workflow_invalid_request()
        }
        DaemonInvocationProblem::NotFoundOrNotAuthorized => workflow_not_found(),
        DaemonInvocationProblem::ResetRequired => workflow_reset_required(),
        DaemonInvocationProblem::ApplicationContractViolation => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "workflow.application_contract_violation".to_owned(),
                message: "The Workflow application result violated its canonical contract"
                    .to_owned(),
            })
        }
        DaemonInvocationProblem::Unavailable => workflow_runtime_unavailable(),
    }
}

/// A conflict that resending the same request can never clear; `action` is
/// the one way forward.
pub(super) fn workflow_final_conflict(
    code: &str,
    message: impl Into<String>,
    action: LegalAction,
) -> ApplicationProblem {
    ApplicationProblem::Conflict {
        diagnostic: SafeDiagnostic {
            code: code.to_owned(),
            message: message.into(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![action],
    }
}

/// The same identity already names different input.
fn workflow_run_command_conflict(message: &str) -> ApplicationProblem {
    workflow_final_conflict(
        "workflow.run.command_conflict",
        message,
        LegalAction::CorrectRequest,
    )
}

fn definition_not_active(
    definition_version: u64,
    state: WorkflowDefinitionLifecycleState,
) -> ApplicationProblem {
    workflow_final_conflict(
        "workflow.definition.not_active",
        format!(
            "definition_version {definition_version} is {state}; runs start only from an active definition version",
            state = state.as_str()
        ),
        LegalAction::CorrectRequest,
    )
}

pub(super) fn workflow_run_problem(error: WorkflowRunServiceError) -> ApplicationProblem {
    let pin_mismatch = |pin: &str| {
        ApplicationProblem::invalid_request(
            format!("workflow.{pin}.pin_mismatch"),
            format!(
                "the definition's pinned_{pin}_digest no longer matches the live registered {pin} digest; register a new immutable definition version with the live digest"
            ),
        )
    };
    match error {
        WorkflowRunServiceError::PolicyDigestMismatch => pin_mismatch("policy"),
        WorkflowRunServiceError::ConfigurationDigestMismatch => pin_mismatch("configuration"),
        WorkflowRunServiceError::CatalogDigestMismatch => pin_mismatch("catalog"),
        WorkflowRunServiceError::State(WorkflowRunStateError::InvalidTransition) => {
            ApplicationProblem::conflict(
                "workflow.run.illegal_transition",
                "the run's current status has no transition for this command; read the run and resend against its current status",
            )
        }
        WorkflowRunServiceError::State(WorkflowRunStateError::DuplicateCommand) => {
            workflow_run_command_conflict("command_id already names a different run command")
        }
        WorkflowRunServiceError::State(error) => {
            ApplicationProblem::invalid_request("workflow.run.state_refused", error.to_string())
        }
        WorkflowRunServiceError::Storage(error) => workflow_run_storage_problem(error),
    }
}

pub(super) fn workflow_coordination_problem(
    error: WorkflowCoordinationError,
) -> ApplicationProblem {
    let invalid = |code: &str, message: String| ApplicationProblem::invalid_request(code, message);
    match error {
        // A catalog that could not be composed is an unavailable authority,
        // not a caller mistake; only a definition the live catalog actually
        // refused is an invalid request.
        WorkflowCoordinationError::AuthorityUnavailable(_)
        | WorkflowCoordinationError::CatalogAdmissionDenied(
            WorkflowCatalogAdmissionError::CatalogUnavailable(_),
        ) => workflow_runtime_unavailable(),
        WorkflowCoordinationError::DefinitionNotFound | WorkflowCoordinationError::ScopeMismatch => {
            workflow_not_found()
        }
        WorkflowCoordinationError::InvalidDefinition => invalid(
            "workflow.definition.invalid",
            "definition content, definition_version, or expected_revision failed validation; versions and revisions start at 1".to_owned(),
        ),
        WorkflowCoordinationError::CatalogAdmissionDenied(
            WorkflowCatalogAdmissionError::CatalogPinMismatch { pinned, current },
        ) => invalid(
            "workflow.catalog.pin_mismatch",
            format!(
                "pinned_catalog_digest expected {current}, observed {pinned}; register a new immutable definition version with the live Work executable catalog digest"
            ),
        ),
        WorkflowCoordinationError::CatalogAdmissionDenied(
            WorkflowCatalogAdmissionError::UnknownOperation { step_id, operation },
        ) => invalid(
            "workflow.catalog.operation_unknown",
            format!(
                "steps[{step_id}].operation observed {operation}; expected an operation in the live Work executable catalog"
            ),
        ),
        WorkflowCoordinationError::CatalogAdmissionDenied(
            WorkflowCatalogAdmissionError::OperationUnavailable { step_id, operation },
        ) => invalid(
            "workflow.catalog.operation_unavailable",
            format!(
                "steps[{step_id}].operation observed {operation}; expected a live executable binding"
            ),
        ),
        WorkflowCoordinationError::ImmutableDefinitionConflict => workflow_final_conflict(
            "workflow.definition.immutable_conflict",
            "definition_id and definition_version already identify different immutable content; register a new definition_version",
            LegalAction::CorrectRequest,
        ),
        WorkflowCoordinationError::IllegalLifecycleTransition => ApplicationProblem::conflict(
            "workflow.lifecycle.illegal_transition",
            "lifecycle operation is not legal from the observed definition state",
        ),
        WorkflowCoordinationError::LifecycleRevisionConflict => {
            ApplicationProblem::stale(SafeDiagnostic {
                code: "workflow.lifecycle.revision_conflict".to_owned(),
                message:
                    "expected_revision does not match the observed definition disposition revision"
                        .to_owned(),
            })
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
    let mismatch = |pin: &str, expected: &ManifestDigest, observed: &ManifestDigest| {
        SafeDiagnostic {
            code: format!("workflow.{pin}.pin_mismatch"),
            message: format!(
                "pinned_{pin}_digest expected {expected}, observed {observed}; register a new immutable definition version with the live registered {pin} digest"
            ),
        }
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

pub(super) fn workflow_run_storage_problem(error: WorkflowRunStorageError) -> ApplicationProblem {
    match error {
        WorkflowRunStorageError::NotFound => workflow_not_found(),
        WorkflowRunStorageError::VersionConflict => ApplicationProblem::stale(SafeDiagnostic {
            code: "workflow.run.sequence_conflict".to_owned(),
            message: "expected_sequence does not match the run's current sequence".to_owned(),
        }),
        WorkflowRunStorageError::IdempotencyConflict => {
            workflow_run_command_conflict("command_id already names a different run command")
        }
        WorkflowRunStorageError::InvalidHistory => workflow_reset_required(),
        WorkflowRunStorageError::Unavailable => workflow_runtime_unavailable(),
    }
}

fn workflow_placement_problem(
    error: tracedecay_contracts::WorkflowProviderPlacementError,
) -> ApplicationProblem {
    match error {
        tracedecay_contracts::WorkflowProviderPlacementError::InvalidRegistry
        | tracedecay_contracts::WorkflowProviderPlacementError::ConfigurationDigestMismatch
        | tracedecay_contracts::WorkflowProviderPlacementError::TopologyDigestMismatch
        | tracedecay_contracts::WorkflowProviderPlacementError::InvalidTopology => {
            workflow_invalid_request()
        }
        tracedecay_contracts::WorkflowProviderPlacementError::Unavailable => {
            workflow_runtime_unavailable()
        }
    }
}

fn workflow_topology_problem(error: WorkflowTopologyError) -> ApplicationProblem {
    match error {
        WorkflowTopologyError::Contract(_) => workflow_invalid_request(),
        WorkflowTopologyError::GenerationMismatch | WorkflowTopologyError::Corrupt(_) => {
            workflow_reset_required()
        }
        WorkflowTopologyError::Cancelled
        | WorkflowTopologyError::BudgetExhausted
        | WorkflowTopologyError::Unavailable(_) => workflow_runtime_unavailable(),
    }
}
