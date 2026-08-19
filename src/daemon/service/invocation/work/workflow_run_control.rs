//! Workflow-run admission, state transitions, and fan-out reconciliation.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tracedecay_application::{RequestContext, WorkflowRunStoragePort};
use tracedecay_domain::{ManifestDigest, UtcMicros};

use crate::daemon_contract::DaemonInvocationProblem;

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
    services: &crate::global_db::RegisteredWorkflowApplicationServicesV1,
    context: &RequestContext,
    request: tracedecay_application::WorkflowRunStartRequest,
    input_digest: &ManifestDigest,
    observed_at: UtcMicros,
    attempt_processes: Arc<WorkAttemptProcessRegistryV1>,
    project_root: &Path,
    observability_producer: Option<
        Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
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
        Err(tracedecay_application::WorkflowRunStorageError::NotFound) => {}
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
    if disposition.state != tracedecay_application::WorkflowDefinitionLifecycleState::Active {
        return Err(DaemonInvocationProblem::InvalidRequest);
    }
    let provider_registration = request.provider.clone();
    let registry = tracedecay_application::WorkflowProviderRegistry::new(
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
    let placement = tracedecay_application::WorkflowProviderPlacementService::new(registry.clone());
    for step_id in &ready_steps {
        placement
            .place(
                &tracedecay_application::WorkflowTopologyPlacementRequest {
                    run_id: request.run_id.clone(),
                    step_id: step_id.clone(),
                    configuration_digest: registered.configuration_digest.clone(),
                    topology_digest: topology_digest.clone(),
                },
                topology,
            )
            .map_err(workflow_placement_problem)?;
    }
    let admission = tracedecay_application::WorkflowAdmissionSnapshot {
        policy_digest: registered.policy_digest.clone(),
        configuration_digest: registered.configuration_digest.clone(),
        catalog_digest: tracedecay_application::work_executable_catalog_digest()
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
            tracedecay_application::require_registered_work_topology(
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
            let provider = tracedecay_application::WorkflowProviderAdmission {
                execution_snapshot: fan_out.execution_snapshot,
                topology_digest: topology_digest.clone(),
                provider_registry_digest: registry.digest().clone(),
                worktree_placement: topology.placement.clone(),
                reference: fan_out.reference,
                commit: fan_out.commit,
                cancellation_generation: 1,
                effect_state: fan_out.effect_state,
            };
            let plan = tracedecay_application::prepare_workflow_fan_out(
                &tracedecay_application::WorkflowFanOutRequest {
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
                tracedecay_application::durable_workflow_fan_out_plan(
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
    let projection = tracedecay_application::WorkflowRunService::new(services.effects().clone())
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
    services: &crate::global_db::RegisteredWorkflowApplicationServicesV1,
    run_id: &tracedecay_domain::RunId,
    expected_sequence: u64,
    command: tracedecay_domain::WorkflowRunCommand,
    command_id: tracedecay_domain::WorkCommandId,
    input_digest: &ManifestDigest,
    observed_at: UtcMicros,
) -> Result<tracedecay_domain::WorkflowRunProjection, DaemonInvocationProblem> {
    tracedecay_application::WorkflowRunService::new(services.effects().clone())
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
    services: &crate::global_db::RegisteredWorkflowApplicationServicesV1,
    context: &RequestContext,
    request: tracedecay_application::WorkflowRunCancelRequest,
    input_digest: &ManifestDigest,
    observed_at: UtcMicros,
    attempt_processes: Arc<WorkAttemptProcessRegistryV1>,
    project_root: &Path,
    observability_producer: Option<
        Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
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
    error: tracedecay_application::WorkflowRunServiceError,
) -> DaemonInvocationProblem {
    match error {
        tracedecay_application::WorkflowRunServiceError::PolicyDigestMismatch
        | tracedecay_application::WorkflowRunServiceError::ConfigurationDigestMismatch
        | tracedecay_application::WorkflowRunServiceError::CatalogDigestMismatch
        | tracedecay_application::WorkflowRunServiceError::State(_) => {
            DaemonInvocationProblem::InvalidRequest
        }
        tracedecay_application::WorkflowRunServiceError::Storage(error) => {
            workflow_run_storage_problem(error)
        }
    }
}

pub(super) fn workflow_coordination_problem(
    error: tracedecay_application::WorkflowCoordinationError,
) -> DaemonInvocationProblem {
    match error {
        tracedecay_application::WorkflowCoordinationError::AuthorityUnavailable(_) => {
            DaemonInvocationProblem::Unavailable
        }
        // A catalog that could not be composed is an unavailable authority,
        // not a caller mistake; only a definition the live catalog actually
        // refused is an invalid request.
        tracedecay_application::WorkflowCoordinationError::CatalogAdmissionDenied(
            tracedecay_application::WorkflowCatalogAdmissionError::CatalogUnavailable(_),
        ) => DaemonInvocationProblem::Unavailable,
        tracedecay_application::WorkflowCoordinationError::DefinitionNotFound
        | tracedecay_application::WorkflowCoordinationError::ScopeMismatch => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        tracedecay_application::WorkflowCoordinationError::InvalidDefinition
        | tracedecay_application::WorkflowCoordinationError::CatalogAdmissionDenied(_)
        | tracedecay_application::WorkflowCoordinationError::ImmutableDefinitionConflict
        | tracedecay_application::WorkflowCoordinationError::IllegalLifecycleTransition
        | tracedecay_application::WorkflowCoordinationError::LifecycleRevisionConflict => {
            DaemonInvocationProblem::InvalidRequest
        }
    }
}

pub(super) fn workflow_run_storage_problem(
    error: tracedecay_application::WorkflowRunStorageError,
) -> DaemonInvocationProblem {
    match error {
        tracedecay_application::WorkflowRunStorageError::NotFound => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        tracedecay_application::WorkflowRunStorageError::VersionConflict
        | tracedecay_application::WorkflowRunStorageError::IdempotencyConflict => {
            DaemonInvocationProblem::InvalidRequest
        }
        tracedecay_application::WorkflowRunStorageError::InvalidHistory => {
            DaemonInvocationProblem::ResetRequired
        }
        tracedecay_application::WorkflowRunStorageError::Unavailable => {
            DaemonInvocationProblem::Unavailable
        }
    }
}

fn workflow_placement_problem(
    error: tracedecay_application::WorkflowProviderPlacementError,
) -> DaemonInvocationProblem {
    match error {
        tracedecay_application::WorkflowProviderPlacementError::InvalidRegistry
        | tracedecay_application::WorkflowProviderPlacementError::ConfigurationDigestMismatch
        | tracedecay_application::WorkflowProviderPlacementError::TopologyDigestMismatch
        | tracedecay_application::WorkflowProviderPlacementError::InvalidTopology => {
            DaemonInvocationProblem::InvalidRequest
        }
        tracedecay_application::WorkflowProviderPlacementError::Unavailable => {
            DaemonInvocationProblem::Unavailable
        }
    }
}

fn workflow_topology_problem(
    error: tracedecay_runtime_core::workflow_topology::WorkflowTopologyError,
) -> DaemonInvocationProblem {
    use tracedecay_runtime_core::workflow_topology::WorkflowTopologyError;
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
