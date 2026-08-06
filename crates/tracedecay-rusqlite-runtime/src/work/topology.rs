//! Grafeo-backed current Work dependency topology.
//!
//! Immutable Work events remain canonical in SQLite. This adapter stores only
//! the rebuildable current task nodes and their gating dependency edges.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use thiserror::Error;
use tracedecay_domain::{TaskId, WorkAuthority, WorkProjection, canonical_sha256};
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability,
    GraphEntity, GraphEntityId, GraphFormatVersion, GraphLabel, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphProperty, GraphPropertyName, GraphRelation, GraphRelationId,
    GraphRelationKind, GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration,
};

const GRAPH_FORMAT_VERSION: u32 = 2;
const MAX_DELTA_RELATIONS: usize = 100_000;
const TASK_PREFIX: &str = "work-task:";
const DEPENDENCY_KIND: &str = "work-depends-on";
const PROJECTION_PAYLOAD: &str = "work-projection-json";
const VERSION_PROPERTY: &str = "work-version";
const TASK_LABEL: &str = "work-task";
const REFERENCE_LABEL: &str = "work-task-reference";

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkTopologyError {
    #[error("Work topology projection was not found")]
    NotFound,
    #[error("Work dependency topology contains a cycle")]
    Cycle,
    #[error("Work topology projection is stale")]
    Stale,
    #[error("Work topology store requires reset")]
    ResetRequired,
    #[error("Work topology request was cancelled")]
    Cancelled,
    #[error("Work topology is unavailable")]
    Unavailable,
    #[error("Work topology is corrupt")]
    Corrupt,
}

#[derive(Clone)]
pub struct WorkGraphTopologyStore {
    database: GraphDb,
}

impl std::fmt::Debug for WorkGraphTopologyStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkGraphTopologyStore")
            .finish_non_exhaustive()
    }
}

impl WorkGraphTopologyStore {
    #[must_use]
    pub const fn new(database: GraphDb) -> Self {
        Self { database }
    }

    #[must_use]
    pub(crate) fn database(&self) -> GraphDb {
        self.database.clone()
    }

    pub fn memory() -> Result<Self, WorkTopologyError> {
        Self::open_location(GraphDbLocation::Memory, GraphDurability::Memory)
    }

    pub fn open(path: &Path) -> Result<Self, WorkTopologyError> {
        Self::open_location(
            GraphDbLocation::Persistent(path.to_path_buf()),
            GraphDurability::Sync,
        )
    }

    fn open_location(
        location: GraphDbLocation,
        durability: GraphDurability,
    ) -> Result<Self, WorkTopologyError> {
        let database = GraphDb::open(GraphDbOpenOptions {
            location,
            expected_format: GraphFormatVersion::new(GRAPH_FORMAT_VERSION)
                .map_err(map_graph_error)?,
            durability,
            cancellation: cancellation(),
        })
        .map_err(map_graph_error)?;
        Ok(Self::new(database))
    }

    pub fn publish(
        &self,
        projection: &WorkProjection,
    ) -> Result<GraphWatermark, WorkTopologyError> {
        self.publish_batch(std::slice::from_ref(projection))
    }

