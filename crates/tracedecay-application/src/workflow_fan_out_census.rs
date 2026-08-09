//! Generation-exact Workflow fan-out census derivation and persistence port.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use tracedecay_domain::configuration::{
    BranchTopologyKindV1, CrossMergeModeV1, ReviewTopologyKindV1, WorktreePlacementModeV1,
};
use tracedecay_domain::{
    ExecutionPlacementV1, ExecutionTopologyKindV1, IntegrationStrategyV1, ReviewTopologyV1, RunId,
    UtcMicros, WorkAttemptIdentityV1, WorkAttemptV1, WorkAuthority, WorkProjectionCoverageV1,
    WorkProjectionSnapshotV1, WorkTopologyBranchV1, WorkflowCensusCountV1,
    WorkflowCensusDurationV1, WorkflowCensusEvidenceReasonV1, WorkflowCensusGenerationV1,
    WorkflowExecutionTopologyClassificationV1, WorkflowExecutionTopologyEvidenceV1,
    WorkflowFanOutCensusV1, WorkflowProviderCapacityEvidenceV1, WorkflowProviderCapacityV1,
    WorkflowRunEventKind, WorkflowRunProjection, WorkflowRunStatus, WorkflowStepId,
};

/// Evidence read from the exact Work authority at the census transition.
pub struct WorkflowFanOutCensusEvidenceV1<'a> {
    pub work_snapshot: Option<&'a WorkProjectionSnapshotV1>,
    /// Every successfully read child attempt. A child absent from this slice is
    /// counted as not admitted only when `attempt_reads_complete` is true.
    pub attempts: &'a [WorkAttemptV1],
    pub attempt_reads_complete: bool,
    /// Exact children waiting on a shared writer/authority. `None` means the
    /// classification authority was unavailable, never zero.
    pub shared_authority_waits: Option<&'a BTreeSet<WorkAttemptIdentityV1>>,
    /// Exact duplicate adjudication negatives. Advancement is useful only
    /// when its attempt is present here; absence of this authority is typed.
    pub non_duplicate_attempts: Option<&'a BTreeSet<WorkAttemptIdentityV1>>,
    /// Exact readiness/control verdicts over every unfinished child.
    pub runnable_children: Option<&'a BTreeSet<WorkAttemptIdentityV1>>,
    pub blocked_children: Option<&'a BTreeSet<WorkAttemptIdentityV1>>,
    pub previous: Option<&'a WorkflowFanOutCensusV1>,
    pub observed_at: UtcMicros,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowFanOutCensusError {
    #[error("workflow fan-out census input is invalid")]
    InvalidInput,
    #[error("workflow fan-out census count exceeds the wire bound")]
    CountOverflow,
    #[error("workflow fan-out census storage is unavailable")]
    Unavailable,
    #[error("workflow fan-out census conflicts with the persisted transition")]
    Conflict,
    #[error("workflow fan-out census history is corrupt")]
    InvalidHistory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowFanOutCensusPersistOutcomeV1 {
    Persisted,
    Replayed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowFanOutCensusObservationV1 {
    pub census: WorkflowFanOutCensusV1,
    pub previous_observed_at: UtcMicros,
    pub terminal: Option<tracedecay_domain::ObservabilityTerminalResultV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowFanOutCensusBackfillPageV1 {
    pub projections: Vec<WorkflowRunProjection>,
    pub continuation: Option<crate::WorkflowActiveRunRecoveryCursorV1>,
}

pub trait WorkflowFanOutCensusStoragePort: Send + Sync {
    fn latest_census(
        &self,
        run_id: &RunId,
    ) -> Result<Option<WorkflowFanOutCensusV1>, WorkflowFanOutCensusError>;

    fn census_before(
        &self,
        run_id: &RunId,
        workflow_sequence: u64,
    ) -> Result<Option<WorkflowFanOutCensusV1>, WorkflowFanOutCensusError>;

    fn persist_census(
        &self,
        census: &WorkflowFanOutCensusV1,
    ) -> Result<WorkflowFanOutCensusPersistOutcomeV1, WorkflowFanOutCensusError>;

    fn pending_census_observations(
        &self,
        limit: u16,
    ) -> Result<Vec<WorkflowFanOutCensusObservationV1>, WorkflowFanOutCensusError>;

    /// Reads one bounded page of current workflow journal heads that have no
    /// census for their exact sequence. Terminal heads remain eligible.
    fn census_backfill_projection_page(
        &self,
        authority: &WorkAuthority,
        after: Option<&crate::WorkflowActiveRunRecoveryCursorV1>,
    ) -> Result<WorkflowFanOutCensusBackfillPageV1, WorkflowFanOutCensusError>;

    fn mark_census_observability_durable(
        &self,
        census: &WorkflowFanOutCensusV1,
    ) -> Result<(), WorkflowFanOutCensusError>;
}

pub fn derive_workflow_fan_out_census(
    projection: &WorkflowRunProjection,
    evidence: &WorkflowFanOutCensusEvidenceV1<'_>,
) -> Result<WorkflowFanOutCensusV1, WorkflowFanOutCensusError> {
    if projection.fan_out_plans().is_empty()
        || evidence.observed_at
            < projection
                .history()
                .last()
                .map(|event| event.occurred_at())
                .ok_or(WorkflowFanOutCensusError::InvalidInput)?
        || evidence
            .previous
            .is_some_and(|prior| prior.run_id != *projection.run_id())
    {
        return Err(WorkflowFanOutCensusError::InvalidInput);
    }
    let children = projection
        .fan_out_plans()
        .values()
        .flat_map(|plan| &plan.children)
        .collect::<Vec<_>>();
    let requested = count(children.len())?;
    let attempts = evidence
        .attempts
        .iter()
        .map(|attempt| (attempt.identity().clone(), attempt))
        .collect::<BTreeMap<_, _>>();
    if attempts.len() != evidence.attempts.len()
        || attempts.keys().any(|identity| {
            !children
                .iter()
                .any(|child| &child.attempt_identity == identity)
        })
        || evidence.non_duplicate_attempts.is_some_and(|classified| {
            classified
                .iter()
                .any(|identity| !attempts.contains_key(identity))
        })
    {
        return Err(WorkflowFanOutCensusError::InvalidInput);
    }

    let (work_generation, accepted_width, admitted_width, generation_exact) =
        classify_work_projection(&children, evidence.work_snapshot)?;
    let generation_id = generation_exact.as_ref();
    let generation_mismatch = attempts.values().any(|attempt| {
        generation_id
            .is_some_and(|generation| attempt.projection_binding().generation_id() != generation)
    });
    let interval_started_at = evidence
        .previous
        .map_or(evidence.observed_at, |previous| previous.observed_at);
    let terminal_time_invalid = attempts.values().any(|attempt| {
        attempt
            .terminal()
            .is_some_and(|terminal| terminal.observed_at() > evidence.observed_at)
    });
    let attempts_exact =
        evidence.attempt_reads_complete && !generation_mismatch && !terminal_time_invalid;
    let active_observed = attempts
        .values()
        .filter(|attempt| {
            generation_id.is_some_and(|generation| {
                attempt.projection_binding().generation_id() == generation
            }) && attempt_active_in_interval(attempt, interval_started_at, evidence.observed_at)
        })
        .count();
    let active_width = exact_or_partial_count(
        active_observed,
        attempts_exact,
        if generation_mismatch {
            WorkflowCensusEvidenceReasonV1::WorkGenerationMismatch
        } else {
            WorkflowCensusEvidenceReasonV1::AttemptUnavailable
        },
    )?;

    let mut frontiers = children
        .iter()
        .map(|child| tracedecay_domain::WorkflowAttemptFrontierV1 {
            attempt: child.attempt_identity.clone(),
            completed: attempts
                .get(&child.attempt_identity)
                .and_then(|attempt| attempt.progress())
                .map(|progress| progress.completed()),
        })
        .collect::<Vec<_>>();
    frontiers.sort_by(|left, right| left.attempt.cmp(&right.attempt));
    let useful_width = useful_width(
        evidence.previous,
        &frontiers,
        &attempts,
        attempts_exact,
        generation_id,
        evidence.non_duplicate_attempts,
        active_observed,
    )?;
    let (runnable_count, blocked_count) = readiness_widths(
        projection,
        &attempts,
        evidence.runnable_children,
        evidence.blocked_children,
    )?;
    let shared_authority_serialized_count = match evidence.shared_authority_waits {
        Some(waits)
            if waits.iter().all(|identity| {
                attempts
                    .get(identity)
                    .is_some_and(|attempt| !attempt.is_terminal())
            }) =>
        {
            WorkflowCensusCountV1::Known {
                value: count(waits.len())?,
            }
        }
        Some(_) => return Err(WorkflowFanOutCensusError::InvalidInput),
        None => WorkflowCensusCountV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::SharedAuthorityEvidenceUnavailable,
        },
    };
    let (observed_duration, critical_path_duration) = durations(projection, evidence.observed_at)?;
    let provider_capacities = provider_capacities(
        projection,
        evidence.work_snapshot,
        &attempts,
        attempts_exact,
        generation_id,
        interval_started_at,
        evidence.observed_at,
    )?;
    let census = WorkflowFanOutCensusV1 {
        run_id: projection.run_id().clone(),
        workflow_sequence: projection.sequence(),
        topology_digest: projection.pinned_topology_digest().clone(),
        provider_registry_digest: projection.pinned_provider_registry_digest().clone(),
        work_generation,
        execution_topology: classify_execution_topology(projection),
        interval_started_at,
        observed_at: evidence.observed_at,
        requested_width: WorkflowCensusCountV1::Known { value: requested },
        accepted_width,
        admitted_width,
        active_width,
        useful_width,
        runnable_count,
        blocked_count,
        shared_authority_serialized_count,
        provider_capacities,
        observed_duration,
        critical_path_duration,
        attempt_frontiers: frontiers,
    };
    census
        .validate()
        .map_err(|_| WorkflowFanOutCensusError::InvalidInput)?;
    Ok(census)
}

fn classify_work_projection(
    children: &[&tracedecay_domain::WorkflowFanOutChildPlanV1],
    snapshot: Option<&WorkProjectionSnapshotV1>,
) -> Result<
    (
        WorkflowCensusGenerationV1,
        WorkflowCensusCountV1,
        WorkflowCensusCountV1,
        Option<tracedecay_domain::ProjectionGenerationId>,
    ),
    WorkflowFanOutCensusError,
> {
    let Some(snapshot) = snapshot else {
        return Ok((
            WorkflowCensusGenerationV1::Unavailable {
                reason: WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable,
            },
            WorkflowCensusCountV1::Unavailable {
                reason: WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable,
            },
            WorkflowCensusCountV1::Unavailable {
                reason: WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable,
            },
            None,
        ));
    };
    if !matches!(
        snapshot.coverage(),
        WorkProjectionCoverageV1::Complete { .. }
    ) {
        return Ok((
            WorkflowCensusGenerationV1::Unavailable {
                reason: WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable,
            },
            WorkflowCensusCountV1::Partial {
                observed: 0,
                reason: WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable,
            },
            WorkflowCensusCountV1::Partial {
                observed: 0,
                reason: WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable,
            },
            None,
        ));
    }
    let accepted = children
        .iter()
        .filter(|child| {
            snapshot.projections().iter().any(|projection| {
                projection.task_id() == &child.task_id
                    && projection.accepted_proposal() == Some(&child.proposal_id)
            })
        })
        .count();
    let admitted = children
        .iter()
        .filter(|child| {
            snapshot.projections().iter().any(|projection| {
                projection.task_id() == &child.task_id
                    && projection.accepted_proposal() == Some(&child.proposal_id)
                    && projection.is_execution_admitted()
            })
        })
        .count();
    Ok((
        WorkflowCensusGenerationV1::Exact {
            generation_id: snapshot.generation_id().clone(),
        },
        WorkflowCensusCountV1::Known {
            value: count(accepted)?,
        },
        WorkflowCensusCountV1::Known {
            value: count(admitted)?,
        },
        Some(snapshot.generation_id().clone()),
    ))
}

fn useful_width(
    previous: Option<&WorkflowFanOutCensusV1>,
    current: &[tracedecay_domain::WorkflowAttemptFrontierV1],
    attempts: &BTreeMap<WorkAttemptIdentityV1, &WorkAttemptV1>,
    attempts_exact: bool,
    generation: Option<&tracedecay_domain::ProjectionGenerationId>,
    non_duplicate_attempts: Option<&BTreeSet<WorkAttemptIdentityV1>>,
    active_width: usize,
) -> Result<WorkflowCensusCountV1, WorkflowFanOutCensusError> {
    let Some(previous) = previous else {
        return Ok(WorkflowCensusCountV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::FirstObservation,
        });
    };
    let WorkflowCensusGenerationV1::Exact {
        generation_id: previous_generation,
    } = &previous.work_generation
    else {
        return Ok(WorkflowCensusCountV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::ProgressFrontierUnavailable,
        });
    };
    if generation != Some(previous_generation) {
        return Ok(WorkflowCensusCountV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::WorkGenerationMismatch,
        });
    }
    let prior = previous
        .attempt_frontiers
        .iter()
        .map(|frontier| (&frontier.attempt, frontier.completed))
        .collect::<BTreeMap<_, _>>();
    let advanced = current
        .iter()
        .filter(|frontier| {
            attempts.contains_key(&frontier.attempt)
                && frontier.completed.is_some_and(|completed| {
                    prior.get(&frontier.attempt).is_some_and(|before| {
                        before.map_or(completed > 0, |before| completed > before)
                    })
                })
        })
        .map(|frontier| frontier.attempt.clone())
        .collect::<BTreeSet<_>>();
    if advanced.is_empty() {
        return exact_or_partial_count(
            0,
            attempts_exact,
            WorkflowCensusEvidenceReasonV1::ProgressFrontierUnavailable,
        );
    }
    let Some(non_duplicate_attempts) = non_duplicate_attempts else {
        return Ok(WorkflowCensusCountV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::DuplicateAdjudicationUnavailable,
        });
    };
    let useful = advanced
        .iter()
        .filter(|attempt| non_duplicate_attempts.contains(*attempt))
        .count();
    exact_or_partial_count(
        useful,
        attempts_exact && useful <= active_width,
        WorkflowCensusEvidenceReasonV1::ProgressFrontierUnavailable,
    )
}

