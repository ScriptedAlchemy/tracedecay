use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_application::{
    AcceptProposalCommand, AdmitExecutionCommand, AttachRuntimeEvidenceCommand, CreateWorkCommand,
    RequestContext, ReviewProposalCommand, WORKFLOW_CANONICAL_WORK_OPERATION_V1,
    WorkExecutionError, WorkflowChildReceiptV1, WorkflowChildRecordV1,
    WorkflowExecutionAdmissionV1, WorkflowExecutionAuthorityError, WorkflowExecutionAuthorityPort,
    WorkflowExecutionTruthV1, WorkflowFailurePolicyV1, WorkflowFanOutPlanV1,
    WorkflowFanOutRequestV1, WorkflowFanOutRuntimeError, WorkflowPlannedChildV1,
    prepare_workflow_fan_out, validate_workflow_checkpoint, workflow_checkpoint, workflow_truth,
};
use tracedecay_domain::{
    ManifestDigest, TaskId, UtcMicros, WorkAttemptIdentityV1, WorkAttemptProjectionBindingV1,
    WorkAttemptStateV1, WorkAttemptV1, WorkCancellationRequestId, WorkCancellationRequestV1,
    WorkExecutionEnvelopeV1, WorkFenceEpochV1, WorkLeaseFenceV1, WorkLeaseId, WorkRecoveryStateV1,
    WorkTerminalEvidenceV1, WorkflowOperationRef, canonical_sha256,
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
    child: WorkflowPlannedChildV1,
    identity: WorkAttemptIdentityV1,
    lease: WorkLeaseFenceV1,
}

enum PreparedChild {
    Running(RunningChild),
    Terminal {
        child: WorkflowPlannedChildV1,
        attempt: Box<WorkAttemptV1>,
    },
}

