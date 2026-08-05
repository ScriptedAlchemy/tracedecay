//! Deterministic Work views over one exact product graph version.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    TaskId, UtcMicros, WorkGraphVersionV1, WorkItemV1, WorkProductContractError,
    WorkProductGraphV1, WorkProviderAdmissionV1,
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
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkLegalActionV1 {
    ViewEvidence,
    GenerateProposal,
    AcceptProposal,
    AdmitProvider,
    RecordOutcome,
    CancelAttempt,
    RetryAttempt,
    RollbackAdmission,
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
    ready_effort: u32,
    running_effort: u32,
    blocked_effort: u32,
    requested_concurrency: u32,
    actual_concurrency: u32,
}

impl WorkWorkloadProjectionV1 {
    pub const fn graph_version(&self) -> WorkGraphVersionV1 {
        self.graph_version
    }

    pub const fn total_effort(&self) -> u32 {
        self.total_effort
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductProjectionBundleV1 {
    graph_version: WorkGraphVersionV1,
    kanban: WorkKanbanProjectionV1,
    dag: WorkDagProjectionV1,
    timeline: WorkTimelineProjectionV1,
    causal: WorkCausalProjectionV1,
    critical_path: WorkCriticalPathProjectionV1,
    workload: WorkWorkloadProjectionV1,
}

impl WorkProductProjectionBundleV1 {
    pub fn from_graph(graph: &WorkProductGraphV1) -> Result<Self, WorkProductContractError> {
        let accepted = graph
            .items()
            .iter()
            .filter(|item| item.is_accepted())
            .map(WorkItemV1::task_id)
            .collect::<BTreeSet<_>>();
        let lane_by_task = graph
            .items()
            .iter()
            .map(|item| {
                (
                    item.task_id().clone(),
                    lane(item, &accepted, UtcMicros(i64::MAX)),
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
                legal_actions: legal_actions(item, &accepted),
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
        let ready_effort = graph
            .items()
            .iter()
            .filter(|item| lane_by_task[item.task_id()] == WorkTimelineLaneV1::Ready)
            .map(WorkItemV1::effort)
            .sum();
        let running_effort = graph
            .items()
            .iter()
            .filter(|item| lane_by_task[item.task_id()] == WorkTimelineLaneV1::Running)
            .map(WorkItemV1::effort)
            .sum();
        let blocked_effort = graph
            .items()
            .iter()
            .filter(|item| lane_by_task[item.task_id()] == WorkTimelineLaneV1::Blocked)
            .map(WorkItemV1::effort)
            .sum();
        let requested_concurrency = u32::try_from(
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
        .map_err(|_| WorkProductContractError::GraphTooLarge)?;
        let actual_concurrency = u32::try_from(
            graph
                .items()
                .iter()
                .filter(|item| lane_by_task[item.task_id()] == WorkTimelineLaneV1::Running)
                .count(),
        )
        .map_err(|_| WorkProductContractError::GraphTooLarge)?;
        let version = graph.version();
        Ok(Self {
            graph_version: version,
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

fn lane(item: &WorkItemV1, accepted: &BTreeSet<&TaskId>, now: UtcMicros) -> WorkTimelineLaneV1 {
    if item.is_archived() {
        return WorkTimelineLaneV1::Archived;
    }
    if item.is_accepted() {
        return WorkTimelineLaneV1::Done;
    }
    if !item.provider_outcomes().is_empty() {
        return WorkTimelineLaneV1::Review;
    }
    match item.provider_admission() {
        Some(WorkProviderAdmissionV1::Admitted { .. }) => return WorkTimelineLaneV1::Running,
        Some(WorkProviderAdmissionV1::Cancelled { .. }) => return WorkTimelineLaneV1::Cancelled,
        Some(WorkProviderAdmissionV1::RolledBack { .. }) | None => {}
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
    if item.accepted_proposal().is_some() {
        WorkTimelineLaneV1::Ready
    } else if item.acceptance_criteria().is_empty() {
        WorkTimelineLaneV1::Triage
    } else {
        WorkTimelineLaneV1::Todo
    }
}

fn legal_actions(item: &WorkItemV1, accepted: &BTreeSet<&TaskId>) -> BTreeSet<WorkLegalActionV1> {
    let mut actions = BTreeSet::from([WorkLegalActionV1::ViewEvidence, WorkLegalActionV1::Handoff]);
    if item.is_accepted() {
        actions.insert(WorkLegalActionV1::Archive);
        return actions;
    }
    if item.provider_outcomes().is_empty() && item.provider_admission().is_none() {
        actions.insert(WorkLegalActionV1::GenerateProposal);
        actions.insert(WorkLegalActionV1::AcceptProposal);
    }
    if item.accepted_proposal().is_some()
        && item
            .dependencies()
            .iter()
            .all(|dependency| accepted.contains(dependency))
        && item.provider_admission().is_none()
    {
        actions.insert(WorkLegalActionV1::AdmitProvider);
    }
    match item.provider_admission() {
        Some(WorkProviderAdmissionV1::Admitted { .. }) if item.provider_outcomes().is_empty() => {
            actions.insert(WorkLegalActionV1::RecordOutcome);
            actions.insert(WorkLegalActionV1::CancelAttempt);
            actions.insert(WorkLegalActionV1::RollbackAdmission);
        }
        Some(WorkProviderAdmissionV1::Cancelled { .. }) => {
            actions.insert(WorkLegalActionV1::RetryAttempt);
        }
        _ if !item.provider_outcomes().is_empty() => {
            actions.insert(WorkLegalActionV1::RetryAttempt);
            actions.insert(WorkLegalActionV1::AcceptTask);
        }
        _ => {}
    }
    actions
}

fn critical_path(
    graph: &WorkProductGraphV1,
) -> Result<(Vec<TaskId>, u32), WorkProductContractError> {
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