fn readiness_widths(
    projection: &WorkflowRunProjection,
    attempts: &BTreeMap<WorkAttemptIdentityV1, &WorkAttemptV1>,
    runnable_children: Option<&BTreeSet<WorkAttemptIdentityV1>>,
    blocked_children: Option<&BTreeSet<WorkAttemptIdentityV1>>,
) -> Result<(WorkflowCensusCountV1, WorkflowCensusCountV1), WorkflowFanOutCensusError> {
    let (Some(runnable), Some(blocked)) = (runnable_children, blocked_children) else {
        let unavailable = WorkflowCensusCountV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::ReadinessEvidenceUnavailable,
        };
        return Ok((unavailable.clone(), unavailable));
    };
    if !runnable.is_disjoint(blocked) {
        return Err(WorkflowFanOutCensusError::InvalidInput);
    }
    let unfinished = projection
        .fan_out_plans()
        .values()
        .flat_map(|plan| &plan.children)
        .filter(|child| {
            !projection
                .settled_fan_out_attempts()
                .contains(&child.attempt_identity)
                && !attempts.contains_key(&child.attempt_identity)
        })
        .map(|child| child.attempt_identity.clone())
        .collect::<BTreeSet<_>>();
    if runnable.union(blocked).cloned().collect::<BTreeSet<_>>() != unfinished {
        return Err(WorkflowFanOutCensusError::InvalidInput);
    }
    Ok((
        WorkflowCensusCountV1::Known {
            value: count(runnable.len())?,
        },
        WorkflowCensusCountV1::Known {
            value: count(blocked.len())?,
        },
    ))
}

