//! Immutable workflow-definition DAG projection over verified Grafeo generations.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde::Serialize;
use thiserror::Error;
use tracedecay_domain::{WorkflowDefinition, WorkflowStep, WorkflowStepId, canonical_sha256};
use tracedecay_graph_db::graph_stable_identity as stable_identity;
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationId,
    GraphGenerationManifest, GraphGenerationRelation, GraphIdempotencyKey, GraphLabel,
    GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProjectorRevision,
    GraphProperty, GraphPropertyName, GraphRelationId, GraphRelationKind, GraphStoreFailureClass,
    GraphWatermark, SourceGeneration, VerifiedGraphSnapshot, classify_graph_store_error,
};

const WORKFLOW_PROJECTION: &str = "workflow-topology";
const STEP_LABEL: &str = "WorkflowStep";
const STEP_ID_PROPERTY: &str = "step-id";
const STEP_RECORD_PROPERTY: &str = "step-record";
const PRECEDES_RELATION: &str = "WorkflowPrecedes";

pub const WORKFLOW_TOPOLOGY_PROJECTOR_REVISION_V1: &str = "workflow-topology-projector.v1";

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum WorkflowTopologyError {
    #[error("Workflow topology contract violation: {0}")]
    Contract(String),
    #[error("Workflow topology generation does not match")]
    GenerationMismatch,
    #[error("Workflow topology operation was cancelled")]
    Cancelled,
    #[error("Workflow topology traversal budget was exhausted")]
    BudgetExhausted,
    #[error("Workflow topology is unavailable: {0}")]
    Unavailable(String),
    #[error("Workflow topology is corrupt: {0}")]
    Corrupt(String),
}

impl From<GraphDbError> for WorkflowTopologyError {
    fn from(error: GraphDbError) -> Self {
        match classify_graph_store_error(error, "workflow topology publication conflict") {
            GraphStoreFailureClass::Cancelled => Self::Cancelled,
            GraphStoreFailureClass::BudgetExhausted => Self::BudgetExhausted,
            GraphStoreFailureClass::Contract(message) => Self::Contract(message),
            GraphStoreFailureClass::Corrupt(message) => Self::Corrupt(message),
            GraphStoreFailureClass::Unavailable(message) => Self::Unavailable(message),
        }
    }
}

pub fn workflow_topology_projection_identity(
    namespace: GraphNamespace,
) -> Result<GraphProjectionIdentity, WorkflowTopologyError> {
    Ok(GraphProjectionIdentity::new(
        namespace,
        GraphProjectionId::new(WORKFLOW_PROJECTION)?,
    ))
}

pub fn workflow_topology_generation_id(
    definition: &WorkflowDefinition,
    projector_revision: &GraphProjectorRevision,
) -> Result<GraphGenerationId, WorkflowTopologyError> {
    definition
        .validate()
        .map_err(|error| WorkflowTopologyError::Contract(error.to_string()))?;
    let digest = canonical_sha256(&(
        "tracedecay.workflow-topology-generation.v1",
        definition,
        projector_revision,
    ))
    .map_err(|error| WorkflowTopologyError::Contract(error.to_string()))?;
    GraphGenerationId::new(format!("workflow-topology:{}", digest.as_str())).map_err(Into::into)
}

pub fn workflow_topology_namespace(
    definition: &WorkflowDefinition,
) -> Result<GraphNamespace, WorkflowTopologyError> {
    definition
        .validate()
        .map_err(|error| WorkflowTopologyError::Contract(error.to_string()))?;
    let digest = canonical_sha256(&(
        "tracedecay.workflow-topology-namespace.v1",
        definition.project_id(),
        definition.definition_id(),
    ))
    .map_err(|error| WorkflowTopologyError::Contract(error.to_string()))?;
    GraphNamespace::new(format!("workflow-topology:{}", digest.as_str())).map_err(Into::into)
}

