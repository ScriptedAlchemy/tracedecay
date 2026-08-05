use std::collections::{BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_application::{
    AcceptProposalCommand, AdmitExecutionCommand, AttachRuntimeEvidenceCommand, CreateWorkCommand,
    RequestContext, ReviewProposalCommand, WORKFLOW_CANONICAL_WORK_OPERATION, WorkExecutionError,
    WorkflowFailurePolicy, WorkflowFanOutPlan, WorkflowFanOutRequest, WorkflowFanOutRuntimeError,
    WorkflowPlannedChild, WorkflowRunService, WorkflowRunServiceError, WorkflowRunStorageError,
    WorkflowRunStoragePort, prepare_workflow_fan_out,
};
use tracedecay_domain::{
    ManifestDigest, UtcMicros, WorkAttemptIdentityV1, WorkAttemptProjectionBindingV1,
    WorkAttemptStateV1, WorkAttemptV1, WorkCancellationRequestId, WorkCancellationRequestV1,
    WorkCommandId, WorkExecutionEnvelopeV1, WorkFenceEpochV1, WorkLeaseFenceV1, WorkLeaseId,
    WorkRecoveryStateV1, WorkTerminalEvidenceV1, WorkflowOperationRef, WorkflowOutputArtifact,
    WorkflowRunCommand, WorkflowRunEventContext, WorkflowRunProjection, WorkflowStepEffectOutcome,
    WorkflowStepEffectReceipt, WorkflowStepOutput, WorkflowStepStatus, canonical_sha256,
};

use super::work_runtime::DaemonWorkRuntimeV1;
use crate::global_db::RegisteredGlobalDb;

type WorkStorage = tracedecay_rusqlite_runtime::work::WorkSqliteStorage;
type WorkflowAuthority = tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority;

#[cfg(test)]
static CRASH_AFTER_SETTLEMENT_BEFORE_CHECKPOINT: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn crash_after_next_workflow_settlement_for_test() {
    CRASH_AFTER_SETTLEMENT_BEFORE_CHECKPOINT.store(true, Ordering::Release);
}

struct RunningChild {
    child: WorkflowPlannedChild,
    identity: WorkAttemptIdentityV1,
    lease: WorkLeaseFenceV1,
}

enum PreparedChild {
    Running(RunningChild),
    Terminal {
        child: WorkflowPlannedChild,
        attempt: Box<WorkAttemptV1>,
    },
}

pub(crate) async fn execute_canonical_workflow(
    database: &Arc<RegisteredGlobalDb>,
    runtime: &Arc<DaemonWorkRuntimeV1<WorkStorage>>,
    context: &RequestContext,
    project_root: &Path,
    request: WorkflowFanOutRequest,
) -> Result<WorkflowRunProjection, WorkflowFanOutRuntimeError> {
    validate_active_definition(database, context, &request)?;
    let plan = prepare_workflow_fan_out(&request)?;
    if plan.operation.as_str() != WORKFLOW_CANONICAL_WORK_OPERATION {
        return Err(WorkflowFanOutRuntimeError::InvalidPlan);
    }
    let authority = workflow_authority(database)?;
    let run_service = WorkflowRunService::new(authority.clone());
    let placement = request
        .provider
        .placement(request.run_id.clone(), request.step_id.clone())?;
    let mut projection = match WorkflowRunStoragePort::projection(&authority, &request.run_id) {
        Ok(projection) => {
            if projection.definition() != &request.definition
                || projection.pinned_topology_digest() != &request.provider.topology_digest
                || projection.pinned_provider_registry_digest()
                    != &request.provider.provider_registry_digest
            {
                return Err(WorkflowFanOutRuntimeError::PlanConflict);
            }
            projection
        }
        Err(WorkflowRunStorageError::NotFound) => run_service
            .admit(
                request.run_id.clone(),
                request.definition.clone(),
                tracedecay_application::WorkflowAdmissionSnapshot {
                    policy_digest: context.grant().digest.clone(),
                    configuration_digest: request
                        .provider
                        .execution_snapshot
                        .effective_behavior_digest()
                        .clone(),
                    catalog_digest: request.definition.pinned_catalog_digest().clone(),
                    topology_digest: request.provider.topology_digest.clone(),
                    provider_registry_digest: request.provider.provider_registry_digest.clone(),
                },
                run_event_context(
                    &request.run_id,
                    "admit",
                    plan.plan_digest.clone(),
                    request.admitted_at,
                )?,
            )
            .map_err(run_service_error)?,
        Err(error) => return Err(run_storage_error(error)),
    };

    if projection.status().is_terminal() {
        return Ok(projection);
    }

    if request.cancellation.is_cancelled() {
        projection = run_service
            .apply(
                &request.run_id,
                projection.sequence(),
                WorkflowRunCommand::RequestCancellation,
                run_event_context(
                    &request.run_id,
                    "cancel-request",
                    plan.plan_digest.clone(),
                    request.admitted_at,
                )?,
            )
            .map_err(run_service_error)?;
        return run_service
            .apply(
                &request.run_id,
                projection.sequence(),
                WorkflowRunCommand::ReconcileCancelled,
                run_event_context(
                    &request.run_id,
                    "cancel-reconciled",
                    plan.plan_digest,
                    request.admitted_at,
                )?,
            )
            .map_err(run_service_error);
    }

    let step = projection
        .step(&request.step_id)
        .ok_or(WorkflowFanOutRuntimeError::StepNotFound)?;
    match step.status() {
        WorkflowStepStatus::Ready => {
            projection = run_service
                .apply(
                    &request.run_id,
                    projection.sequence(),
                    WorkflowRunCommand::StartStep {
                        step_id: request.step_id.clone(),
                        placement,
                    },
                    run_event_context(
                        &request.run_id,
                        "step-start",
                        plan.plan_digest.clone(),
                        request.admitted_at,
                    )?,
                )
                .map_err(run_service_error)?;
        }
        WorkflowStepStatus::Running => {}
        WorkflowStepStatus::Succeeded
        | WorkflowStepStatus::Failed
        | WorkflowStepStatus::Cancelled => return Ok(projection),
        WorkflowStepStatus::Blocked => return Err(WorkflowFanOutRuntimeError::InvalidPlan),
    }

    let mut pending = plan.children.iter().cloned().collect::<VecDeque<_>>();
    let mut terminal_attempts = Vec::new();
    let parallelism = usize::try_from(plan.max_parallel)
        .unwrap_or(usize::MAX)
        .min(runtime.capacity())
        .max(1);
    while !pending.is_empty() {
        let mut active = Vec::with_capacity(parallelism);
        let mut fail_fast = false;
        while active.len() < parallelism {
            let Some(child) = pending.pop_front() else {
                break;
            };
            match admit_child(
                database,
                runtime,
                context,
                project_root,
                &request,
                &plan.operation,
                child,
                true,
            )
            .await?
            {
                PreparedChild::Running(running) => {
                    runtime
                        .start(
                            &running.identity,
                            &running.lease,
                            WorkRecoveryStateV1::Fresh,
                        )
                        .await
                        .map_err(work_error)?;
                    active.push(running);
                }
                PreparedChild::Terminal { child, attempt } => {
                    attach_terminal_evidence(database, context, &request, &child, &attempt)?;
                    terminal_attempts.push(*attempt);
                    fail_fast |= matches!(plan.failure_policy, WorkflowFailurePolicy::FailFast)
                        && !matches!(
                            terminal_attempts
                                .last()
                                .and_then(tracedecay_domain::WorkAttemptV1::terminal),
                            Some(WorkTerminalEvidenceV1::Succeeded { .. })
                        );
                    if fail_fast {
                        break;
                    }
                }
            }
        }

        for running in active {
            let attempt = if fail_fast {
                cancel_child(runtime, &running).await?
            } else {
                settle_child(database, runtime, context, &request, running).await?
            };
            fail_fast |= matches!(plan.failure_policy, WorkflowFailurePolicy::FailFast)
                && !matches!(
                    attempt.terminal(),
                    Some(WorkTerminalEvidenceV1::Succeeded { .. })
                );
            #[cfg(test)]
            if CRASH_AFTER_SETTLEMENT_BEFORE_CHECKPOINT.swap(false, Ordering::AcqRel) {
                return Err(child_unavailable(
                    "injected crash after Work settlement before Workflow checkpoint",
                ));
            }
        }
        if fail_fast {
            break;
        }
    }

    let outputs = completed_outputs(&request, &plan, &terminal_attempts)?;
    let success_count = terminal_attempts
        .iter()
        .filter(|attempt| {
            matches!(
                attempt.terminal(),
                Some(WorkTerminalEvidenceV1::Succeeded { .. })
            )
        })
        .count();
    let succeeded = match plan.failure_policy {
        WorkflowFailurePolicy::FailFast | WorkflowFailurePolicy::Collect => {
            terminal_attempts.len() == plan.children.len() && success_count == plan.children.len()
        }
        WorkflowFailurePolicy::RequireAtLeast {
            successes: required,
        } => usize::try_from(required).is_ok_and(|required| success_count >= required),
    };
    let effect_digest = canonical_sha256(&(
        "tracedecay.daemon.workflow-step-effect.v1",
        &plan.plan_digest,
        &terminal_attempts,
        &outputs,
    ))
    .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
    let placement_digest = projection
        .step(&request.step_id)
        .and_then(|step| step.placement_receipt())
        .map(|placement| placement.placement_digest().clone())
        .ok_or(WorkflowFanOutRuntimeError::InvalidPlan)?;
    let effect_receipt = WorkflowStepEffectReceipt::new(
        request.run_id.clone(),
        request.step_id.clone(),
        placement_digest,
        if succeeded {
            WorkflowStepEffectOutcome::Completed
        } else {
            WorkflowStepEffectOutcome::Failed
        },
        effect_digest,
        &outputs,
    )
    .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
    let command = if succeeded {
        WorkflowRunCommand::CompleteStep {
            step_id: request.step_id.clone(),
            outputs,
            effect_receipt,
        }
    } else {
        WorkflowRunCommand::FailStep {
            step_id: request.step_id.clone(),
            outputs,
            effect_receipt,
        }
    };
    run_service
        .apply(
            &request.run_id,
            projection.sequence(),
            command,
            run_event_context(
                &request.run_id,
                "step-terminal",
                plan.plan_digest,
                request.admitted_at,
            )?,
        )
        .map_err(run_service_error)
}

fn workflow_authority(
    database: &RegisteredGlobalDb,
) -> Result<WorkflowAuthority, WorkflowFanOutRuntimeError> {
    let work = database
        .work_storage()
        .map_err(|_| authority_unavailable())?;
    WorkflowAuthority::from_work_storage(&work).map_err(|error| match error {
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthorityBuildError::ResetRequired => {
            WorkflowFanOutRuntimeError::ResetRequired
        }
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthorityBuildError::Unavailable => {
            authority_unavailable()
        }
    })
}

fn run_event_context(
    run_id: &tracedecay_domain::RunId,
    transition: &str,
    input_digest: ManifestDigest,
    occurred_at: UtcMicros,
) -> Result<WorkflowRunEventContext, WorkflowFanOutRuntimeError> {
    let command_digest = canonical_sha256(&(
        "tracedecay.daemon.workflow-run-command.v1",
        run_id,
        transition,
        &input_digest,
    ))
    .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
    Ok(WorkflowRunEventContext {
        command_id: WorkCommandId::new(format!(
            "workflow-run-{transition}:{}",
            command_digest.as_str()
        ))
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?,
        input_digest,
        occurred_at,
    })
}

fn completed_outputs(
    request: &WorkflowFanOutRequest,
    plan: &WorkflowFanOutPlan,
    attempts: &[WorkAttemptV1],
) -> Result<Vec<WorkflowStepOutput>, WorkflowFanOutRuntimeError> {
    let step = request
        .definition
        .steps()
        .iter()
        .find(|step| step.step_id == request.step_id)
        .ok_or(WorkflowFanOutRuntimeError::StepNotFound)?;
    if attempts.len() > plan.children.len() {
        return Err(WorkflowFanOutRuntimeError::InvalidPlan);
    }
    let succeeded = attempts
        .iter()
        .filter(|attempt| {
            matches!(
                attempt.terminal(),
                Some(WorkTerminalEvidenceV1::Succeeded { .. })
            )
        })
        .collect::<Vec<_>>();
    if succeeded.is_empty() {
        return Ok(Vec::new());
    }
    let mut outputs = Vec::with_capacity(step.outputs.len());
    for (ordinal, output_name) in step.outputs.iter().enumerate() {
        let artifacts = succeeded
            .iter()
            .map(|attempt| {
                let artifact = attempt
                    .artifacts()
                    .get(ordinal)
                    .cloned()
                    .ok_or(WorkflowFanOutRuntimeError::InvalidPlan)?;
                Ok(WorkflowOutputArtifact::new(
                    attempt.identity().clone(),
                    artifact,
                ))
            })
            .collect::<Result<Vec<_>, WorkflowFanOutRuntimeError>>()?;
        outputs.push(
            WorkflowStepOutput::new(output_name.clone(), artifacts)
                .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?,
        );
    }
    Ok(outputs)
}

fn run_storage_error(error: WorkflowRunStorageError) -> WorkflowFanOutRuntimeError {
    match error {
        WorkflowRunStorageError::VersionConflict => WorkflowFanOutRuntimeError::StaleFence,
        WorkflowRunStorageError::IdempotencyConflict => WorkflowFanOutRuntimeError::PlanConflict,
        WorkflowRunStorageError::NotFound
        | WorkflowRunStorageError::InvalidHistory
        | WorkflowRunStorageError::Unavailable => authority_unavailable(),
    }
}

fn run_service_error(error: WorkflowRunServiceError) -> WorkflowFanOutRuntimeError {
    match error {
        WorkflowRunServiceError::Storage(error) => run_storage_error(error),
        WorkflowRunServiceError::PolicyDigestMismatch
        | WorkflowRunServiceError::ConfigurationDigestMismatch
        | WorkflowRunServiceError::CatalogDigestMismatch
        | WorkflowRunServiceError::State(_) => WorkflowFanOutRuntimeError::InvalidPlan,
    }
}

fn validate_active_definition(
    database: &Arc<RegisteredGlobalDb>,
    context: &RequestContext,
    request: &WorkflowFanOutRequest,
) -> Result<(), WorkflowFanOutRuntimeError> {
    if request.definition.project_id() != &context.scope().project_id
        || request.definition.pinned_policy_digest() != &context.grant().digest
        || request.provider.reference != context.scope().reference
    {
        return Err(WorkflowFanOutRuntimeError::InvalidPlan);
    }
    let authority = database
        .workflow_storage()
        .map_err(|_| authority_unavailable())?;
    let active = tracedecay_application::WorkflowDefinitionAuthorityPort::active_version(
        &authority,
        request.definition.definition_id(),
    )
    .map_err(definition_authority_error)?;
    if active != Some(request.definition.definition_version()) {
        return Err(WorkflowFanOutRuntimeError::InvalidPlan);
    }
    let stored = tracedecay_application::WorkflowDefinitionAuthorityPort::load(
        &authority,
        request.definition.definition_id(),
        request.definition.definition_version(),
    )
    .map_err(definition_authority_error)?;
    if stored.as_ref() != Some(&request.definition) {
        return Err(WorkflowFanOutRuntimeError::InvalidPlan);
    }
    Ok(())
}

async fn admit_child(
    database: &Arc<RegisteredGlobalDb>,
    runtime: &Arc<DaemonWorkRuntimeV1<WorkStorage>>,
    context: &RequestContext,
    project_root: &Path,
    request: &WorkflowFanOutRequest,
    operation: &WorkflowOperationRef,
    child: WorkflowPlannedChild,
    allow_create_or_resume: bool,
) -> Result<PreparedChild, WorkflowFanOutRuntimeError> {
    let identity = child.attempt_identity.clone();
    let lease = child_lease(&request.fence, &child)?;
    if let Some(mut attempt) = runtime.attempt(&identity).map_err(work_error)? {
        validate_existing_attempt(context, project_root, request, operation, &child, &attempt)?;
        if attempt.lease().lease_id() != lease.lease_id() || attempt.lease().epoch() > lease.epoch()
        {
            return Err(WorkflowFanOutRuntimeError::StaleFence);
        }
        if attempt.is_terminal() {
            return Ok(PreparedChild::Terminal {
                child,
                attempt: Box::new(attempt),
            });
        }
        if !allow_create_or_resume {
            return Err(child_unavailable(
                "terminal workflow replay references a non-terminal Work attempt",
            ));
        }
        if attempt.state() == WorkAttemptStateV1::Running
            && attempt.execution().effect_state()
                != tracedecay_domain::WorkEffectStateV1::Observational
        {
            return Err(child_unavailable(
                "running Work attempt requires effect reconciliation before resume",
            ));
        }
        if attempt.lease().epoch() < lease.epoch() {
            attempt = runtime
                .renew_lease(&identity, attempt.lease(), lease.clone())
                .map_err(work_error)?;
        }
        match attempt.state() {
            WorkAttemptStateV1::Leased | WorkAttemptStateV1::Running => {
                return Ok(PreparedChild::Running(RunningChild {
                    child,
                    identity,
                    lease,
                }));
            }
            WorkAttemptStateV1::CancellationRequested
            | WorkAttemptStateV1::CancellationAcknowledged
            | WorkAttemptStateV1::CancellationEscalated => {
                let terminal = runtime
                    .finish(&identity, &lease, now())
                    .await
                    .map_err(work_error)?;
                return Ok(PreparedChild::Terminal {
                    child,
                    attempt: Box::new(terminal),
                });
            }
            WorkAttemptStateV1::RecoveryRequired => {
                return Err(child_unavailable(
                    "canonical Work attempt requires effect reconciliation",
                ));
            }
            WorkAttemptStateV1::Succeeded
            | WorkAttemptStateV1::Failed
            | WorkAttemptStateV1::TimedOut
            | WorkAttemptStateV1::Cancelled => {
                return Ok(PreparedChild::Terminal {
                    child,
                    attempt: Box::new(attempt),
                });
            }
        }
    }
    if !allow_create_or_resume {
        return Err(child_unavailable(
            "terminal workflow replay references a missing Work attempt",
        ));
    }
    let services = database
        .work_application_services()
        .map_err(|_| child_unavailable("canonical Work services are unavailable"))?;
    let work = services.commands();
    let created = work
        .create(
            context,
            CreateWorkCommand {
                task_id: child.task_id.clone(),
                title: format!(
                    "Workflow {} child {}",
                    request.step_id.as_str(),
                    child.input.identity
                ),
                dependencies: BTreeSet::new(),
                command_id: child.create_command_id.clone(),
                occurred_at: request.admitted_at,
            },
        )
        .map_err(|_| child_unavailable("canonical Work child creation failed"))?;
    let accepted = work
        .accept_proposal(
            context,
            AcceptProposalCommand {
                review: ReviewProposalCommand {
                    task_id: child.task_id.clone(),
                    proposal_id: child.proposal_id.clone(),
                    proposal_digest: child.proposal_digest.clone(),
                    expected_version: created.version(),
                    command_id: child.proposal_command_id.clone(),
                    occurred_at: request.admitted_at,
                },
            },
        )
        .map_err(|_| child_unavailable("canonical Work proposal acceptance failed"))?;
    let admitted = work
        .admit_execution(
            context,
            AdmitExecutionCommand {
                task_id: child.task_id.clone(),
                expected_version: accepted.version(),
                command_id: child.admit_command_id.clone(),
                occurred_at: request.admitted_at,
            },
        )
        .map_err(|_| child_unavailable("canonical Work execution admission failed"))?;
    let snapshot = services
        .projections()
        .exact_snapshot(context, &child.task_id)
        .map_err(|_| child_unavailable("canonical Work projection snapshot failed"))?;
    let projection = snapshot
        .projections()
        .iter()
        .find(|projection| projection.task_id() == &child.task_id)
        .ok_or_else(|| child_unavailable("admitted Work projection is missing"))?;
    if projection != &admitted {
        return Err(child_unavailable(
            "admitted Work projection changed before leasing",
        ));
    }
    let binding = WorkAttemptProjectionBindingV1::new(
        snapshot.generation_id().clone(),
        snapshot.sequence(),
        projection.version(),
        projection
            .accepted_proposal()
            .cloned()
            .ok_or_else(|| child_unavailable("admitted Work proposal is missing"))?,
    )
    .map_err(|_| child_unavailable("canonical Work projection binding failed"))?;
    let root = project_root
        .to_str()
        .ok_or_else(|| child_unavailable("Work worktree path is not UTF-8"))?
        .to_owned();
    let envelope = WorkExecutionEnvelopeV1::new(
        identity.clone(),
        binding,
        operation.clone(),
        request.provider.execution_snapshot.clone(),
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        root,
        request.provider.reference.clone(),
        request.provider.commit.clone(),
        request.provider.cancellation_generation,
        request.provider.effect_state,
    )
    .map_err(|_| child_unavailable("canonical Work execution envelope is invalid"))?;
    runtime
        .acquire_lease(&snapshot, identity.clone(), envelope, lease.clone())
        .await
        .map_err(work_error)?;
    Ok(PreparedChild::Running(RunningChild {
        child,
        identity,
        lease,
    }))
}

async fn settle_child(
    database: &Arc<RegisteredGlobalDb>,
    runtime: &Arc<DaemonWorkRuntimeV1<WorkStorage>>,
    context: &RequestContext,
    request: &WorkflowFanOutRequest,
    running: RunningChild,
) -> Result<WorkAttemptV1, WorkflowFanOutRuntimeError> {
    let attempt = runtime
        .finish(&running.identity, &running.lease, now())
        .await
        .map_err(work_error)?;
    attach_terminal_evidence(database, context, request, &running.child, &attempt)?;
    Ok(attempt)
}

async fn cancel_child(
    runtime: &Arc<DaemonWorkRuntimeV1<WorkStorage>>,
    running: &RunningChild,
) -> Result<WorkAttemptV1, WorkflowFanOutRuntimeError> {
    let requested_at = now();
    let request_id = WorkCancellationRequestId::new(format!(
        "cancel.workflow.fail-fast.{}",
        running.identity.attempt_id().as_str()
    ))
    .map_err(|_| child_unavailable("workflow cancellation identity is invalid"))?;
    runtime
        .cancel(
            &running.identity,
            &running.lease,
            WorkCancellationRequestV1::new(request_id, requested_at)
                .map_err(|_| child_unavailable("workflow cancellation is invalid"))?,
        )
        .await
        .map_err(work_error)
}

fn attach_terminal_evidence(
    database: &Arc<RegisteredGlobalDb>,
    context: &RequestContext,
    request: &WorkflowFanOutRequest,
    child: &WorkflowPlannedChild,
    attempt: &tracedecay_domain::WorkAttemptV1,
) -> Result<(), WorkflowFanOutRuntimeError> {
    let terminal = attempt
        .terminal()
        .ok_or_else(|| child_unavailable("Work attempt settled without terminal evidence"))?;
    let evidence = terminal
        .runtime_evidence_ref(request.run_id.clone())
        .map_err(|_| child_unavailable("Work terminal evidence reference is invalid"))?;
    let services = database
        .work_application_services()
        .map_err(|_| child_unavailable("canonical Work services are unavailable"))?;
    let projection = services
        .commands()
        .load(context, &child.task_id)
        .map_err(|_| child_unavailable("canonical Work projection is unavailable"))?;
    if projection.runtime_evidence().contains(&evidence) {
        return Ok(());
    }
    services
        .commands()
        .attach_runtime_evidence(
            context,
            AttachRuntimeEvidenceCommand {
                task_id: child.task_id.clone(),
                evidence,
                expected_version: projection.version(),
                command_id: child.evidence_command_id.clone(),
                occurred_at: request.admitted_at,
            },
        )
        .map_err(|_| child_unavailable("canonical Work evidence projection failed"))?;
    Ok(())
}

fn validate_existing_attempt(
    context: &RequestContext,
    project_root: &Path,
    request: &WorkflowFanOutRequest,
    operation: &WorkflowOperationRef,
    child: &WorkflowPlannedChild,
    attempt: &WorkAttemptV1,
) -> Result<(), WorkflowFanOutRuntimeError> {
    let execution = attempt.execution();
    if attempt.identity() != &child.attempt_identity
        || execution.attempt_identity() != &child.attempt_identity
        || execution.operation() != operation
        || execution.execution_snapshot() != &request.provider.execution_snapshot
        || execution.project_id() != &context.scope().project_id
        || execution.repository_id() != &context.scope().repository_id
        || execution.worktree_id() != &context.scope().worktree_id
        || Path::new(execution.worktree_root()) != project_root
        || execution.reference() != request.provider.reference.as_ref()
        || execution.commit() != &request.provider.commit
        || execution.cancellation_generation() != request.provider.cancellation_generation
        || execution.effect_state() != request.provider.effect_state
    {
        return Err(child_unavailable(
            "canonical Work attempt conflicts with the pinned workflow plan",
        ));
    }
    Ok(())
}

fn child_lease(
    workflow_fence: &tracedecay_application::WorkflowExecutionFence,
    child: &WorkflowPlannedChild,
) -> Result<WorkLeaseFenceV1, WorkflowFanOutRuntimeError> {
    let digest = canonical_sha256(&(
        "tracedecay.daemon.workflow-work-lease.v3",
        &workflow_fence.attempt_id,
        workflow_fence.lease.lease_id(),
        &child.attempt_identity,
    ))
    .map_err(|_| child_unavailable("Work lease identity could not be derived"))?;
    WorkLeaseFenceV1::new(
        WorkLeaseId::new(format!("workflow-work-lease:{}", digest.as_str()))
            .map_err(|_| child_unavailable("Work lease identity is invalid"))?,
        WorkFenceEpochV1::new(workflow_fence.lease.epoch().get())
            .map_err(|_| child_unavailable("Work lease fence is invalid"))?,
    )
    .map_err(|_| child_unavailable("Work lease fence is invalid"))
}

fn now() -> UtcMicros {
    UtcMicros(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_micros()).ok())
            .unwrap_or(i64::MAX),
    )
}