fn provider_capacities(
    projection: &WorkflowRunProjection,
    work_snapshot: Option<&WorkProjectionSnapshotV1>,
    attempts: &BTreeMap<WorkAttemptIdentityV1, &WorkAttemptV1>,
    attempts_exact: bool,
    generation: Option<&tracedecay_domain::ProjectionGenerationId>,
    interval_started_at: UtcMicros,
    observed_at: UtcMicros,
) -> Result<WorkflowProviderCapacityEvidenceV1, WorkflowFanOutCensusError> {
    let mut providers = BTreeMap::new();
    for plan in projection.fan_out_plans().values() {
        let snapshot = &plan.execution_snapshot;
        let topology_digest = snapshot
            .topology()
            .compute_digest()
            .map_err(|_| WorkflowFanOutCensusError::InvalidInput)?
            .0;
        if &topology_digest != projection.pinned_topology_digest() {
            return Ok(WorkflowProviderCapacityEvidenceV1::Unavailable {
                reason: WorkflowCensusEvidenceReasonV1::InconsistentPinnedTopology,
            });
        }
        let provider = snapshot.route().provider_id().clone();
        let policy = &snapshot.topology().concurrency;
        let limits = (
            policy.maximum_global_active.get(),
            policy.maximum_active_per_repository.get(),
            policy.maximum_parallel_per_task.get(),
            0usize,
            0usize,
        );
        let entry = providers.entry(provider.clone()).or_insert(limits);
        if entry.0 != limits.0 || entry.1 != limits.1 || entry.2 != limits.2 {
            return Ok(WorkflowProviderCapacityEvidenceV1::Unavailable {
                reason: WorkflowCensusEvidenceReasonV1::InconsistentPinnedTopology,
            });
        }
        for child in &plan.children {
            if work_snapshot.is_some_and(|snapshot| {
                matches!(
                    snapshot.coverage(),
                    WorkProjectionCoverageV1::Complete { .. }
                ) && snapshot.projections().iter().any(|projection| {
                    projection.task_id() == &child.task_id
                        && projection.accepted_proposal() == Some(&child.proposal_id)
                        && projection.is_execution_admitted()
                })
            }) {
                providers
                    .get_mut(&provider)
                    .ok_or(WorkflowFanOutCensusError::InvalidInput)?
                    .3 += 1;
            }
            if let Some(attempt) = attempts.get(&child.attempt_identity)
                && generation.is_some_and(|expected| {
                    attempt.projection_binding().generation_id() == expected
                })
            {
                if attempt_active_in_interval(attempt, interval_started_at, observed_at) {
                    let active_provider = attempt
                        .actual_route()
                        .unwrap_or_else(|| attempt.requested_route())
                        .provider_id()
                        .clone();
                    let active_entry = providers.entry(active_provider).or_insert(limits);
                    if active_entry.0 != limits.0
                        || active_entry.1 != limits.1
                        || active_entry.2 != limits.2
                    {
                        return Ok(WorkflowProviderCapacityEvidenceV1::Unavailable {
                            reason: WorkflowCensusEvidenceReasonV1::InconsistentPinnedTopology,
                        });
                    }
                    active_entry.4 += 1;
                }
            }
        }
    }
    let providers = providers
        .into_iter()
        .map(
            |(provider_id, (global, repository, task, admitted, active))| {
                Ok(WorkflowProviderCapacityV1 {
                    provider_id,
                    maximum_global_active: global,
                    maximum_active_per_repository: repository,
                    maximum_parallel_per_task: task,
                    admitted: exact_or_partial_count(
                        admitted,
                        work_snapshot.is_some_and(|snapshot| {
                            matches!(
                                snapshot.coverage(),
                                WorkProjectionCoverageV1::Complete { .. }
                            )
                        }),
                        WorkflowCensusEvidenceReasonV1::WorkProjectionUnavailable,
                    )?,
                    active: exact_or_partial_count(
                        active,
                        attempts_exact,
                        WorkflowCensusEvidenceReasonV1::AttemptUnavailable,
                    )?,
                })
            },
        )
        .collect::<Result<Vec<_>, WorkflowFanOutCensusError>>()?;
    Ok(WorkflowProviderCapacityEvidenceV1::Known { providers })
}

