//! Immutable Work dependency projection over verified Grafeo generations.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    ManifestDigest, TaskId, WorkAuthority, WorkEvent, WorkProjection, WorkTopologyGenerationRefV1,
    canonical_sha256,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationId,
    GraphGenerationManifest, GraphGenerationRelation, GraphIdempotencyKey, GraphLabel,
    GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProjectorRevision,
    GraphProperty, GraphPropertyName, GraphRelationId, GraphRelationKind, GraphTraversalDirection,
    GraphWatermark, SourceGeneration, TraversalRequest, VerifiedGraphSnapshot,
};

const WORK_PROJECTION: &str = "work-topology";
const TASK_LABEL: &str = "WorkTask";
const BOUNDARY_TASK_LABEL: &str = "WorkBoundaryTask";
const TASK_ID_PROPERTY: &str = "task-id";
const TASK_RECORD_PROPERTY: &str = "task-record";
const DEPENDENCY_RELATION: &str = "WorkDependsOn";

pub const WORK_TOPOLOGY_PROJECTOR_REVISION_V1: &str = "work-topology-projector.v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkTopologyProjectionV1 {
    pub authority: WorkAuthority,
    pub event_watermark: ManifestDigest,
    pub event_count: u64,
    pub tasks: Vec<WorkProjection>,
}

impl WorkTopologyProjectionV1 {
    pub fn from_events(events: &[WorkEvent]) -> Result<Self, WorkTopologyError> {
        let authority = events
            .first()
            .map(WorkEvent::authority)
            .cloned()
            .ok_or(WorkTopologyError::EmptyEvents)?;
        let mut ordered = events.to_vec();
        ordered.sort_by(|left, right| {
            left.task_id()
                .cmp(right.task_id())
                .then(left.version().cmp(&right.version()))
        });
        if ordered.iter().any(|event| event.authority() != &authority) {
            return Err(WorkTopologyError::MixedAuthority);
        }
        let mut by_task = BTreeMap::<TaskId, Vec<WorkEvent>>::new();
        for event in &ordered {
            by_task
                .entry(event.task_id().clone())
                .or_default()
                .push(event.clone());
        }
        let tasks = by_task
            .values()
            .map(|history| WorkProjection::rebuild(history).map_err(Into::into))
            .collect::<Result<Vec<_>, WorkTopologyError>>()?;
        let event_watermark = canonical_sha256(&("tracedecay.work-topology.events.v1", &ordered))
            .map_err(|error| WorkTopologyError::Contract(error.to_string()))?;
        let event_count = u64::try_from(ordered.len())
            .map_err(|_| WorkTopologyError::Contract("Work event count overflowed".to_owned()))?;
        let projection = Self {
            authority,
            event_watermark,
            event_count,
            tasks,
        };
        projection.validate()?;
        Ok(projection)
    }

