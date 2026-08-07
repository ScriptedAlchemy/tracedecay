//! Deterministic Work views over one exact product graph version.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    ProjectionGenerationId, TaskId, UtcMicros, WorkAttemptIdentityV1, WorkAttemptStateV1,
    WorkGraphVersionV1, WorkItemV1, WorkProductContractError, WorkProductGraphV1,
    WorkProjectionSequenceV1,
};

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkTimelineLaneV1 {
    Triage,
    Todo,
    Scheduled,
    Ready,
    Running,
    Blocked,
    Review,
    Done,
    Archived,
    Cancelled,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRuntimeAttemptProjectionV1 {
    pub identity: WorkAttemptIdentityV1,
    pub state: WorkAttemptStateV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkRuntimeProjectionCoverageV1 {
    Complete,
    Partial {
        unavailable_attempts: BTreeSet<WorkAttemptIdentityV1>,
    },
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRuntimeProjectionV1 {
    graph_version: WorkGraphVersionV1,
    generation_id: ProjectionGenerationId,
    sequence: WorkProjectionSequenceV1,
    observed_at: UtcMicros,
    attempts: Vec<WorkRuntimeAttemptProjectionV1>,
    coverage: WorkRuntimeProjectionCoverageV1,
}

impl WorkRuntimeProjectionV1 {
    pub fn new(
        graph_version: WorkGraphVersionV1,
        generation_id: ProjectionGenerationId,
        sequence: WorkProjectionSequenceV1,
        observed_at: UtcMicros,
        mut attempts: Vec<WorkRuntimeAttemptProjectionV1>,
        coverage: WorkRuntimeProjectionCoverageV1,
    ) -> Result<Self, WorkProductContractError> {
        attempts.sort_by(|left, right| left.identity.cmp(&right.identity));
        let projection = Self {
            graph_version,
            generation_id,
            sequence,
            observed_at,
            attempts,
            coverage,
        };
        projection.validate_shape()?;
        Ok(projection)
    }

    pub const fn graph_version(&self) -> WorkGraphVersionV1 {
        self.graph_version
    }

    pub fn generation_id(&self) -> &ProjectionGenerationId {
        &self.generation_id
    }

    pub const fn sequence(&self) -> WorkProjectionSequenceV1 {
        self.sequence
    }

    pub const fn observed_at(&self) -> UtcMicros {
        self.observed_at
    }

    pub fn attempts(&self) -> &[WorkRuntimeAttemptProjectionV1] {
        &self.attempts
    }

    pub const fn coverage(&self) -> &WorkRuntimeProjectionCoverageV1 {
        &self.coverage
    }

    pub fn validate(
        &self,
        graph: &WorkProductGraphV1,
        projected_at: UtcMicros,
    ) -> Result<(), WorkProductContractError> {
        self.validate_shape()?;
        if self.graph_version != graph.version() || self.observed_at != projected_at {
            return Err(WorkProductContractError::IllegalTransition);
        }
        let accepted = graph
            .items()
            .iter()
            .flat_map(|item| item.accepted_attempts().keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let observed = self
            .attempts
            .iter()
            .map(|attempt| attempt.identity.clone())
            .collect::<BTreeSet<_>>();
        if !observed.is_subset(&accepted) {
            return Err(WorkProductContractError::IllegalTransition);
        }
        match &self.coverage {
            WorkRuntimeProjectionCoverageV1::Complete if observed != accepted => {
                return Err(WorkProductContractError::IllegalTransition);
            }
            WorkRuntimeProjectionCoverageV1::Partial {
                unavailable_attempts,
            } if unavailable_attempts.is_empty()
                || observed.is_empty()
                || !observed.is_disjoint(unavailable_attempts)
                || observed
                    .union(unavailable_attempts)
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    != accepted =>
            {
                return Err(WorkProductContractError::IllegalTransition);
            }
            WorkRuntimeProjectionCoverageV1::Unavailable if !observed.is_empty() => {
                return Err(WorkProductContractError::IllegalTransition);
            }
            _ => {}
        }
        Ok(())
    }

    fn validate_shape(&self) -> Result<(), WorkProductContractError> {
        if self
            .attempts
            .windows(2)
            .any(|pair| pair[0].identity == pair[1].identity)
        {
            return Err(WorkProductContractError::DuplicateIdentity);
        }
        if self
            .attempts
            .windows(2)
            .any(|pair| pair[0].identity > pair[1].identity)
        {
            return Err(WorkProductContractError::IllegalTransition);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkKanbanCardV1 {
    pub task_id: TaskId,
    pub lane: WorkTimelineLaneV1,
    pub effort: u32,
    pub legal_actions: BTreeSet<WorkLegalActionV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkKanbanProjectionV1 {
    graph_version: WorkGraphVersionV1,
    cards: Vec<WorkKanbanCardV1>,
}

impl WorkKanbanProjectionV1 {
    pub const fn graph_version(&self) -> WorkGraphVersionV1 {
        self.graph_version
    }

    pub fn lane_for(&self, task_id: &TaskId) -> Option<WorkTimelineLaneV1> {
        self.cards
            .iter()
            .find(|card| &card.task_id == task_id)
            .map(|card| card.lane)
    }

    pub fn legal_actions_for(&self, task_id: &TaskId) -> Option<&BTreeSet<WorkLegalActionV1>> {
        self.cards
            .iter()
            .find(|card| &card.task_id == task_id)
            .map(|card| &card.legal_actions)
    }
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkLegalActionV1 {
    ViewEvidence,
    GenerateProposal,
    AcceptProposal,
    LinkAcceptedAttempt,
    AcceptTask,
    Handoff,
    Archive,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkDagEdgeV1 {
    pub dependency: TaskId,
    pub dependent: TaskId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkDagProjectionV1 {
    graph_version: WorkGraphVersionV1,
    task_ids: Vec<TaskId>,
    gating_edges: Vec<WorkDagEdgeV1>,
}

impl WorkDagProjectionV1 {
    pub const fn graph_version(&self) -> WorkGraphVersionV1 {
        self.graph_version
    }

    pub fn gating_edges(&self) -> &[WorkDagEdgeV1] {
        &self.gating_edges
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkTimelineEntryV1 {
    pub task_id: TaskId,
    pub created_at: UtcMicros,
    pub updated_at: UtcMicros,
    pub scheduled_at: Option<UtcMicros>,
    pub deadline: Option<UtcMicros>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkTimelineProjectionV1 {
    graph_version: WorkGraphVersionV1,
    entries: Vec<WorkTimelineEntryV1>,
}

impl WorkTimelineProjectionV1 {
    pub const fn graph_version(&self) -> WorkGraphVersionV1 {
        self.graph_version
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkCausalProjectionV1 {
    graph_version: WorkGraphVersionV1,
    candidate_edges: Vec<WorkDagEdgeV1>,
}

impl WorkCausalProjectionV1 {
    pub const fn graph_version(&self) -> WorkGraphVersionV1 {
        self.graph_version
    }

    /// The DECLARED causal candidates, as edges.
    ///
    /// These come from `WorkItemV1::causal_candidates` — relations a caller
    /// stated, never an order inferred from when attempts happened to finish.
    /// An empty slice therefore means "no candidate was declared", which is a
    /// true reading and not a missing one.
    pub fn candidate_edges(&self) -> &[WorkDagEdgeV1] {
        &self.candidate_edges
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkCriticalPathProjectionV1 {
    graph_version: WorkGraphVersionV1,
    task_ids: Vec<TaskId>,
    total_effort: u32,
}

impl WorkCriticalPathProjectionV1 {
    pub const fn graph_version(&self) -> WorkGraphVersionV1 {
        self.graph_version
    }

    pub fn task_ids(&self) -> &[TaskId] {
        &self.task_ids
    }

    pub const fn total_effort(&self) -> u32 {
        self.total_effort
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkWorkloadProjectionV1 {
    graph_version: WorkGraphVersionV1,
    total_effort: u32,
    ready_effort: Option<u32>,
    running_effort: Option<u32>,
    blocked_effort: Option<u32>,
    requested_concurrency: Option<u32>,
    actual_concurrency: Option<u32>,
}

impl WorkWorkloadProjectionV1 {
    pub const fn graph_version(&self) -> WorkGraphVersionV1 {
        self.graph_version
    }

    pub const fn total_effort(&self) -> u32 {
        self.total_effort
    }

    pub const fn ready_effort(&self) -> Option<u32> {
        self.ready_effort
    }

    pub const fn running_effort(&self) -> Option<u32> {
        self.running_effort
    }

    pub const fn blocked_effort(&self) -> Option<u32> {
        self.blocked_effort
    }

    pub const fn requested_concurrency(&self) -> Option<u32> {
        self.requested_concurrency
    }

    pub const fn actual_concurrency(&self) -> Option<u32> {
        self.actual_concurrency
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductProjectionBundleV1 {
    graph_version: WorkGraphVersionV1,
    runtime: WorkRuntimeProjectionV1,
    kanban: WorkKanbanProjectionV1,
    dag: WorkDagProjectionV1,
    timeline: WorkTimelineProjectionV1,
    causal: WorkCausalProjectionV1,
    critical_path: WorkCriticalPathProjectionV1,
    workload: WorkWorkloadProjectionV1,
}

impl WorkProductProjectionBundleV1 {
    pub fn from_graph(
        graph: &WorkProductGraphV1,
        runtime: &WorkRuntimeProjectionV1,
        observed_at: UtcMicros,
    ) -> Result<Self, WorkProductContractError> {
        graph.validate()?;
        runtime.validate(graph, observed_at)?;
        let accepted = graph
            .items()
            .iter()
            .filter(|item| item.is_accepted())
            .map(WorkItemV1::task_id)
            .collect::<BTreeSet<_>>();
        let runtime_by_task = runtime.attempts().iter().fold(
            BTreeMap::<TaskId, Vec<_>>::new(),
            |mut states, attempt| {
                states
                    .entry(attempt.identity.task_id().clone())
                    .or_default()
                    .push(attempt.state);
                states
            },
        );
        let unavailable_runtime_tasks = match runtime.coverage() {
            WorkRuntimeProjectionCoverageV1::Complete => BTreeSet::new(),
            WorkRuntimeProjectionCoverageV1::Partial {
                unavailable_attempts,
            } => unavailable_attempts
                .iter()
                .map(WorkAttemptIdentityV1::task_id)
                .cloned()
                .collect(),
            WorkRuntimeProjectionCoverageV1::Unavailable => graph
                .items()
                .iter()
                .filter(|item| !item.accepted_attempts().is_empty())
                .map(WorkItemV1::task_id)
                .cloned()
                .collect(),
        };
        let lane_by_task = graph
            .items()
            .iter()
            .map(|item| {
                (
                    item.task_id().clone(),
                    lane(
                        item,
                        &accepted,
                        runtime_by_task
                            .get(item.task_id())
                            .map(Vec::as_slice)
                            .unwrap_or_default(),
                        unavailable_runtime_tasks.contains(item.task_id()),
                        observed_at,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let cards = graph
            .items()
            .iter()
            .map(|item| WorkKanbanCardV1 {
                task_id: item.task_id().clone(),
                lane: lane_by_task[item.task_id()],
                effort: item.effort(),
                legal_actions: legal_actions(item),
            })
            .collect();
        let mut gating_edges = Vec::new();
        let mut causal_edges = Vec::new();
        for item in graph.items() {
            gating_edges.extend(item.dependencies().iter().map(|dependency| WorkDagEdgeV1 {
                dependency: dependency.clone(),
                dependent: item.task_id().clone(),
            }));
            causal_edges.extend(
                item.causal_candidates()
                    .iter()
                    .map(|candidate| WorkDagEdgeV1 {
                        dependency: candidate.clone(),
                        dependent: item.task_id().clone(),
                    }),
            );
        }
        gating_edges.sort_by(|left, right| {
            (&left.dependency, &left.dependent).cmp(&(&right.dependency, &right.dependent))
        });
        causal_edges.sort_by(|left, right| {
            (&left.dependency, &left.dependent).cmp(&(&right.dependency, &right.dependent))
        });
        let (critical_task_ids, critical_effort) = critical_path(graph)?;
        let total_effort = graph.items().iter().map(WorkItemV1::effort).sum();
        let runtime_complete = matches!(
            runtime.coverage(),
            WorkRuntimeProjectionCoverageV1::Complete
        );
        let ready_effort = runtime_complete.then(|| {
            graph
                .items()
                .iter()
                .filter(|item| lane_by_task[item.task_id()] == WorkTimelineLaneV1::Ready)
                .map(WorkItemV1::effort)
                .sum()
        });
        let running_effort = runtime_complete.then(|| {
            graph
                .items()
                .iter()
                .filter(|item| lane_by_task[item.task_id()] == WorkTimelineLaneV1::Running)
                .map(WorkItemV1::effort)
                .sum()
        });
        let blocked_effort = runtime_complete.then(|| {
            graph
                .items()
                .iter()
                .filter(|item| lane_by_task[item.task_id()] == WorkTimelineLaneV1::Blocked)
                .map(WorkItemV1::effort)
                .sum()
        });
        let requested_concurrency = runtime_complete
            .then(|| {
                u32::try_from(
                    graph
                        .items()
                        .iter()
                        .filter(|item| {
                            matches!(
                                lane_by_task[item.task_id()],
                                WorkTimelineLaneV1::Ready | WorkTimelineLaneV1::Running
                            )
                        })
                        .count(),
                )
            })
            .transpose()
            .map_err(|_| WorkProductContractError::GraphTooLarge)?;
        let actual_concurrency = runtime_complete
            .then(|| {
                u32::try_from(
                    runtime
                        .attempts()
                        .iter()
                        .filter(|attempt| runtime_attempt_is_running(attempt.state))
                        .count(),
                )
            })
            .transpose()
            .map_err(|_| WorkProductContractError::GraphTooLarge)?;
        let version = graph.version();
        Ok(Self {
            graph_version: version,
            runtime: runtime.clone(),
            kanban: WorkKanbanProjectionV1 {
                graph_version: version,
                cards,
            },
            dag: WorkDagProjectionV1 {
                graph_version: version,
                task_ids: graph
                    .items()
                    .iter()
                    .map(WorkItemV1::task_id)
                    .cloned()
                    .collect(),
                gating_edges,
            },
            timeline: WorkTimelineProjectionV1 {
                graph_version: version,
                entries: graph
                    .items()
                    .iter()
                    .map(|item| WorkTimelineEntryV1 {
                        task_id: item.task_id().clone(),
                        created_at: item.created_at(),
                        updated_at: item.updated_at(),
                        scheduled_at: item.scheduled_at(),
                        deadline: item.deadline(),
                    })
                    .collect(),
            },
            causal: WorkCausalProjectionV1 {
                graph_version: version,
                candidate_edges: causal_edges,
            },
            critical_path: WorkCriticalPathProjectionV1 {
                graph_version: version,
                task_ids: critical_task_ids,
                total_effort: critical_effort,
            },
            workload: WorkWorkloadProjectionV1 {
                graph_version: version,
                total_effort,
                ready_effort,
                running_effort,
                blocked_effort,
                requested_concurrency,
                actual_concurrency,
            },
        })
    }

    pub const fn graph_version(&self) -> WorkGraphVersionV1 {
        self.graph_version
    }

    pub const fn kanban(&self) -> &WorkKanbanProjectionV1 {
        &self.kanban
    }

    pub const fn runtime(&self) -> &WorkRuntimeProjectionV1 {
        &self.runtime
    }

    pub const fn dag(&self) -> &WorkDagProjectionV1 {
        &self.dag
    }

    pub const fn timeline(&self) -> &WorkTimelineProjectionV1 {
        &self.timeline
    }

    pub const fn causal(&self) -> &WorkCausalProjectionV1 {
        &self.causal
    }

    pub const fn critical_path(&self) -> &WorkCriticalPathProjectionV1 {
        &self.critical_path
    }

    pub const fn workload(&self) -> &WorkWorkloadProjectionV1 {
        &self.workload
    }
}

fn lane(
    item: &WorkItemV1,
    accepted: &BTreeSet<&TaskId>,
    runtime: &[WorkAttemptStateV1],
    runtime_unavailable: bool,
    now: UtcMicros,
) -> WorkTimelineLaneV1 {
    if item.is_archived() {
        return WorkTimelineLaneV1::Archived;
    }
    if item.is_accepted() {
        return WorkTimelineLaneV1::Done;
    }
    if runtime_unavailable {
        return WorkTimelineLaneV1::Unavailable;
    }
    if runtime
        .iter()
        .any(|state| runtime_attempt_is_running(*state))
    {
        return WorkTimelineLaneV1::Running;
    }
    if runtime.iter().any(|state| {
        matches!(
            state,
            WorkAttemptStateV1::Succeeded
                | WorkAttemptStateV1::Failed
                | WorkAttemptStateV1::TimedOut
        )
    }) {
        return WorkTimelineLaneV1::Review;
    }
    if runtime.contains(&WorkAttemptStateV1::RecoveryRequired) {
        return WorkTimelineLaneV1::Blocked;
    }
    if !item.accepted_attempts().is_empty()
        && runtime.len() == item.accepted_attempts().len()
        && runtime
            .iter()
            .all(|state| *state == WorkAttemptStateV1::Cancelled)
    {
        return WorkTimelineLaneV1::Cancelled;
    }
    if item
        .dependencies()
        .iter()
        .any(|dependency| !accepted.contains(dependency))
    {
        return WorkTimelineLaneV1::Blocked;
    }
    if item.scheduled_at().is_some_and(|scheduled| scheduled > now) {
        return WorkTimelineLaneV1::Scheduled;
    }
    if runtime.contains(&WorkAttemptStateV1::Leased) || item.accepted_proposal().is_some() {
        WorkTimelineLaneV1::Ready
    } else if item.acceptance_criteria().is_empty() {
        WorkTimelineLaneV1::Triage
    } else {
        WorkTimelineLaneV1::Todo
    }
}

const fn runtime_attempt_is_running(state: WorkAttemptStateV1) -> bool {
    matches!(
        state,
        WorkAttemptStateV1::Running
            | WorkAttemptStateV1::CancellationRequested
            | WorkAttemptStateV1::CancellationAcknowledged
            | WorkAttemptStateV1::CancellationEscalated
    )
}

fn legal_actions(item: &WorkItemV1) -> BTreeSet<WorkLegalActionV1> {
    let mut actions = BTreeSet::from([
        WorkLegalActionV1::ViewEvidence,
        WorkLegalActionV1::LinkAcceptedAttempt,
        WorkLegalActionV1::Handoff,
    ]);
    if item.is_accepted() {
        actions.insert(WorkLegalActionV1::Archive);
        return actions;
    }
    if item.accepted_proposal().is_none() {
        actions.insert(WorkLegalActionV1::GenerateProposal);
        actions.insert(WorkLegalActionV1::AcceptProposal);
    }
    if !item.evidence_links().is_empty() {
        actions.insert(WorkLegalActionV1::AcceptTask);
    }
    actions
}

fn critical_path(
    graph: &WorkProductGraphV1,
) -> Result<(Vec<TaskId>, u32), WorkProductContractError> {
    if graph.items().is_empty() {
        return Ok((Vec::new(), 0));
    }
    let mut remaining = graph
        .items()
        .iter()
        .map(|item| (item.task_id().clone(), item.dependencies().len()))
        .collect::<BTreeMap<_, _>>();
    let mut outgoing = BTreeMap::<TaskId, Vec<TaskId>>::new();
    for item in graph.items() {
        for dependency in item.dependencies() {
            outgoing
                .entry(dependency.clone())
                .or_default()
                .push(item.task_id().clone());
        }
    }
    let by_id = graph
        .items()
        .iter()
        .map(|item| (item.task_id(), item))
        .collect::<BTreeMap<_, _>>();
    let mut ready = remaining
        .iter()
        .filter_map(|(task_id, count)| (*count == 0).then_some(task_id.clone()))
        .collect::<BTreeSet<_>>();
    let mut best = BTreeMap::<TaskId, (u32, Vec<TaskId>)>::new();
    while let Some(task_id) = ready.pop_first() {
        let item = by_id
            .get(&task_id)
            .ok_or(WorkProductContractError::UnknownTask)?;
        let prefix = item
            .dependencies()
            .iter()
            .filter_map(|dependency| best.get(dependency))
            .max_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)))
            .cloned()
            .unwrap_or_default();
        let effort = prefix
            .0
            .checked_add(item.effort())
            .ok_or(WorkProductContractError::GraphTooLarge)?;
        let mut path = prefix.1;
        path.push(task_id.clone());
        best.insert(task_id.clone(), (effort, path));
        for dependent in outgoing.get(&task_id).into_iter().flatten() {
            let count = remaining
                .get_mut(dependent)
                .ok_or(WorkProductContractError::UnknownTask)?;
            *count -= 1;
            if *count == 0 {
                ready.insert(dependent.clone());
            }
        }
    }
    best.into_values()
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)))
        .ok_or(WorkProductContractError::UnknownTask)
        .map(|(effort, path)| (path, effort))
}