fn attempt_is_live(attempt: &WorkAttemptV1) -> bool {
    matches!(
        attempt.state(),
        tracedecay_domain::WorkAttemptStateV1::Running
            | tracedecay_domain::WorkAttemptStateV1::CancellationRequested
            | tracedecay_domain::WorkAttemptStateV1::CancellationAcknowledged
            | tracedecay_domain::WorkAttemptStateV1::CancellationEscalated
    )
}

fn attempt_active_in_interval(
    attempt: &WorkAttemptV1,
    interval_started_at: UtcMicros,
    observed_at: UtcMicros,
) -> bool {
    attempt_is_live(attempt)
        || attempt.terminal().is_some_and(|terminal| {
            terminal.observed_at() > interval_started_at && terminal.observed_at() <= observed_at
        })
}

fn classify_execution_topology(
    projection: &WorkflowRunProjection,
) -> WorkflowExecutionTopologyEvidenceV1 {
    let Some(first) = projection.fan_out_plans().values().next() else {
        return WorkflowExecutionTopologyEvidenceV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::InconsistentPinnedTopology,
        };
    };
    let policy = first.execution_snapshot.topology();
    let same_policy = projection
        .fan_out_plans()
        .values()
        .all(|plan| plan.execution_snapshot.topology() == policy);
    let Some(branch) = singleton(&policy.branch_topology.allowed) else {
        return WorkflowExecutionTopologyEvidenceV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::InconsistentPinnedTopology,
        };
    };
    let Some(review) = singleton(&policy.review_topology.allowed) else {
        return WorkflowExecutionTopologyEvidenceV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::InconsistentPinnedTopology,
        };
    };
    if !same_policy {
        return WorkflowExecutionTopologyEvidenceV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::InconsistentPinnedTopology,
        };
    }
    WorkflowExecutionTopologyEvidenceV1::Known {
        value: WorkflowExecutionTopologyClassificationV1 {
            topology: if projection.definition().steps().len() == 1 {
                ExecutionTopologyKindV1::Parallel
            } else {
                ExecutionTopologyKindV1::Hybrid
            },
            placement: match policy.placement {
                WorktreePlacementModeV1::ExistingWorktreeOnly => ExecutionPlacementV1::InPlace,
                WorktreePlacementModeV1::SiblingOfPrimaryCheckout
                | WorktreePlacementModeV1::RepositoryLocalRoot
                | WorktreePlacementModeV1::ConfiguredRoot(_) => {
                    ExecutionPlacementV1::LinkedWorktree
                }
            },
            branch_topology: match branch {
                BranchTopologyKindV1::NoBranches => WorkTopologyBranchV1::NoBranches,
                BranchTopologyKindV1::Unbranched => WorkTopologyBranchV1::Unbranched,
                BranchTopologyKindV1::IndependentBranches => {
                    WorkTopologyBranchV1::IndependentBranches
                }
                BranchTopologyKindV1::LocalStack => WorkTopologyBranchV1::LocalStack,
            },
            review_topology: match review {
                ReviewTopologyKindV1::NoReview => ReviewTopologyV1::NoReview,
                ReviewTopologyKindV1::IndependentReview => ReviewTopologyV1::IndependentReview,
                ReviewTopologyKindV1::StandardPullRequests => {
                    ReviewTopologyV1::StandardPullRequests
                }
                ReviewTopologyKindV1::GitHubStackedPullRequests => {
                    ReviewTopologyV1::GitHubStackedPullRequests
                }
            },
            integration_strategy: match policy.cross_merge.default_mode {
                CrossMergeModeV1::Disabled | CrossMergeModeV1::ManualReceiptOnly => {
                    IntegrationStrategyV1::NoIntegration
                }
                CrossMergeModeV1::FastForwardOnly => IntegrationStrategyV1::FastForwardOnly,
                CrossMergeModeV1::MergeCommit => IntegrationStrategyV1::MergeCommit,
                CrossMergeModeV1::CherryPickExactCommits => {
                    IntegrationStrategyV1::CherryPickExactCommits
                }
            },
        },
    }
}

