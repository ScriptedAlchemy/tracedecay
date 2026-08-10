use std::sync::Arc;

use tracedecay_application::{ApplicationProblem, RequestContext};
use tracedecay_domain::UtcMicros;

use crate::daemon_contract::DaemonInvocationProblem;

use super::RegisteredWorkRuntime;

pub(super) fn persist_workflow_fan_out_census(
    registered: &RegisteredWorkRuntime,
    workflow: &crate::global_db::RegisteredWorkflowApplicationServicesV1,
    context: &RequestContext,
    projection: &tracedecay_domain::WorkflowRunProjection,
    observed_at: UtcMicros,
    producer: Option<Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>>,
) {
    if let Err(error) = try_persist_workflow_fan_out_census(
        registered,
        workflow,
        context,
        projection,
        observed_at,
        producer,
    ) {
        tracing::warn!(
            ?error,
            run_id = %projection.run_id(),
            workflow_sequence = projection.sequence(),
            "workflow fan-out census remains pending"
        );
    }
}

fn try_persist_workflow_fan_out_census(
    registered: &RegisteredWorkRuntime,
    workflow: &crate::global_db::RegisteredWorkflowApplicationServicesV1,
    context: &RequestContext,
    projection: &tracedecay_domain::WorkflowRunProjection,
    observed_at: UtcMicros,
    producer: Option<Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>>,
) -> Result<(), DaemonInvocationProblem> {
    if projection.fan_out_plans().is_empty() {
        return Ok(());
    }
    let work = registered
        .database
        .work_application_services()
        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let snapshot = work
        .projections()
        .snapshot(
            context,
            tracedecay_application::MAX_WORK_PROJECTION_PAGE_SIZE,
        )
        .ok();
    let topology_generation = tracedecay_domain::WorkAuthority::new(
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        context.actor().clone(),
        context.grant().digest.clone(),
    )
    .ok()
    .and_then(|authority| {
        work.topology()
            .verified_snapshot(
                &authority,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .ok()
    })
    .and_then(|topology| topology.evidence_ref().ok());
    let mut attempts = Vec::new();
    let mut attempt_reads_complete = true;
    for child in projection
        .fan_out_plans()
        .values()
        .flat_map(|plan| &plan.children)
    {
        match work.attempts().status(
            context,
            &tracedecay_application::WorkAttemptStatusRequestV1 {
                task_id: child.task_id.clone(),
                run_id: child.attempt_identity.run_id().clone(),
                attempt_id: child.attempt_identity.attempt_id().clone(),
            },
        ) {
            Ok(attempt) => attempts.push(attempt),
            Err(ApplicationProblem::NotFoundOrNotAuthorized { .. }) => {}
            Err(_) => attempt_reads_complete = false,
        }
    }
    let readiness = census_readiness(
        registered,
        &work,
        context,
        projection,
        &attempts,
        attempt_reads_complete,
    );
    let (runnable, blocked) = readiness
        .as_ref()
        .map_or((None, None), |(runnable, blocked)| {
            (Some(runnable), Some(blocked))
        });
    let shared_authority_waits = projection
        .fan_out_plans()
        .values()
        .all(|plan| plan.effect_state == tracedecay_domain::WorkEffectStateV1::Observational)
        .then(std::collections::BTreeSet::new);
    let latest = tracedecay_application::WorkflowFanOutCensusStoragePort::latest_census(
        workflow.effects(),
        projection.run_id(),
    )
    .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    if latest
        .as_ref()
        .is_some_and(|census| census.workflow_sequence > projection.sequence())
    {
        return Err(DaemonInvocationProblem::Unavailable);
    }
    let previous = tracedecay_application::WorkflowFanOutCensusStoragePort::census_before(
        workflow.effects(),
        projection.run_id(),
        projection.sequence(),
    )
    .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let non_duplicate_attempts = snapshot
        .as_ref()
        .zip(topology_generation.as_ref())
        .and_then(|(snapshot, topology_generation)| {
            if !matches!(
                snapshot.coverage(),
                tracedecay_domain::WorkProjectionCoverageV1::Complete { .. }
            ) {
                return None;
            }
            let read = work
                .duplicate_adjudications()
                .classify_attempts(
                    context,
                    tracedecay_application::WorkDuplicateAttemptClassificationRequestV1 {
                        work_generation: snapshot.generation_id().clone(),
                        topology_generation: topology_generation.clone(),
                        attempts: attempts
                            .iter()
                            .map(|attempt| attempt.identity().clone())
                            .collect(),
                        observed_at,
                    },
                )
                .ok()?;
            match read {
                tracedecay_application::WorkDuplicateAttemptClassificationReadV1::Complete {
                    classification,
                } => Some(
                    classification
                        .non_duplicate_attempts
                        .into_iter()
                        .collect::<std::collections::BTreeSet<_>>(),
                ),
                tracedecay_application::WorkDuplicateAttemptClassificationReadV1::Unavailable {
                    ..
                } => None,
            }
        });
    let census = match latest {
        Some(census) if census.workflow_sequence == projection.sequence() => census,
        Some(_) | None => {
            let census = tracedecay_application::derive_workflow_fan_out_census(
                projection,
                &tracedecay_application::WorkflowFanOutCensusEvidenceV1 {
                    work_snapshot: snapshot.as_ref(),
                    attempts: &attempts,
                    attempt_reads_complete,
                    shared_authority_waits: shared_authority_waits.as_ref(),
                    non_duplicate_attempts: non_duplicate_attempts.as_ref(),
                    runnable_children: runnable,
                    blocked_children: blocked,
                    previous: previous.as_ref(),
                    observed_at,
                },
            )
            .map_err(|_| DaemonInvocationProblem::Unavailable)?;
            tracedecay_application::WorkflowFanOutCensusStoragePort::persist_census(
                workflow.effects(),
                &census,
            )
            .map_err(|_| DaemonInvocationProblem::Unavailable)?;
            census
        }
    };

    if let Some(producer) = producer.as_deref() {
        match tracedecay_usecases::observability::record_workflow_settlement(
            Some(producer),
            projection,
            &census,
            &attempts,
            attempt_reads_complete,
        ) {
            tracedecay_usecases::observability::WorkOwnerObservationResultV1::Enqueued => {}
            tracedecay_usecases::observability::WorkOwnerObservationResultV1::DroppedAtCapacity
            | tracedecay_usecases::observability::WorkOwnerObservationResultV1::Unavailable => {
                return Err(DaemonInvocationProblem::Unavailable);
            }
        }
    }

    let (Some(previous), Some(sample), Some(producer)) = (
        previous.as_ref(),
        census.execution_topology_sample(),
        producer,
    ) else {
        return Ok(());
    };
    if previous.observed_at >= census.observed_at {
        return Ok(());
    }
    let terminal = match projection.status() {
        tracedecay_domain::WorkflowRunStatus::Completed => {
            Some(tracedecay_domain::ObservabilityTerminalResultV1::Succeeded)
        }
        tracedecay_domain::WorkflowRunStatus::Failed => {
            Some(tracedecay_domain::ObservabilityTerminalResultV1::Failed)
        }
        tracedecay_domain::WorkflowRunStatus::Cancelled => {
            Some(tracedecay_domain::ObservabilityTerminalResultV1::Cancelled)
        }
        tracedecay_domain::WorkflowRunStatus::Running
        | tracedecay_domain::WorkflowRunStatus::Paused
        | tracedecay_domain::WorkflowRunStatus::Cancelling => None,
    };
    let owner_ref = format!(
        "workflow-fan-out-census:{}:{}",
        projection.run_id().as_str(),
        projection.sequence()
    );
    let envelope = tracedecay_usecases::observability::execution_owner_fact_envelope(
        producer.identity(),
        context.scope().project_id.as_str(),
        &owner_ref,
        "workflow_fan_out_census",
        census.observed_at,
        Some(previous.observed_at),
        Some(census.observed_at),
        terminal,
        tracedecay_domain::CoverageStateV1::Known,
        tracedecay_domain::ObservabilityPayloadV1::ExecutionTopology(sample),
    )
    .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    match producer
        .try_emit_owner_fact(envelope)
        .map_err(|_| DaemonInvocationProblem::Unavailable)?
    {
        tracedecay_usecases::observability::ObservabilityEmissionOutcomeV1::Enqueued => Ok(()),
        tracedecay_usecases::observability::ObservabilityEmissionOutcomeV1::DroppedAtCapacity => {
            Err(DaemonInvocationProblem::Unavailable)
        }
    }
}

fn census_readiness(
    registered: &RegisteredWorkRuntime,
    work: &crate::global_db::RegisteredWorkApplicationServicesV1,
    context: &RequestContext,
    projection: &tracedecay_domain::WorkflowRunProjection,
    attempts: &[tracedecay_domain::WorkAttemptV1],
    attempt_reads_complete: bool,
) -> Option<(
    std::collections::BTreeSet<tracedecay_domain::WorkAttemptIdentityV1>,
    std::collections::BTreeSet<tracedecay_domain::WorkAttemptIdentityV1>,
)> {
    if !attempt_reads_complete {
        return None;
    }
    let existing = attempts
        .iter()
        .map(|attempt| attempt.identity().clone())
        .collect::<std::collections::BTreeSet<_>>();
    let candidates = projection
        .fan_out_plans()
        .values()
        .flat_map(|plan| {
            plan.children
                .iter()
                .filter(|child| {
                    !projection
                        .settled_fan_out_attempts()
                        .contains(&child.attempt_identity)
                        && !existing.contains(&child.attempt_identity)
                })
                .map(move |child| (plan, child))
        })
        .collect::<Vec<_>>();
    let task_ids = candidates
        .iter()
        .map(|(_, child)| child.task_id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let capacities = work
        .attempts()
        .admission_capacities_against_registered_topology(
            context,
            &task_ids,
            &registered.work_topology_policy,
        )
        .ok()?;
    let mut global_remaining = capacities
        .values()
        .next()
        .map(|capacity| {
            u64::from(
                registered
                    .work_topology_policy
                    .concurrency
                    .maximum_global_active
                    .get(),
            )
            .saturating_sub(capacity.global_active())
        })
        .unwrap_or_default();
    let mut repository_remaining = capacities
        .values()
        .next()
        .map(|capacity| {
            u64::from(
                registered
                    .work_topology_policy
                    .concurrency
                    .maximum_active_per_repository
                    .get(),
            )
            .saturating_sub(capacity.repository_active())
        })
        .unwrap_or_default();
    if capacities.values().any(|capacity| {
        capacity.concurrency() != &registered.work_topology_policy.concurrency
            || capacities.values().next().is_some_and(|first| {
                capacity.global_active() != first.global_active()
                    || capacity.repository_active() != first.repository_active()
            })
    }) {
        return None;
    }
    let mut task_remaining = capacities
        .iter()
        .map(|(task_id, capacity)| {
            (
                task_id.clone(),
                u64::from(
                    registered
                        .work_topology_policy
                        .concurrency
                        .maximum_parallel_per_task
                        .get(),
                )
                .saturating_sub(capacity.task_active()),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut runnable = std::collections::BTreeSet::new();
    let mut blocked = std::collections::BTreeSet::new();
    for (_plan, child) in candidates {
        if projection.status() != tracedecay_domain::WorkflowRunStatus::Running
            || !projection
                .released_fan_out_attempts()
                .contains(&child.attempt_identity)
        {
            blocked.insert(child.attempt_identity.clone());
            continue;
        }
        match work.commands().readiness(context, &child.task_id).ok()? {
            tracedecay_application::WorkReadiness::Ready => {}
            tracedecay_application::WorkReadiness::Blocked { .. }
            | tracedecay_application::WorkReadiness::Accepted => {
                blocked.insert(child.attempt_identity.clone());
                continue;
            }
        }
        match work.run_control().admit_reservation(
            context,
            &child.task_id,
            child.attempt_identity.run_id(),
        ) {
            Ok(()) => {}
            Err(ApplicationProblem::Conflict { .. }) => {
                blocked.insert(child.attempt_identity.clone());
                continue;
            }
            Err(_) => return None,
        }
        let capacity = capacities.get(&child.task_id)?;
        let remaining_for_task = task_remaining.get_mut(&child.task_id)?;
        match capacity.verdict() {
            tracedecay_application::WorkAttemptCapacityVerdictV1::Available
                if global_remaining > 0 && repository_remaining > 0 && *remaining_for_task > 0 =>
            {
                runnable.insert(child.attempt_identity.clone());
                global_remaining -= 1;
                repository_remaining -= 1;
                *remaining_for_task -= 1;
            }
            tracedecay_application::WorkAttemptCapacityVerdictV1::Available
            | tracedecay_application::WorkAttemptCapacityVerdictV1::Exhausted(_) => {
                blocked.insert(child.attempt_identity.clone());
            }
        }
    }
    Some((runnable, blocked))
}

#[derive(Clone)]
pub(in crate::daemon::service::invocation) struct WorkflowFanOutCensusObservationRecoveryOwnerV1 {
    inner: Arc<WorkflowFanOutCensusObservationRecoveryInnerV1>,
}

struct WorkflowFanOutCensusObservationRecoveryInnerV1 {
    cancellation: tracedecay_runtime_core::cancellation::CancellationToken,
    task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl WorkflowFanOutCensusObservationRecoveryOwnerV1 {
    pub(in crate::daemon::service::invocation) fn mount(
        database: Arc<crate::global_db::RegisteredGlobalDb>,
        project_id: tracedecay_domain::ProjectId,
        producer: Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
    ) -> Result<Self, tracedecay_application::ApplicationContractError> {
        if producer.identity().authorized_scope_ref != project_id.as_str() {
            return Err(tracedecay_application::ApplicationContractError::Domain(
                "workflow census recovery producer scope mismatch".to_owned(),
            ));
        }
        let cancellation = tracedecay_runtime_core::cancellation::CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            loop {
                let read_database = Arc::clone(&database);
                let mut read = tokio::task::spawn_blocking(move || {
                    read_pending_census_observations(&read_database)
                });
                let observations = tokio::select! {
                    biased;
                    () = worker_cancellation.cancelled() => {
                        read.abort();
                        return;
                    }
                    result = &mut read => match result {
                        Ok(Ok(observations)) => observations,
                        Ok(Err(error)) => {
                            tracing::warn!(?error, "pending workflow census recovery read failed");
                            Vec::new()
                        }
                        Err(error) => {
                            tracing::warn!(?error, "pending workflow census recovery worker failed");
                            Vec::new()
                        }
                    },
                };
                if !observations.is_empty() {
                    let Ok(envelopes) =
                        pending_census_envelopes(&producer, &project_id, &observations)
                    else {
                        tracing::warn!("pending workflow census envelope is invalid");
                        tokio::select! {
                            biased;
                            () = worker_cancellation.cancelled() => return,
                            () = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                        }
                        continue;
                    };
                    let emission = producer.emit_owner_facts(envelopes);
                    tokio::pin!(emission);
                    let emission = tokio::select! {
                        biased;
                        () = worker_cancellation.cancelled() => return,
                        outcome = &mut emission => outcome,
                    };
                    match emission {
                        Ok(_) => {
                            let mark_database = Arc::clone(&database);
                            let mut mark = tokio::task::spawn_blocking(move || {
                                mark_durable_census_observations(&mark_database, &observations)
                            });
                            let marked = tokio::select! {
                            biased;
                            () = worker_cancellation.cancelled() => {
                                mark.abort();
                                return;
                            }
                                result = &mut mark => result,
                            };
                            match marked {
                                Ok(Ok(())) => {}
                                Ok(Err(error)) => tracing::warn!(
                                    ?error,
                                    "workflow census durable marker remains pending"
                                ),
                                Err(error) => {
                                    tracing::warn!(?error, "workflow census marker worker failed")
                                }
                            }
                        }
                        Err(error) => {
                            tracing::warn!(?error, "pending workflow census durable claim failed");
                        }
                    }
                }
                tokio::select! {
                    biased;
                    () = worker_cancellation.cancelled() => return,
                    () = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                }
            }
        });
        Ok(Self {
            inner: Arc::new(WorkflowFanOutCensusObservationRecoveryInnerV1 {
                cancellation,
                task: std::sync::Mutex::new(Some(task)),
            }),
        })
    }

    pub(in crate::daemon::service) async fn shutdown(&self) {
        self.inner.cancellation.cancel();
        let task = self
            .inner
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task
            && let Err(error) = task.await
        {
            tracing::warn!(%error, "workflow census recovery shutdown failed");
        }
    }
}

impl Drop for WorkflowFanOutCensusObservationRecoveryInnerV1 {
    fn drop(&mut self) {
        self.cancellation.cancel();
        let task = match self.task.get_mut() {
            Ok(task) => task.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(task) = task {
            task.abort();
        }
    }
}

fn read_pending_census_observations(
    database: &crate::global_db::RegisteredGlobalDb,
) -> Result<
    Vec<tracedecay_application::WorkflowFanOutCensusObservationV1>,
    tracedecay_application::WorkflowFanOutCensusError,
> {
    let workflow = database
        .workflow_application_services()
        .map_err(|_| tracedecay_application::WorkflowFanOutCensusError::Unavailable)?;
    tracedecay_application::WorkflowFanOutCensusStoragePort::pending_census_observations(
        workflow.effects(),
        32,
    )
}

fn pending_census_envelopes(
    producer: &tracedecay_usecases::observability::BoundedObservabilityProducerV1,
    project_id: &tracedecay_domain::ProjectId,
    observations: &[tracedecay_application::WorkflowFanOutCensusObservationV1],
) -> Result<Vec<tracedecay_domain::ObservabilityEnvelopeV1>, &'static str> {
    observations
        .iter()
        .map(|observation| {
            let sample = observation
                .census
                .execution_topology_sample()
                .ok_or("workflow_census_observation_incomplete")?;
            let owner_ref = format!(
                "workflow-fan-out-census:{}:{}",
                observation.census.run_id.as_str(),
                observation.census.workflow_sequence
            );
            tracedecay_usecases::observability::execution_owner_fact_envelope(
                producer.identity(),
                project_id.as_str(),
                &owner_ref,
                "workflow_fan_out_census",
                observation.census.observed_at,
                Some(observation.previous_observed_at),
                Some(observation.census.observed_at),
                observation.terminal,
                tracedecay_domain::CoverageStateV1::Known,
                tracedecay_domain::ObservabilityPayloadV1::ExecutionTopology(sample),
            )
        })
        .collect()
}

fn mark_durable_census_observations(
    database: &crate::global_db::RegisteredGlobalDb,
    observations: &[tracedecay_application::WorkflowFanOutCensusObservationV1],
) -> Result<(), tracedecay_application::WorkflowFanOutCensusError> {
    let workflow = database
        .workflow_application_services()
        .map_err(|_| tracedecay_application::WorkflowFanOutCensusError::Unavailable)?;
    for observation in observations {
        tracedecay_application::WorkflowFanOutCensusStoragePort::mark_census_observability_durable(
            workflow.effects(),
            &observation.census,
        )?;
    }
    Ok(())
}