pub(crate) async fn execute_canonical_workflow(
    database: &Arc<RegisteredGlobalDb>,
    runtime: &Arc<DaemonWorkRuntimeV1<WorkStorage>>,
    context: &RequestContext,
    project_root: &Path,
    request: WorkflowFanOutRequestV1,
) -> Result<WorkflowExecutionTruthV1, WorkflowFanOutRuntimeError> {
    validate_active_definition(database, context, &request)?;
    let plan = prepare_workflow_fan_out(&request)?;
    if plan.operation.as_str() != WORKFLOW_CANONICAL_WORK_OPERATION_V1 {
        return Err(WorkflowFanOutRuntimeError::InvalidPlan);
    }
    let authority = database
        .workflow_storage()
        .map_err(|_| authority_unavailable())?;
    let admission = authority
        .begin(&plan.identity, &request.fence, &plan.plan_digest)
        .map_err(authority_error)?;
    let mut records = match admission {
        WorkflowExecutionAdmissionV1::Execute => BTreeMap::new(),
        WorkflowExecutionAdmissionV1::Recover { checkpoint } => {
            validate_workflow_checkpoint(&plan, &checkpoint)?;
            checkpoint
                .children
                .into_iter()
                .map(|record| (record.task_id.clone(), record))
                .collect()
        }
        WorkflowExecutionAdmissionV1::Replay(truth) => {
            let checkpoint = truth.checkpoint();
            validate_workflow_checkpoint(&plan, checkpoint)?;
            validate_terminal_replay(runtime, context, project_root, &request, &plan, &truth)?;
            return Ok(truth);
        }
        WorkflowExecutionAdmissionV1::PlanConflict => {
            return Err(WorkflowFanOutRuntimeError::PlanConflict);
        }
        WorkflowExecutionAdmissionV1::StaleLease => {
            return Err(WorkflowFanOutRuntimeError::StaleFence);
        }
    };

    if request.cancellation.is_cancelled() {
        records.clear();
        for child in &plan.children {
            let Some(mut attempt) = runtime
                .attempt(&child.attempt_identity)
                .map_err(work_error)?
            else {
                continue;
            };
            validate_existing_attempt(
                context,
                project_root,
                &request,
                &plan.operation,
                child,
                &attempt,
            )?;
            let lease = child_lease(&request.fence, child)?;
            if !attempt.is_terminal() {
                if attempt.lease().lease_id() != lease.lease_id()
                    || attempt.lease().epoch() > lease.epoch()
                {
                    return Err(WorkflowFanOutRuntimeError::StaleFence);
                }
                if attempt.lease().epoch() < lease.epoch() {
                    runtime
                        .renew_lease(&child.attempt_identity, attempt.lease(), lease.clone())
                        .map_err(work_error)?;
                }
                attempt = cancel_child(
                    runtime,
                    &RunningChild {
                        child: child.clone(),
                        identity: child.attempt_identity.clone(),
                        lease: lease.clone(),
                    },
                )
                .await?;
            }
            attach_terminal_evidence(database, context, &request, child, &attempt)?;
            let record = child_record(&plan.plan_digest, child, attempt.lease(), Some(&attempt))?;
            upsert_child_record(&mut records, record)?;
        }
        let checkpoint = workflow_checkpoint(plan.plan_digest, records.into_values().collect());
        let truth = WorkflowExecutionTruthV1::Cancelled {
            checkpoint,
            cancellation: request.cancellation,
        };
        authority
            .complete(&plan.identity, &request.fence, &truth)
            .map_err(authority_error)?;
        return Ok(truth);
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
                    let record =
                        child_record(&plan.plan_digest, &running.child, &running.lease, None)?;
                    upsert_child_record(&mut records, record)?;
                    checkpoint_children(
                        &authority,
                        &plan.identity,
                        &request.fence,
                        &plan.plan_digest,
                        &records,
                    )?;
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
                    let record =
                        child_record(&plan.plan_digest, &child, attempt.lease(), Some(&attempt))?;
                    upsert_child_record(&mut records, record)?;
                    terminal_attempts.push(*attempt);
                    fail_fast |= matches!(plan.failure_policy, WorkflowFailurePolicyV1::FailFast)
                        && !matches!(
                            terminal_attempts
                                .last()
                                .and_then(tracedecay_domain::WorkAttemptV1::terminal),
                            Some(WorkTerminalEvidenceV1::Succeeded { .. })
                        );
                    checkpoint_children(
                        &authority,
                        &plan.identity,
                        &request.fence,
                        &plan.plan_digest,
                        &records,
                    )?;
                    if fail_fast {
                        break;
                    }
                }
            }
        }

        for running in active {
            let child = running.child.clone();
            let attempt = if fail_fast {
                cancel_child(runtime, &running).await?
            } else {
                settle_child(database, runtime, context, &request, running).await?
            };
            fail_fast |= matches!(plan.failure_policy, WorkflowFailurePolicyV1::FailFast)
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
            let record = child_record(&plan.plan_digest, &child, attempt.lease(), Some(&attempt))?;
            upsert_child_record(&mut records, record)?;
            terminal_attempts.push(attempt);
            checkpoint_children(
                &authority,
                &plan.identity,
                &request.fence,
                &plan.plan_digest,
                &records,
            )?;
        }
        if fail_fast {
            break;
        }
    }

    let checkpoint = workflow_checkpoint(plan.plan_digest, records.into_values().collect());
    let truth = workflow_truth(plan.failure_policy, checkpoint, &terminal_attempts)?;
    authority
        .complete(&plan.identity, &request.fence, &truth)
        .map_err(authority_error)?;
    Ok(truth)
}