fn singleton<T: Copy + Ord>(values: &BTreeSet<T>) -> Option<T> {
    if values.len() == 1 {
        values.iter().next().copied()
    } else {
        None
    }
}

fn durations(
    projection: &WorkflowRunProjection,
    observed_at: UtcMicros,
) -> Result<(WorkflowCensusDurationV1, WorkflowCensusDurationV1), WorkflowFanOutCensusError> {
    let admitted_at = projection
        .history()
        .first()
        .map(|event| event.occurred_at())
        .ok_or(WorkflowFanOutCensusError::InvalidInput)?;
    let end = if projection.status().is_terminal() {
        projection
            .history()
            .last()
            .map(|event| event.occurred_at())
            .ok_or(WorkflowFanOutCensusError::InvalidInput)?
    } else {
        observed_at
    };
    let observed = duration_between(admitted_at, end)?;
    let mut started = BTreeMap::new();
    let mut elapsed = BTreeMap::new();
    for event in projection.history() {
        match event.event() {
            WorkflowRunEventKind::StepStarted { step_id, .. } => {
                started.insert(step_id.clone(), event.occurred_at());
            }
            WorkflowRunEventKind::StepCompleted { step_id, .. }
            | WorkflowRunEventKind::StepFailed { step_id, .. } => {
                let Some(start) = started.get(step_id).copied() else {
                    return Err(WorkflowFanOutCensusError::InvalidInput);
                };
                elapsed.insert(
                    step_id.clone(),
                    duration_between(start, event.occurred_at())?,
                );
            }
            _ => {}
        }
    }
    let mut memo = BTreeMap::new();
    let critical = projection
        .definition()
        .steps()
        .iter()
        .filter_map(|step| critical_path(step.step_id.clone(), projection, &elapsed, &mut memo))
        .max()
        .unwrap_or(0);
    let critical_path_duration = if elapsed.len() == projection.definition().steps().len()
        && projection.status() == WorkflowRunStatus::Completed
    {
        WorkflowCensusDurationV1::Known { micros: critical }
    } else if !elapsed.is_empty() {
        WorkflowCensusDurationV1::Partial {
            observed_micros: critical,
            reason: WorkflowCensusEvidenceReasonV1::IncompleteWorkflow,
        }
    } else {
        WorkflowCensusDurationV1::Unavailable {
            reason: WorkflowCensusEvidenceReasonV1::IncompleteWorkflow,
        }
    };
    Ok((
        WorkflowCensusDurationV1::Known { micros: observed },
        critical_path_duration,
    ))
}

