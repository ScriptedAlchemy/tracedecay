//! Grafeo-backed workflow-definition DAG topology.
//!
//! SQLite retains immutable definition payloads, activation CAS, handoff
//! consumption, execution fences, checkpoints, and terminal receipts.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use thiserror::Error;
use tracedecay_domain::{
    WorkflowDefinitionId, WorkflowDefinitionV1, WorkflowStepId, canonical_sha256,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability,
    GraphEntity, GraphEntityId, GraphFormatVersion, GraphIdempotencyKey, GraphLabel, GraphMutation,
    GraphNamespace, GraphProjectionId, GraphPublication, GraphRelation, GraphRelationId,
    GraphRelationKind, GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration,
};

const GRAPH_FORMAT_VERSION: u32 = 2;
const STEP_PREFIX: &str = "workflow-step:";
const STEP_LABEL: &str = "workflow-step";
const PREDECESSOR_KIND: &str = "workflow-precedes";

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowTopologyError {
    #[error("Workflow topology contains a cycle")]
    Cycle,
    #[error("Workflow topology request was cancelled")]
    Cancelled,
    #[error("Workflow topology is unavailable: {0}")]
    Unavailable(String),
    #[error("Workflow topology is corrupt")]
    Corrupt,
}

#[derive(Clone)]
pub struct WorkflowGraphTopologyStore {
    database: GraphDb,
}

impl std::fmt::Debug for WorkflowGraphTopologyStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkflowGraphTopologyStore")
            .finish_non_exhaustive()
    }
}

impl WorkflowGraphTopologyStore {
    #[must_use]
    pub const fn new(database: GraphDb) -> Self {
        Self { database }
    }

    pub fn memory() -> Result<Self, WorkflowTopologyError> {
        GraphDb::open(GraphDbOpenOptions {
            location: GraphDbLocation::Memory,
            expected_format: GraphFormatVersion::new(GRAPH_FORMAT_VERSION)
                .map_err(map_graph_error)?,
            durability: GraphDurability::Memory,
            cancellation: cancellation(),
        })
        .map(Self::new)
        .map_err(map_graph_error)
    }

    pub fn publish_definition(
        &self,
        definition: &WorkflowDefinitionV1,
    ) -> Result<GraphWatermark, WorkflowTopologyError> {
        definition
            .validate()
            .map_err(|_| WorkflowTopologyError::Cycle)?;
        let namespace = namespace(definition.project_id().as_str())?;
        let projection = projection(definition.definition_id(), definition.definition_version())?;
        let entities = definition
            .steps()
            .iter()
            .map(|step| {
                step_entity(
                    definition.definition_id(),
                    definition.definition_version(),
                    &step.step_id,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut relations = Vec::new();
        for step in definition.steps() {
            for predecessor in &step.predecessors {
                relations.push(predecessor_relation(
                    definition.definition_id(),
                    definition.definition_version(),
                    predecessor,
                    &step.step_id,
                )?);
            }
        }
        let digest = canonical_sha256(definition)
            .map_err(|error| WorkflowTopologyError::Unavailable(error.to_string()))?;
        let source_generation = SourceGeneration::new(digest.as_str()).map_err(map_graph_error)?;
        let watermark = GraphWatermark::new(format!(
            "workflow-definition-watermark:{}:{}:{}",
            definition.definition_id().as_str(),
            definition.definition_version(),
            digest.as_str()
        ))
        .map_err(map_graph_error)?;
        let mutations = entities
            .into_iter()
            .map(GraphMutation::UpsertEntity)
            .chain(relations.into_iter().map(GraphMutation::UpsertRelation))
            .collect();
        let batch = GraphWriteBatch::new(
            namespace.clone(),
            projection,
            source_generation.clone(),
            watermark.clone(),
            mutations,
            cancellation(),
        )
        .map_err(map_graph_error)?;
        self.database
            .publish(GraphPublication {
                namespace,
                idempotency_key: GraphIdempotencyKey::new(format!(
                    "workflow-definition-publication:{}:{}:{}",
                    definition.definition_id().as_str(),
                    definition.definition_version(),
                    digest.as_str()
                ))
                .map_err(map_graph_error)?,
                source_generation,
                expected_watermark: None,
                next_watermark: watermark.clone(),
                batch,
                cancellation: cancellation(),
            })
            .map_err(map_graph_error)?;
        Ok(watermark)
    }
}

fn namespace(project: &str) -> Result<GraphNamespace, WorkflowTopologyError> {
    GraphNamespace::new(format!("project:{project}")).map_err(map_graph_error)
}

fn projection(
    definition_id: &WorkflowDefinitionId,
    definition_version: u64,
) -> Result<GraphProjectionId, WorkflowTopologyError> {
    GraphProjectionId::new(format!(
        "workflow-definition:{}:{definition_version}",
        definition_id.as_str()
    ))
    .map_err(map_graph_error)
}

fn step_entity(
    definition_id: &WorkflowDefinitionId,
    definition_version: u64,
    step_id: &WorkflowStepId,
) -> Result<GraphEntity, WorkflowTopologyError> {
    GraphEntity::new(
        step_entity_id(definition_id, definition_version, step_id)?,
        BTreeSet::from([GraphLabel::new(STEP_LABEL).map_err(map_graph_error)?]),
        BTreeMap::new(),
    )
    .map_err(map_graph_error)
}

fn step_entity_id(
    definition_id: &WorkflowDefinitionId,
    definition_version: u64,
    step_id: &WorkflowStepId,
) -> Result<GraphEntityId, WorkflowTopologyError> {
    GraphEntityId::new(format!(
        "{STEP_PREFIX}{}:{definition_version}:{}",
        definition_id.as_str(),
        step_id.as_str()
    ))
    .map_err(map_graph_error)
}

fn predecessor_relation(
    definition_id: &WorkflowDefinitionId,
    definition_version: u64,
    predecessor: &WorkflowStepId,
    step: &WorkflowStepId,
) -> Result<GraphRelation, WorkflowTopologyError> {
    GraphRelation::new(
        GraphRelationId::new(format!(
            "workflow-predecessor:{}:{definition_version}:{}:{}",
            definition_id.as_str(),
            predecessor.as_str(),
            step.as_str()
        ))
        .map_err(map_graph_error)?,
        step_entity_id(definition_id, definition_version, predecessor)?,
        step_entity_id(definition_id, definition_version, step)?,
        GraphRelationKind::new(PREDECESSOR_KIND).map_err(map_graph_error)?,
        BTreeMap::new(),
    )
    .map_err(map_graph_error)
}

fn cancellation() -> Arc<dyn GraphCancellation> {
    Arc::new(NeverCancelled)
}

fn map_graph_error(error: GraphDbError) -> WorkflowTopologyError {
    match error {
        GraphDbError::Cancelled => WorkflowTopologyError::Cancelled,
        GraphDbError::Corrupt { .. }
        | GraphDbError::ResetRequired { .. }
        | GraphDbError::DurabilityUncertain { .. } => WorkflowTopologyError::Corrupt,
        error @ (GraphDbError::Conflict
        | GraphDbError::InvalidRequest { .. }
        | GraphDbError::BudgetExhausted
        | GraphDbError::Unavailable { .. }
        | GraphDbError::Closed) => WorkflowTopologyError::Unavailable(error.to_string()),
    }
}