fn validate_active_definition(
    database: &Arc<RegisteredGlobalDb>,
    context: &RequestContext,
    request: &WorkflowFanOutRequestV1,
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
    request: &WorkflowFanOutRequestV1,
    operation: &WorkflowOperationRef,
    child: WorkflowPlannedChildV1,
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
        request.provider.route.clone(),
        request.provider.backend,
        request.provider.model.clone(),
        request.provider.configuration_digest.clone(),
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        root,
        request.provider.reference.clone(),
        request.provider.commit.clone(),
        request.provider.deadline,
        request.provider.cancellation_generation,
        request.provider.budget,
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
    request: &WorkflowFanOutRequestV1,
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
    request: &WorkflowFanOutRequestV1,
    child: &WorkflowPlannedChildV1,
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

fn child_record(
    plan_digest: &ManifestDigest,
    child: &WorkflowPlannedChildV1,
    lease: &WorkLeaseFenceV1,
    attempt: Option<&WorkAttemptV1>,
) -> Result<WorkflowChildRecordV1, WorkflowFanOutRuntimeError> {
    let receipt = match attempt {
        Some(attempt)
            if attempt.identity() == &child.attempt_identity && attempt.lease() == lease =>
        {
            let terminal = attempt
                .terminal()
                .ok_or_else(|| child_unavailable("Work checkpoint lacks terminal evidence"))?;
            Some(WorkflowChildReceiptV1 {
                observation_digest: canonical_sha256(&(
                    "tracedecay.daemon.workflow-child-checkpoint.v1",
                    plan_digest,
                    &child.attempt_identity,
                    lease,
                    terminal,
                    attempt.artifacts(),
                ))
                .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?,
                terminal_receipt_digest: canonical_sha256(&(
                    "tracedecay.daemon.workflow-child-terminal.v1",
                    plan_digest,
                    &child.attempt_identity,
                    lease,
                    terminal,
                ))
                .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?,
            })
        }
        Some(_) => {
            return Err(child_unavailable(
                "Work checkpoint conflicts with the planned child fence",
            ));
        }
        None => None,
    };
    Ok(WorkflowChildRecordV1 {
        task_id: child.task_id.clone(),
        attempt_identity: child.attempt_identity.clone(),
        lease: lease.clone(),
        receipt,
    })
}

fn upsert_child_record(
    records: &mut BTreeMap<TaskId, WorkflowChildRecordV1>,
    candidate: WorkflowChildRecordV1,
) -> Result<(), WorkflowFanOutRuntimeError> {
    if let Some(stored) = records.get(&candidate.task_id) {
        let advances = stored.attempt_identity == candidate.attempt_identity
            && stored.lease.lease_id() == candidate.lease.lease_id()
            && stored.lease.epoch().get() <= candidate.lease.epoch().get()
            && match (&stored.receipt, &candidate.receipt) {
                (None, _) => true,
                (Some(stored_receipt), Some(candidate_receipt)) => {
                    stored.lease == candidate.lease && stored_receipt == candidate_receipt
                }
                (Some(_), None) => false,
            };
        if !advances {
            return Err(WorkflowFanOutRuntimeError::PlanConflict);
        }
    }
    records.insert(candidate.task_id.clone(), candidate);
    Ok(())
}

fn checkpoint_children(
    authority: &WorkflowAuthority,
    identity: &tracedecay_application::WorkflowExecutionIdentityV1,
    fence: &tracedecay_application::WorkflowExecutionFenceV1,
    plan_digest: &ManifestDigest,
    records: &BTreeMap<TaskId, WorkflowChildRecordV1>,
) -> Result<(), WorkflowFanOutRuntimeError> {
    let checkpoint = workflow_checkpoint(plan_digest.clone(), records.values().cloned().collect());
    authority
        .checkpoint(identity, fence, &checkpoint)
        .map_err(authority_error)
}

fn validate_terminal_replay(
    runtime: &DaemonWorkRuntimeV1<WorkStorage>,
    context: &RequestContext,
    project_root: &Path,
    request: &WorkflowFanOutRequestV1,
    plan: &WorkflowFanOutPlanV1,
    truth: &WorkflowExecutionTruthV1,
) -> Result<(), WorkflowFanOutRuntimeError> {
    let checkpoint = truth.checkpoint();
    let mut attempts = Vec::with_capacity(checkpoint.children.len());
    for record in &checkpoint.children {
        let child = plan
            .children
            .iter()
            .find(|child| {
                child.task_id == record.task_id && child.attempt_identity == record.attempt_identity
            })
            .ok_or(WorkflowFanOutRuntimeError::InvalidPlan)?;
        let attempt = runtime
            .attempt(&record.attempt_identity)
            .map_err(work_error)?
            .ok_or_else(|| {
                child_unavailable("terminal workflow replay references a missing Work attempt")
            })?;
        validate_existing_attempt(
            context,
            project_root,
            request,
            &plan.operation,
            child,
            &attempt,
        )?;
        if !attempt.is_terminal() {
            return Err(child_unavailable(
                "terminal workflow replay references a non-terminal Work attempt",
            ));
        }
        let canonical_record =
            child_record(&plan.plan_digest, child, attempt.lease(), Some(&attempt))?;
        if &canonical_record != record {
            return Err(child_unavailable(
                "terminal workflow replay conflicts with its durable child receipt",
            ));
        }
        attempts.push(attempt);
    }
    validate_terminal_attempts(checkpoint, &attempts)?;
    if !matches!(truth, WorkflowExecutionTruthV1::Cancelled { .. }) {
        let canonical =
            workflow_truth(plan.failure_policy, checkpoint.clone(), attempts.as_slice())?;
        if &canonical != truth {
            return Err(child_unavailable(
                "workflow terminal replay conflicts with canonical Work attempts",
            ));
        }
    }
    Ok(())
}

fn validate_terminal_attempts(
    checkpoint: &tracedecay_application::WorkflowFanOutCheckpointV1,
    attempts: &[WorkAttemptV1],
) -> Result<(), WorkflowFanOutRuntimeError> {
    if attempts.len() != checkpoint.children.len()
        || checkpoint.children.iter().any(|record| {
            !attempts.iter().any(|attempt| {
                attempt.identity() == &record.attempt_identity
                    && attempt.is_terminal()
                    && record.receipt.is_some()
            })
        })
    {
        return Err(child_unavailable(
            "workflow terminal replay conflicts with canonical Work attempts",
        ));
    }
    Ok(())
}

fn validate_existing_attempt(
    context: &RequestContext,
    project_root: &Path,
    request: &WorkflowFanOutRequestV1,
    operation: &WorkflowOperationRef,
    child: &WorkflowPlannedChildV1,
    attempt: &WorkAttemptV1,
) -> Result<(), WorkflowFanOutRuntimeError> {
    let execution = attempt.execution();
    if attempt.identity() != &child.attempt_identity
        || execution.attempt_identity() != &child.attempt_identity
        || execution.operation() != operation
        || execution.route() != &request.provider.route
        || execution.backend() != request.provider.backend
        || execution.model() != request.provider.model
        || execution.configuration_digest() != &request.provider.configuration_digest
        || execution.project_id() != &context.scope().project_id
        || execution.repository_id() != &context.scope().repository_id
        || execution.worktree_id() != &context.scope().worktree_id
        || Path::new(execution.worktree_root()) != project_root
        || execution.reference() != request.provider.reference.as_ref()
        || execution.commit() != &request.provider.commit
        || execution.deadline() != request.provider.deadline
        || execution.cancellation_generation() != request.provider.cancellation_generation
        || execution.budget() != request.provider.budget
        || execution.effect_state() != request.provider.effect_state
    {
        return Err(child_unavailable(
            "canonical Work attempt conflicts with the pinned workflow plan",
        ));
    }
    Ok(())
}

fn child_lease(
    workflow_fence: &tracedecay_application::WorkflowExecutionFenceV1,
    child: &WorkflowPlannedChildV1,
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

fn authority_error(error: WorkflowExecutionAuthorityError) -> WorkflowFanOutRuntimeError {
    match error {
        WorkflowExecutionAuthorityError::Conflict => WorkflowFanOutRuntimeError::StaleFence,
        WorkflowExecutionAuthorityError::Unavailable(message) => {
            WorkflowFanOutRuntimeError::AuthorityUnavailable(message)
        }
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