fn critical_path(
    step_id: WorkflowStepId,
    projection: &WorkflowRunProjection,
    elapsed: &BTreeMap<WorkflowStepId, u64>,
    memo: &mut BTreeMap<WorkflowStepId, u64>,
) -> Option<u64> {
    if let Some(value) = memo.get(&step_id) {
        return Some(*value);
    }
    let step = projection
        .definition()
        .steps()
        .iter()
        .find(|step| step.step_id == step_id)?;
    let own = *elapsed.get(&step_id)?;
    let predecessor = step
        .predecessors
        .iter()
        .filter_map(|predecessor| critical_path(predecessor.clone(), projection, elapsed, memo))
        .max()
        .unwrap_or(0);
    let total = own.saturating_add(predecessor);
    memo.insert(step_id, total);
    Some(total)
}

fn duration_between(start: UtcMicros, end: UtcMicros) -> Result<u64, WorkflowFanOutCensusError> {
    let delta = end
        .0
        .checked_sub(start.0)
        .ok_or(WorkflowFanOutCensusError::InvalidInput)?;
    u64::try_from(delta).map_err(|_| WorkflowFanOutCensusError::InvalidInput)
}

fn exact_or_partial_count(
    observed: usize,
    exact: bool,
    reason: WorkflowCensusEvidenceReasonV1,
) -> Result<WorkflowCensusCountV1, WorkflowFanOutCensusError> {
    let observed = count(observed)?;
    Ok(if exact {
        WorkflowCensusCountV1::Known { value: observed }
    } else {
        WorkflowCensusCountV1::Partial { observed, reason }
    })
}

fn count(value: usize) -> Result<u16, WorkflowFanOutCensusError> {
    u16::try_from(value).map_err(|_| WorkflowFanOutCensusError::CountOverflow)
}