    pub fn validate(&self) -> Result<(), WorkTopologyError> {
        self.event_watermark
            .validate()
            .map_err(|error| WorkTopologyError::Contract(error.to_string()))?;
        if self.event_count == 0 || self.tasks.is_empty() {
            return Err(WorkTopologyError::EmptyEvents);
        }
        let mut prior = None;
        let task_ids = self
            .tasks
            .iter()
            .map(WorkProjection::task_id)
            .cloned()
            .collect::<BTreeSet<_>>();
        for task in &self.tasks {
            if task.authority() != &self.authority {
                return Err(WorkTopologyError::MixedAuthority);
            }
            if prior.as_ref().is_some_and(|prior| prior >= task.task_id()) {
                return Err(WorkTopologyError::NonCanonicalTasks);
            }
            prior = Some(task.task_id().clone());
        }
        validate_acyclic(&self.tasks, &task_ids)
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum WorkTopologyError {
    #[error("Work topology requires at least one event")]
    EmptyEvents,
    #[error("Work topology events mix authorities")]
    MixedAuthority,
    #[error("Work topology tasks are duplicated or out of order")]
    NonCanonicalTasks,
    #[error("Work gating dependencies contain a cycle at {0}")]
    DependencyCycle(TaskId),
    #[error("Work topology generation does not match")]
    GenerationMismatch,
    #[error("Work topology operation was cancelled")]
    Cancelled,
    #[error("Work topology traversal budget was exhausted")]
    BudgetExhausted,
    #[error("Work topology contract violation: {0}")]
    Contract(String),
    #[error("Work topology is unavailable: {0}")]
    Unavailable(String),
    #[error("Work topology is corrupt: {0}")]
    Corrupt(String),
}

impl From<tracedecay_domain::WorkContractError> for WorkTopologyError {
    fn from(error: tracedecay_domain::WorkContractError) -> Self {
        Self::Contract(error.to_string())
    }
}

impl From<GraphDbError> for WorkTopologyError {
    fn from(error: GraphDbError) -> Self {
        match error {
            GraphDbError::Cancelled => Self::Cancelled,
            GraphDbError::BudgetExhausted | GraphDbError::DeadlineExceeded => Self::BudgetExhausted,
            GraphDbError::InvalidRequest { message } => Self::Contract(message),
            GraphDbError::Corrupt { message }
            | GraphDbError::ResetRequired { message }
            | GraphDbError::DurabilityUncertain { message }
            | GraphDbError::ProjectionMismatch { message, .. }
            | GraphDbError::GenerationMismatch { message, .. } => Self::Corrupt(message),
            GraphDbError::Conflict => {
                Self::Unavailable("Work topology publication conflict".to_owned())
            }
            GraphDbError::Unavailable { message } => Self::Unavailable(message),
            GraphDbError::Closed => Self::Unavailable("graph store is closed".to_owned()),
        }
    }
}

pub fn work_topology_projection_identity(
    namespace: GraphNamespace,
) -> Result<GraphProjectionIdentity, WorkTopologyError> {
    Ok(GraphProjectionIdentity::new(
        namespace,
        GraphProjectionId::new(WORK_PROJECTION)?,
    ))
}

pub fn work_topology_generation_id(
    projection: &WorkTopologyProjectionV1,
    projector_revision: &GraphProjectorRevision,
) -> Result<GraphGenerationId, WorkTopologyError> {
    projection.validate()?;
    let digest = canonical_sha256(&(
        "tracedecay.work-topology-generation.v1",
        projection,
        projector_revision,
    ))
    .map_err(|error| WorkTopologyError::Contract(error.to_string()))?;
    GraphGenerationId::new(format!("work-topology:{}", digest.as_str())).map_err(Into::into)
}

pub fn work_topology_namespace(
    authority: &WorkAuthority,
) -> Result<GraphNamespace, WorkTopologyError> {
    let digest = canonical_sha256(&("tracedecay.work-topology-namespace.v1", authority))
        .map_err(|error| WorkTopologyError::Contract(error.to_string()))?;
    GraphNamespace::new(format!("work-topology:{}", digest.as_str())).map_err(Into::into)
}

pub fn work_topology_idempotency_key(
    projection: &WorkTopologyProjectionV1,
    projector_revision: &GraphProjectorRevision,
) -> Result<GraphIdempotencyKey, WorkTopologyError> {
    let generation = work_topology_generation_id(projection, projector_revision)?;
    GraphIdempotencyKey::new(format!("publish:{}", generation.as_str())).map_err(Into::into)
}

pub fn build_work_topology_manifest_checked(
    identity: GraphProjectionIdentity,
    projection: &WorkTopologyProjectionV1,
    projector_revision: &GraphProjectorRevision,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphGenerationManifest, WorkTopologyError> {
    check()?;
    projection.validate()?;
    if identity.projection.as_str() != WORK_PROJECTION {
        return Err(WorkTopologyError::Contract(
            "Work topology projection identity uses a foreign projector".to_owned(),
        ));
    }
    let tasks = projection
        .tasks
        .iter()
        .map(|task| (task.task_id().clone(), task))
        .collect::<BTreeMap<_, _>>();
    let mut task_ids = tasks.keys().cloned().collect::<BTreeSet<_>>();
    for task in tasks.values() {
        check()?;
        task_ids.extend(task.dependencies().iter().cloned());
    }

    let entities = task_ids
        .into_iter()
        .map(|task_id| task_entity(task_id.clone(), tasks.get(&task_id).copied()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut relations = Vec::new();
    for task in tasks.values() {
        for dependency in task.dependencies() {
            check()?;
            relations.push(dependency_relation(&identity, task.task_id(), dependency)?);
        }
    }
    let generation = work_topology_generation_id(projection, projector_revision)?;
    GraphGenerationManifest::new_checked(
        identity,
        generation,
        SourceGeneration::new(projection.event_watermark.as_str())?,
        GraphWatermark::new(projection.event_watermark.as_str())?,
        Vec::new(),
        entities,
        relations,
        check,
    )
    .map_err(Into::into)
}

#[derive(Clone)]
pub struct WorkTopologyStore {
    snapshot: Arc<VerifiedGraphSnapshot>,
    projection: GraphProjectionIdentity,
    generation: GraphGenerationId,
    task_ids: BTreeSet<TaskId>,
}

impl fmt::Debug for WorkTopologyStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkTopologyStore")
            .field("projection", &self.projection)
            .field("generation", &self.generation)
            .field("task_count", &self.task_ids.len())
            .finish_non_exhaustive()
    }
}

impl WorkTopologyStore {
    pub fn publish_from_events(
        events: &[WorkEvent],
        check: &dyn Fn() -> Result<(), GraphDbError>,
        publish: impl FnOnce(
            &GraphGenerationManifest,
            GraphIdempotencyKey,
        ) -> Result<VerifiedGraphSnapshot, GraphDbError>,
    ) -> Result<Self, WorkTopologyError> {
        check()?;
        let projection = WorkTopologyProjectionV1::from_events(events)?;
        let revision =
            GraphProjectorRevision::try_from(WORK_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())?;
        let identity =
            work_topology_projection_identity(work_topology_namespace(&projection.authority)?)?;
        let manifest =
            build_work_topology_manifest_checked(identity, &projection, &revision, check)?;
        let idempotency_key = work_topology_idempotency_key(&projection, &revision)?;
        let snapshot = publish(&manifest, idempotency_key)?;
        Self::from_verified_snapshot(snapshot, &projection)
    }

    pub fn from_verified_snapshot(
        snapshot: VerifiedGraphSnapshot,
        projection: &WorkTopologyProjectionV1,
    ) -> Result<Self, WorkTopologyError> {
        let revision =
            GraphProjectorRevision::try_from(WORK_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())?;
        let generation = work_topology_generation_id(projection, &revision)?;
        if snapshot.generation() != &generation {
            return Err(WorkTopologyError::GenerationMismatch);
        }
        Ok(Self {
            projection: snapshot.projection().clone(),
            generation,
            snapshot: Arc::new(snapshot),
            task_ids: projection
                .tasks
                .iter()
                .map(WorkProjection::task_id)
                .cloned()
                .collect(),
        })
    }

    /// The verified graph generation this topology snapshot is published
    /// under. Reads bound to this generation become stale when a newer
    /// generation is published.
    pub const fn generation(&self) -> &GraphGenerationId {
        &self.generation
    }

    /// Authority-bound evidence for the exact mounted topology projection and
    /// verified graph generation. The graph store remains the generation
    /// authority; callers receive only this opaque digest for later staleness
    /// checks.
    pub fn evidence_ref(&self) -> Result<WorkTopologyGenerationRefV1, WorkTopologyError> {
        let digest = canonical_sha256(&(
            "tracedecay.work-topology-generation-evidence.v1",
            &self.projection,
            &self.generation,
        ))
        .map_err(|error| WorkTopologyError::Contract(error.to_string()))?;
        WorkTopologyGenerationRefV1::new(digest.as_str().to_owned())
            .map_err(|error| WorkTopologyError::Contract(error.to_string()))
    }

    /// The number of tasks in the verified topology.
    pub fn task_count(&self) -> usize {
        self.task_ids.len()
    }

    pub fn projection(
        &self,
        task_id: &TaskId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<WorkProjection>, WorkTopologyError> {
        task_id
            .validate()
            .map_err(|error| WorkTopologyError::Contract(error.to_string()))?;
        let reference = GraphEntityRef::new(self.projection.clone(), task_entity_id(task_id)?);
        let Some(entity) = self.snapshot.entity(&reference, cancellation)? else {
            return Ok(None);
        };
        deserialize_optional_record(&entity)
    }

    pub fn blockers(
        &self,
        task_id: &TaskId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<BTreeSet<TaskId>, WorkTopologyError> {
        let Some(task) = self.projection(task_id, Arc::clone(&cancellation))? else {
            return Ok(BTreeSet::from([task_id.clone()]));
        };
        let mut blockers = BTreeSet::new();
        for dependency in task.dependencies() {
            check_cancelled(cancellation.as_ref())?;
            match self.projection(dependency, Arc::clone(&cancellation))? {
                Some(projection) if projection.is_task_accepted() => {}
                _ => {
                    blockers.insert(dependency.clone());
                }
            }
        }
        Ok(blockers)
    }

    pub fn dependency_closure(
        &self,
        task_id: &TaskId,
        max_depth: usize,
        max_results: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<TaskId>, WorkTopologyError> {
        validate_budget(max_depth, max_results)?;
        let result = self.snapshot.traverse(TraversalRequest {
            namespace: self.projection.namespace.clone(),
            start: task_entity_id(task_id)?,
            relation_kinds: BTreeSet::from([GraphRelationKind::new(DEPENDENCY_RELATION)?]),
            direction: GraphTraversalDirection::Outgoing,
            max_depth,
            max_visits: max_results.saturating_add(1),
            max_results,
            cancellation: Arc::clone(&cancellation),
        })?;
        result
            .visits
            .into_iter()
            .map(|visit| task_id_from_ref(&self.snapshot, &visit.entity, Arc::clone(&cancellation)))
            .filter(|result| result.as_ref() != Ok(task_id))
            .collect()
    }

    pub fn topological_order(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<TaskId>, WorkTopologyError> {
        let projections = self.read_all(Arc::clone(&cancellation))?;
        let mut remaining = projections
            .iter()
            .map(|(task_id, projection)| {
                let dependencies = projection
                    .dependencies()
                    .iter()
                    .filter(|dependency| projections.contains_key(*dependency))
                    .cloned()
                    .collect::<BTreeSet<_>>();
                (task_id.clone(), dependencies)
            })
            .collect::<BTreeMap<_, _>>();
        let mut ordered = Vec::with_capacity(remaining.len());
        while !remaining.is_empty() {
            check_cancelled(cancellation.as_ref())?;
            let ready = remaining
                .iter()
                .filter(|(_, dependencies)| dependencies.is_empty())
                .map(|(task_id, _)| task_id.clone())
                .collect::<Vec<_>>();
            if ready.is_empty() {
                let task =
                    remaining.keys().next().cloned().ok_or_else(|| {
                        WorkTopologyError::Corrupt("missing cycle node".to_owned())
                    })?;
                return Err(WorkTopologyError::DependencyCycle(task));
            }
            for task_id in ready {
                remaining.remove(&task_id);
                for dependencies in remaining.values_mut() {
                    dependencies.remove(&task_id);
                }
                ordered.push(task_id);
            }
        }
        Ok(ordered)
    }

    pub fn critical_path(
        &self,
        task_id: &TaskId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<TaskId>, WorkTopologyError> {
        let projections = self.read_all(Arc::clone(&cancellation))?;
        let mut memo = BTreeMap::new();
        longest_dependency_path(task_id, &projections, &mut memo, cancellation.as_ref())
    }

    fn read_all(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<BTreeMap<TaskId, WorkProjection>, WorkTopologyError> {
        let mut projections = BTreeMap::new();
        for task_id in &self.task_ids {
            check_cancelled(cancellation.as_ref())?;
            let projection = self
                .projection(task_id, Arc::clone(&cancellation))?
                .ok_or_else(|| {
                    WorkTopologyError::Corrupt(
                        "verified Work topology is missing a projected task".to_owned(),
                    )
                })?;
            projections.insert(task_id.clone(), projection);
        }
        Ok(projections)
    }
}

fn validate_acyclic(
    tasks: &[WorkProjection],
    task_ids: &BTreeSet<TaskId>,
) -> Result<(), WorkTopologyError> {
    let dependencies = tasks
        .iter()
        .map(|task| {
            (
                task.task_id().clone(),
                task.dependencies()
                    .iter()
                    .filter(|dependency| task_ids.contains(*dependency))
                    .cloned()
                    .collect::<BTreeSet<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut active = BTreeSet::new();
    let mut complete = BTreeSet::new();
    for task_id in dependencies.keys() {
        visit_acyclic(task_id, &dependencies, &mut active, &mut complete)?;
    }
    Ok(())
}

fn visit_acyclic(
    task_id: &TaskId,
    dependencies: &BTreeMap<TaskId, BTreeSet<TaskId>>,
    active: &mut BTreeSet<TaskId>,
    complete: &mut BTreeSet<TaskId>,
) -> Result<(), WorkTopologyError> {
    if complete.contains(task_id) {
        return Ok(());
    }
    if !active.insert(task_id.clone()) {
        return Err(WorkTopologyError::DependencyCycle(task_id.clone()));
    }
    for dependency in dependencies.get(task_id).into_iter().flatten() {
        visit_acyclic(dependency, dependencies, active, complete)?;
    }
    active.remove(task_id);
    complete.insert(task_id.clone());
    Ok(())
}

fn longest_dependency_path(
    task_id: &TaskId,
    projections: &BTreeMap<TaskId, WorkProjection>,
    memo: &mut BTreeMap<TaskId, Vec<TaskId>>,
    cancellation: &dyn GraphCancellation,
) -> Result<Vec<TaskId>, WorkTopologyError> {
    check_cancelled(cancellation)?;
    if let Some(path) = memo.get(task_id) {
        return Ok(path.clone());
    }
    let mut best = Vec::new();
    if let Some(projection) = projections.get(task_id) {
        for dependency in projection.dependencies() {
            let candidate = longest_dependency_path(dependency, projections, memo, cancellation)?;
            if candidate.len() > best.len() || (candidate.len() == best.len() && candidate < best) {
                best = candidate;
            }
        }
    }
    best.push(task_id.clone());
    memo.insert(task_id.clone(), best.clone());
    Ok(best)
}

fn task_entity(
    task_id: TaskId,
    projection: Option<&WorkProjection>,
) -> Result<GraphEntity, WorkTopologyError> {
    let mut labels = BTreeSet::from([GraphLabel::new(TASK_LABEL)?]);
    let mut properties = BTreeMap::from([(
        GraphPropertyName::new(TASK_ID_PROPERTY)?,
        GraphProperty::String(task_id.as_str().to_owned()),
    )]);
    if let Some(projection) = projection {
        properties.insert(
            GraphPropertyName::new(TASK_RECORD_PROPERTY)?,
            GraphProperty::Bytes(serialize(projection)?),
        );
    } else {
        labels.insert(GraphLabel::new(BOUNDARY_TASK_LABEL)?);
    }
    GraphEntity::new(task_entity_id(&task_id)?, labels, properties).map_err(Into::into)
}

fn dependency_relation(
    projection: &GraphProjectionIdentity,
    task_id: &TaskId,
    dependency: &TaskId,
) -> Result<GraphGenerationRelation, WorkTopologyError> {
    GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity(
            "dependency",
            &format!("{}\0{}", task_id.as_str(), dependency.as_str()),
        ))?,
        GraphEntityRef::new(projection.clone(), task_entity_id(task_id)?),
        GraphEntityRef::new(projection.clone(), task_entity_id(dependency)?),
        GraphRelationKind::new(DEPENDENCY_RELATION)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn deserialize_optional_record(
    entity: &GraphEntity,
) -> Result<Option<WorkProjection>, WorkTopologyError> {
    let Some(property) = entity
        .properties
        .get(&GraphPropertyName::new(TASK_RECORD_PROPERTY)?)
    else {
        return Ok(None);
    };
    let GraphProperty::Bytes(bytes) = property else {
        return Err(WorkTopologyError::Corrupt(
            "Work task record has the wrong property type".to_owned(),
        ));
    };
    serde_json::from_slice(bytes)
        .map(Some)
        .map_err(|error| WorkTopologyError::Corrupt(error.to_string()))
}

fn task_id_from_ref(
    snapshot: &VerifiedGraphSnapshot,
    reference: &GraphEntityRef,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<TaskId, WorkTopologyError> {
    let entity = snapshot.entity(reference, cancellation)?.ok_or_else(|| {
        WorkTopologyError::Corrupt("Work traversal reached a missing task".to_owned())
    })?;
    let property = entity
        .properties
        .get(&GraphPropertyName::new(TASK_ID_PROPERTY)?)
        .ok_or_else(|| WorkTopologyError::Corrupt("Work task ID is missing".to_owned()))?;
    let GraphProperty::String(value) = property else {
        return Err(WorkTopologyError::Corrupt(
            "Work task ID has the wrong property type".to_owned(),
        ));
    };
    TaskId::new(value.clone()).map_err(|error| WorkTopologyError::Corrupt(error.to_string()))
}

fn task_entity_id(task_id: &TaskId) -> Result<GraphEntityId, WorkTopologyError> {
    GraphEntityId::new(stable_identity("task", task_id.as_str())).map_err(Into::into)
}

fn validate_budget(max_depth: usize, max_results: usize) -> Result<(), WorkTopologyError> {
    if max_depth == 0 || max_results == 0 {
        Err(WorkTopologyError::Contract(
            "Work topology traversal bounds must be positive".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn check_cancelled(cancellation: &dyn GraphCancellation) -> Result<(), WorkTopologyError> {
    if cancellation.is_cancelled() {
        Err(WorkTopologyError::Cancelled)
    } else {
        Ok(())
    }
}

fn stable_identity(kind: &str, value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(kind.as_bytes());
    digest.update([0]);
    digest.update(value.as_bytes());
    format!("{kind}:{}", hex::encode(digest.finalize()))
}

fn serialize(value: &impl Serialize) -> Result<Vec<u8>, WorkTopologyError> {
    serde_json::to_vec(value).map_err(|error| WorkTopologyError::Contract(error.to_string()))
}