pub fn workflow_topology_idempotency_key(
    definition: &WorkflowDefinition,
    projector_revision: &GraphProjectorRevision,
) -> Result<GraphIdempotencyKey, WorkflowTopologyError> {
    let generation = workflow_topology_generation_id(definition, projector_revision)?;
    GraphIdempotencyKey::new(format!("publish:{}", generation.as_str())).map_err(Into::into)
}

pub fn build_workflow_topology_manifest_checked(
    identity: GraphProjectionIdentity,
    definition: &WorkflowDefinition,
    projector_revision: &GraphProjectorRevision,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphGenerationManifest, WorkflowTopologyError> {
    check()?;
    definition
        .validate()
        .map_err(|error| WorkflowTopologyError::Contract(error.to_string()))?;
    if identity.projection.as_str() != WORKFLOW_PROJECTION {
        return Err(WorkflowTopologyError::Contract(
            "workflow topology identity uses a foreign projector".to_owned(),
        ));
    }
    let mut steps = definition.steps().iter().collect::<Vec<_>>();
    steps.sort_by(|left, right| left.step_id.cmp(&right.step_id));
    let entities = steps
        .iter()
        .map(|step| step_entity(step))
        .collect::<Result<Vec<_>, _>>()?;
    let mut relations = Vec::new();
    for step in &steps {
        for predecessor in &step.predecessors {
            check()?;
            relations.push(precedes_relation(&identity, predecessor, &step.step_id)?);
        }
    }
    let source_digest = canonical_sha256(&(
        "tracedecay.workflow-topology-source.v1",
        definition.definition_id(),
        definition.definition_version(),
        definition.project_id(),
        definition.pinned_policy_digest(),
        definition.pinned_configuration_digest(),
        definition.pinned_catalog_digest(),
    ))
    .map_err(|error| WorkflowTopologyError::Contract(error.to_string()))?;
    GraphGenerationManifest::new_checked(
        identity,
        workflow_topology_generation_id(definition, projector_revision)?,
        SourceGeneration::new(source_digest.as_str())?,
        GraphWatermark::new(source_digest.as_str())?,
        Vec::new(),
        entities,
        relations,
        check,
    )
    .map_err(Into::into)
}

#[derive(Clone)]
pub struct WorkflowTopologyStore {
    snapshot: Arc<VerifiedGraphSnapshot>,
    projection: GraphProjectionIdentity,
    generation: GraphGenerationId,
    step_ids: BTreeSet<WorkflowStepId>,
}

impl fmt::Debug for WorkflowTopologyStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkflowTopologyStore")
            .field("projection", &self.projection)
            .field("generation", &self.generation)
            .field("step_count", &self.step_ids.len())
            .finish_non_exhaustive()
    }
}

impl WorkflowTopologyStore {
    #[tracing::instrument(name = "runtime_core.workflow.publish", level = "trace", skip_all)]
    pub fn publish_from_definition(
        definition: &WorkflowDefinition,
        check: &dyn Fn() -> Result<(), GraphDbError>,
        publish: impl FnOnce(
            &GraphGenerationManifest,
            GraphIdempotencyKey,
        ) -> Result<VerifiedGraphSnapshot, GraphDbError>,
    ) -> Result<Self, WorkflowTopologyError> {
        check()?;
        let revision =
            GraphProjectorRevision::try_from(WORKFLOW_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())?;
        let identity =
            workflow_topology_projection_identity(workflow_topology_namespace(definition)?)?;
        let manifest =
            build_workflow_topology_manifest_checked(identity, definition, &revision, check)?;
        let idempotency_key = workflow_topology_idempotency_key(definition, &revision)?;
        let snapshot = publish(&manifest, idempotency_key)?;
        Self::from_verified_snapshot(snapshot, definition)
    }

    pub fn from_verified_snapshot(
        snapshot: VerifiedGraphSnapshot,
        definition: &WorkflowDefinition,
    ) -> Result<Self, WorkflowTopologyError> {
        let revision =
            GraphProjectorRevision::try_from(WORKFLOW_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())?;
        let generation = workflow_topology_generation_id(definition, &revision)?;
        if snapshot.generation() != &generation {
            return Err(WorkflowTopologyError::GenerationMismatch);
        }
        Ok(Self {
            projection: snapshot.projection().clone(),
            generation,
            snapshot: Arc::new(snapshot),
            step_ids: definition
                .steps()
                .iter()
                .map(|step| step.step_id.clone())
                .collect(),
        })
    }