fn work_error(error: WorkExecutionError) -> WorkflowFanOutRuntimeError {
    match error {
        WorkExecutionError::Provider(
            tracedecay_application::WorkProviderExecutionError::Unavailable(_),
        ) => child_unavailable("configured Work provider is unavailable"),
        WorkExecutionError::Provider(
            tracedecay_application::WorkProviderExecutionError::Rejected(_),
        )
        | WorkExecutionError::Contract(_) => {
            child_unavailable("canonical Work provider rejected the admitted child")
        }
        WorkExecutionError::StaleLease => WorkflowFanOutRuntimeError::StaleFence,
        _ => child_unavailable("canonical Work attempt lifecycle failed"),
    }
}

fn definition_authority_error(
    error: tracedecay_application::WorkflowDefinitionAuthorityError,
) -> WorkflowFanOutRuntimeError {
    match error {
        tracedecay_application::WorkflowDefinitionAuthorityError::Unavailable(message) => {
            WorkflowFanOutRuntimeError::AuthorityUnavailable(message)
        }
        _ => WorkflowFanOutRuntimeError::InvalidPlan,
    }
}

fn authority_unavailable() -> WorkflowFanOutRuntimeError {
    WorkflowFanOutRuntimeError::AuthorityUnavailable(
        "registered workflow authority is unavailable".to_owned(),
    )
}

fn child_unavailable(message: &str) -> WorkflowFanOutRuntimeError {
    WorkflowFanOutRuntimeError::ChildUnavailable(message.to_owned())
}