    pub fn publish_batch(
        &self,
        projections: &[WorkProjection],
    ) -> Result<GraphWatermark, WorkTopologyError> {
        let first = projections.first().ok_or(WorkTopologyError::Unavailable)?;
        if projections
            .iter()
            .any(|projection| projection.authority() != first.authority())
        {
            return Err(WorkTopologyError::Unavailable);
        }
        let namespace = namespace(first.authority())?;
        let graph_projection = graph_projection(first.authority())?;
        let task_label = GraphLabel::new(TASK_LABEL).map_err(map_graph_error)?;
        let dependency_kind = GraphRelationKind::new(DEPENDENCY_KIND).map_err(map_graph_error)?;
        let mut final_projections = BTreeMap::new();
        for projection in projections {
            final_projections.insert(projection.task_id().clone(), projection);
        }
        let overlay = final_projections
            .values()
            .map(|projection| {
                Ok((
                    work_task_entity_id(projection.task_id())?,
                    projection
                        .dependencies()
                        .iter()
                        .map(work_task_entity_id)
                        .collect::<Result<BTreeSet<_>, _>>()?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, WorkTopologyError>>()?;
        let requested_entities = overlay
            .keys()
            .chain(overlay.values().flatten())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let observed_entities = requested_entities
            .iter()
            .cloned()
            .zip(
                self.database
                    .projection_entities(
                        &namespace,
                        &graph_projection,
                        &requested_entities,
                        cancellation(),
                    )
                    .map_err(map_graph_error)?,
            )
            .collect::<BTreeMap<_, _>>();
        let mut changed_tasks = BTreeSet::new();
        for projection in final_projections.values() {
            let task = work_task_entity_id(projection.task_id())?;
            let existing = observed_entities
                .get(&task)
                .ok_or(WorkTopologyError::Corrupt)?
                .clone();
            if let Some(existing_entity) = existing.as_ref()
                && existing_entity.labels.contains(&task_label)
            {
                let existing_projection = projected_task(existing_entity)?;
                if existing_projection == **projection {
                    continue;
                }
                if existing_projection.version().get() >= projection.version().get() {
                    return Err(WorkTopologyError::Stale);
                }
            }
            changed_tasks.insert(task);
        }
        self.validate_overlay(&namespace, &graph_projection, &dependency_kind, &overlay)?;

        let changed_task_ids = changed_tasks.iter().cloned().collect::<Vec<_>>();
        let outgoing_relations = changed_task_ids
            .iter()
            .cloned()
            .zip(
                self.database
                    .outgoing_relation_ids(
                        &namespace,
                        &changed_task_ids,
                        &BTreeSet::from([dependency_kind.clone()]),
                        MAX_DELTA_RELATIONS,
                        cancellation(),
                    )
                    .map_err(map_graph_error)?,
            )
            .collect::<BTreeMap<_, _>>();
        let mut delete_relations = BTreeSet::new();
        let mut upsert_entities = BTreeMap::new();
        let mut upsert_relations = BTreeMap::new();
        for projection in final_projections.values() {
            let task = work_task_entity_id(projection.task_id())?;
            if !changed_tasks.contains(&task) {
                continue;
            }
            delete_relations.extend(
                outgoing_relations
                    .get(&task)
                    .ok_or(WorkTopologyError::Corrupt)?
                    .iter()
                    .cloned(),
            );
            upsert_entities.insert(task, projected_task_entity(projection)?);
            for dependency in projection.dependencies() {
                let dependency_entity = work_task_entity_id(dependency)?;
                if !overlay.contains_key(&dependency_entity)
                    && observed_entities
                        .get(&dependency_entity)
                        .ok_or(WorkTopologyError::Corrupt)?
                        .is_none()
                {
                    upsert_entities.insert(dependency_entity, reference_task_entity(dependency)?);
                }
                let relation = dependency_relation(projection.task_id(), dependency)?;
                delete_relations.remove(&relation.identity);
                upsert_relations.insert(relation.identity.clone(), relation);
            }
        }
        if upsert_entities.is_empty() && upsert_relations.is_empty() && delete_relations.is_empty()
        {
            return batch_watermark(projections);
        }
        let watermark = batch_watermark(projections)?;
        let mutations = delete_relations
            .into_iter()
            .map(GraphMutation::DeleteRelation)
            .chain(
                upsert_entities
                    .into_values()
                    .map(GraphMutation::UpsertEntity),
            )
            .chain(
                upsert_relations
                    .into_values()
                    .map(GraphMutation::UpsertRelation),
            )
            .collect();
        self.database
            .apply(
                GraphWriteBatch::new(
                    namespace,
                    graph_projection,
                    source_generation(first.authority())?,
                    watermark.clone(),
                    mutations,
                    cancellation(),
                )
                .map_err(map_graph_error)?,
            )
            .map_err(map_graph_error)?;
        Ok(watermark)
    }

    pub fn validate(&self, projection: &WorkProjection) -> Result<(), WorkTopologyError> {
        let namespace = namespace(projection.authority())?;
        let graph_projection = graph_projection(projection.authority())?;
        let dependency_kind = GraphRelationKind::new(DEPENDENCY_KIND).map_err(map_graph_error)?;
        let task = work_task_entity_id(projection.task_id())?;
        let overlay = BTreeMap::from([(
            task.clone(),
            projection
                .dependencies()
                .iter()
                .map(work_task_entity_id)
                .collect::<Result<BTreeSet<_>, _>>()?,
        )]);
        self.validate_overlay(&namespace, &graph_projection, &dependency_kind, &overlay)
    }

    fn validate_overlay(
        &self,
        namespace: &GraphNamespace,
        graph_projection: &GraphProjectionId,
        dependency_kind: &GraphRelationKind,
        overlay: &BTreeMap<GraphEntityId, BTreeSet<GraphEntityId>>,
    ) -> Result<(), WorkTopologyError> {
        let starts = overlay
            .values()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let reachable = starts
            .iter()
            .cloned()
            .zip(
                self.database
                    .reachable_entities(
                        namespace,
                        graph_projection,
                        &starts,
                        &BTreeSet::from([dependency_kind.clone()]),
                        overlay,
                        MAX_DELTA_RELATIONS,
                        cancellation(),
                    )
                    .map_err(map_graph_error)?,
            )
            .collect::<BTreeMap<_, _>>();
        for (task, dependencies) in overlay {
            for dependency in dependencies {
                if task == dependency
                    || reachable
                        .get(dependency)
                        .ok_or(WorkTopologyError::Corrupt)?
                        .contains(task)
                {
                    return Err(WorkTopologyError::Cycle);
                }
            }
        }
        Ok(())
    }

    pub fn projection(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<WorkProjection, WorkTopologyError> {
        self.database
            .projection_entity(
                &namespace(authority)?,
                &graph_projection(authority)?,
                &work_task_entity_id(task_id)?,
                cancellation(),
            )
            .map_err(map_graph_error)?
            .as_ref()
            .ok_or(WorkTopologyError::NotFound)
            .and_then(projected_task)
    }

    pub fn projection_page(
        &self,
        authority: &WorkAuthority,
        limit: usize,
    ) -> Result<(Vec<WorkProjection>, u64), WorkTopologyError> {
        let task_label = GraphLabel::new(TASK_LABEL).map_err(map_graph_error)?;
        let page = self
            .database
            .projection_entities_by_label(
                &namespace(authority)?,
                &graph_projection(authority)?,
                &task_label,
                limit,
                cancellation(),
            )
            .map_err(map_graph_error)?;
        let projections = page
            .entities
            .iter()
            .map(projected_task)
            .collect::<Result<Vec<_>, _>>()?;
        Ok((projections, page.total_entities))
    }
}

fn projected_task(entity: &GraphEntity) -> Result<WorkProjection, WorkTopologyError> {
    let payload = entity
        .properties
        .get(&GraphPropertyName::new(PROJECTION_PAYLOAD).map_err(map_graph_error)?)
        .ok_or(WorkTopologyError::Corrupt)?;
    let GraphProperty::String(payload) = payload else {
        return Err(WorkTopologyError::Corrupt);
    };
    serde_json::from_str(payload).map_err(|_| WorkTopologyError::Corrupt)
}

fn namespace(authority: &WorkAuthority) -> Result<GraphNamespace, WorkTopologyError> {
    GraphNamespace::new(format!("project:{}", authority.project_id().as_str()))
        .map_err(map_graph_error)
}

fn graph_projection(authority: &WorkAuthority) -> Result<GraphProjectionId, WorkTopologyError> {
    GraphProjectionId::new(format!(
        "work-topology:{}",
        authority
            .projection_generation_id()
            .map_err(|_| WorkTopologyError::Unavailable)?
            .as_str()
    ))
    .map_err(map_graph_error)
}

fn source_generation(authority: &WorkAuthority) -> Result<SourceGeneration, WorkTopologyError> {
    SourceGeneration::new(
        authority
            .projection_generation_id()
            .map_err(|_| WorkTopologyError::Unavailable)?
            .as_str(),
    )
    .map_err(map_graph_error)
}

fn batch_watermark(projections: &[WorkProjection]) -> Result<GraphWatermark, WorkTopologyError> {
    let digest = canonical_sha256(&projections).map_err(|_| WorkTopologyError::Unavailable)?;
    GraphWatermark::new(format!(
        "work-batch-watermark:{}:{}",
        projections.len(),
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(map_graph_error)
}

pub fn work_task_entity_id(task_id: &TaskId) -> Result<GraphEntityId, WorkTopologyError> {
    GraphEntityId::new(format!("{TASK_PREFIX}{}", task_id.as_str())).map_err(map_graph_error)
}

fn projected_task_entity(projection: &WorkProjection) -> Result<GraphEntity, WorkTopologyError> {
    GraphEntity::new(
        work_task_entity_id(projection.task_id())?,
        BTreeSet::from([
            GraphLabel::new(TASK_LABEL).map_err(map_graph_error)?,
            GraphLabel::new(REFERENCE_LABEL).map_err(map_graph_error)?,
        ]),
        BTreeMap::from([
            (
                GraphPropertyName::new(PROJECTION_PAYLOAD).map_err(map_graph_error)?,
                GraphProperty::String(
                    serde_json::to_string(projection).map_err(|_| WorkTopologyError::Corrupt)?,
                ),
            ),
            (
                GraphPropertyName::new(VERSION_PROPERTY).map_err(map_graph_error)?,
                GraphProperty::I64(
                    i64::try_from(projection.version().get())
                        .map_err(|_| WorkTopologyError::Unavailable)?,
                ),
            ),
        ]),
    )
    .map_err(map_graph_error)
}

fn reference_task_entity(task_id: &TaskId) -> Result<GraphEntity, WorkTopologyError> {
    GraphEntity::new(
        work_task_entity_id(task_id)?,
        BTreeSet::from([GraphLabel::new(REFERENCE_LABEL).map_err(map_graph_error)?]),
        BTreeMap::new(),
    )
    .map_err(map_graph_error)
}

fn dependency_relation(
    task_id: &TaskId,
    dependency: &TaskId,
) -> Result<GraphRelation, WorkTopologyError> {
    GraphRelation::new(
        GraphRelationId::new(format!(
            "work-dependency:{}:{}",
            task_id.as_str(),
            dependency.as_str()
        ))
        .map_err(map_graph_error)?,
        work_task_entity_id(task_id)?,
        work_task_entity_id(dependency)?,
        GraphRelationKind::new(DEPENDENCY_KIND).map_err(map_graph_error)?,
        BTreeMap::new(),
    )
    .map_err(map_graph_error)
}

fn cancellation() -> Arc<dyn GraphCancellation> {
    Arc::new(NeverCancelled)
}

fn map_graph_error(error: GraphDbError) -> WorkTopologyError {
    match error {
        GraphDbError::Cancelled => WorkTopologyError::Cancelled,
        GraphDbError::Conflict => WorkTopologyError::Stale,
        GraphDbError::ResetRequired { .. } => WorkTopologyError::ResetRequired,
        GraphDbError::Corrupt { .. } | GraphDbError::DurabilityUncertain { .. } => {
            WorkTopologyError::Corrupt
        }
        GraphDbError::InvalidRequest { .. }
        | GraphDbError::BudgetExhausted
        | GraphDbError::Unavailable { .. }
        | GraphDbError::Closed => WorkTopologyError::Unavailable,
    }
}