    pub fn step(
        &self,
        step_id: &WorkflowStepId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<WorkflowStep>, WorkflowTopologyError> {
        let reference = GraphEntityRef::new(self.projection.clone(), step_entity_id(step_id)?);
        let Some(entity) = self.snapshot.entity(&reference, cancellation)? else {
            return Ok(None);
        };
        let property = entity
            .properties
            .get(&GraphPropertyName::new(STEP_RECORD_PROPERTY)?)
            .ok_or_else(|| {
                WorkflowTopologyError::Corrupt(
                    "workflow step is missing its immutable record".to_owned(),
                )
            })?;
        let GraphProperty::Bytes(bytes) = property else {
            return Err(WorkflowTopologyError::Corrupt(
                "workflow step record has the wrong property type".to_owned(),
            ));
        };
        serde_json::from_slice(bytes)
            .map(Some)
            .map_err(|error| WorkflowTopologyError::Corrupt(error.to_string()))
    }

    pub fn ready_steps(
        &self,
        completed: &BTreeSet<WorkflowStepId>,
        max_results: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<WorkflowStepId>, WorkflowTopologyError> {
        if max_results == 0 {
            return Err(WorkflowTopologyError::Contract(
                "workflow ready-step bound must be positive".to_owned(),
            ));
        }
        let mut ready = Vec::new();
        for step_id in &self.step_ids {
            check_cancelled(cancellation.as_ref())?;
            if completed.contains(step_id) {
                continue;
            }
            let step = self
                .step(step_id, Arc::clone(&cancellation))?
                .ok_or_else(|| {
                    WorkflowTopologyError::Corrupt(
                        "verified workflow topology is missing a step".to_owned(),
                    )
                })?;
            if step.predecessors.is_subset(completed) {
                ready.push(step_id.clone());
                if ready.len() == max_results {
                    break;
                }
            }
        }
        Ok(ready)
    }
}

fn step_entity(step: &WorkflowStep) -> Result<GraphEntity, WorkflowTopologyError> {
    GraphEntity::new(
        step_entity_id(&step.step_id)?,
        BTreeSet::from([GraphLabel::new(STEP_LABEL)?]),
        BTreeMap::from([
            (
                GraphPropertyName::new(STEP_ID_PROPERTY)?,
                GraphProperty::String(step.step_id.as_str().to_owned()),
            ),
            (
                GraphPropertyName::new(STEP_RECORD_PROPERTY)?,
                GraphProperty::Bytes(serialize(step)?),
            ),
        ]),
    )
    .map_err(Into::into)
}

fn precedes_relation(
    projection: &GraphProjectionIdentity,
    predecessor: &WorkflowStepId,
    successor: &WorkflowStepId,
) -> Result<GraphGenerationRelation, WorkflowTopologyError> {
    GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity(
            "precedes",
            &format!("{}\0{}", predecessor.as_str(), successor.as_str()),
        ))?,
        GraphEntityRef::new(projection.clone(), step_entity_id(predecessor)?),
        GraphEntityRef::new(projection.clone(), step_entity_id(successor)?),
        GraphRelationKind::new(PRECEDES_RELATION)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn step_entity_id(step_id: &WorkflowStepId) -> Result<GraphEntityId, WorkflowTopologyError> {
    GraphEntityId::new(stable_identity("step", step_id.as_str())).map_err(Into::into)
}

fn check_cancelled(cancellation: &dyn GraphCancellation) -> Result<(), WorkflowTopologyError> {
    if cancellation.is_cancelled() {
        Err(WorkflowTopologyError::Cancelled)
    } else {
        Ok(())
    }
}

fn serialize(value: &impl Serialize) -> Result<Vec<u8>, WorkflowTopologyError> {
    serde_json::to_vec(value).map_err(|error| WorkflowTopologyError::Contract(error.to_string()))
}
