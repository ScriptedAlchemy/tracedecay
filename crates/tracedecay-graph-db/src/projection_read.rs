use std::fmt;
use std::sync::Arc;

use crate::state::StateCache;
use crate::{
    GraphCancellation, GraphDb, GraphDbError, GraphEntity, GraphEntityId, GraphNamespace,
    GraphProjectionId, GraphRelation, GraphRelationId, GraphSnapshot, GraphWatermark,
    SourceGeneration,
};

#[derive(Clone)]
pub struct GraphProjectionReadRequest {
    pub namespace: GraphNamespace,
    pub projection: GraphProjectionId,
    pub after_entity: Option<GraphEntityId>,
    pub after_relation: Option<GraphRelationId>,
    pub max_entities: usize,
    pub max_relations: usize,
    pub cancellation: Arc<dyn GraphCancellation>,
}

impl fmt::Debug for GraphProjectionReadRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphProjectionReadRequest")
            .field("namespace", &self.namespace)
            .field("projection", &self.projection)
            .field("after_entity", &self.after_entity)
            .field("after_relation", &self.after_relation)
            .field("max_entities", &self.max_entities)
            .field("max_relations", &self.max_relations)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GraphProjectionPage {
    pub entities: Vec<GraphEntity>,
    pub relations: Vec<GraphRelation>,
    pub next_entity: Option<GraphEntityId>,
    pub next_relation: Option<GraphRelationId>,
}

#[derive(Clone)]
pub struct GraphProjectionTelemetryRequest {
    pub namespace: GraphNamespace,
    pub projection: GraphProjectionId,
    pub cancellation: Arc<dyn GraphCancellation>,
}

impl fmt::Debug for GraphProjectionTelemetryRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphProjectionTelemetryRequest")
            .field("namespace", &self.namespace)
            .field("projection", &self.projection)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphProjectionTelemetry {
    pub source_generation: SourceGeneration,
    pub watermark: GraphWatermark,
    pub commit_sequence: u64,
    pub entity_count: u64,
    pub relation_count: u64,
}

impl GraphDb {
    pub fn read_projection(
        &self,
        request: GraphProjectionReadRequest,
    ) -> Result<GraphProjectionPage, GraphDbError> {
        let state = self.point_read_state(request.cancellation.as_ref())?;
        read_projection(&state, request)
    }

    pub fn projection_telemetry(
        &self,
        request: GraphProjectionTelemetryRequest,
    ) -> Result<Option<GraphProjectionTelemetry>, GraphDbError> {
        let state = self.point_read_state(request.cancellation.as_ref())?;
        projection_telemetry(&state, request)
    }
}

impl GraphSnapshot {
    pub fn read_projection(
        &self,
        request: GraphProjectionReadRequest,
    ) -> Result<GraphProjectionPage, GraphDbError> {
        read_projection(&self.state, request)
    }

    pub fn projection_telemetry(
        &self,
        request: GraphProjectionTelemetryRequest,
    ) -> Result<Option<GraphProjectionTelemetry>, GraphDbError> {
        projection_telemetry(&self.state, request)
    }
}

fn projection_telemetry(
    state: &StateCache,
    request: GraphProjectionTelemetryRequest,
) -> Result<Option<GraphProjectionTelemetry>, GraphDbError> {
    if request.cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    let Some(commit) = state.latest_commit(&request.namespace, &request.projection) else {
        return Ok(None);
    };
    let entity_count = state
        .entities
        .values()
        .filter(|(_, stored)| {
            stored.namespace == request.namespace && stored.projection == request.projection
        })
        .count();
    if request.cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    let relation_count = state
        .relations
        .values()
        .filter(|(_, stored)| {
            stored.namespace == request.namespace && stored.projection == request.projection
        })
        .count();
    if request.cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    Ok(Some(GraphProjectionTelemetry {
        source_generation: commit.source_generation.clone(),
        watermark: commit.watermark.clone(),
        commit_sequence: commit.sequence,
        entity_count: u64::try_from(entity_count).unwrap_or(u64::MAX),
        relation_count: u64::try_from(relation_count).unwrap_or(u64::MAX),
    }))
}

fn read_projection(
    state: &StateCache,
    request: GraphProjectionReadRequest,
) -> Result<GraphProjectionPage, GraphDbError> {
    if request.cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    if request.max_entities == 0 && request.max_relations == 0 {
        return Err(GraphDbError::BudgetExhausted);
    }

    let (entities, next_entity) = if request.max_entities == 0 {
        (Vec::new(), None)
    } else {
        let mut entities = state
            .entities
            .values()
            .filter_map(|(_, stored)| {
                (stored.namespace == request.namespace
                    && stored.projection == request.projection
                    && request
                        .after_entity
                        .as_ref()
                        .is_none_or(|after| stored.entity.identity > *after))
                .then(|| stored.entity.clone())
            })
            .take(request.max_entities.saturating_add(1))
            .collect::<Vec<_>>();
        let next = (entities.len() > request.max_entities)
            .then(|| entities[request.max_entities - 1].identity.clone());
        entities.truncate(request.max_entities);
        (entities, next)
    };

    if request.cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    let (relations, next_relation) = if request.max_relations == 0 {
        (Vec::new(), None)
    } else {
        let mut relations = state
            .relations
            .values()
            .filter_map(|(_, stored)| {
                (stored.namespace == request.namespace
                    && stored.projection == request.projection
                    && request
                        .after_relation
                        .as_ref()
                        .is_none_or(|after| stored.relation.identity > *after))
                .then(|| stored.relation.clone())
            })
            .take(request.max_relations.saturating_add(1))
            .collect::<Vec<_>>();
        let next = (relations.len() > request.max_relations)
            .then(|| relations[request.max_relations - 1].identity.clone());
        relations.truncate(request.max_relations);
        (relations, next)
    };

    if request.cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    Ok(GraphProjectionPage {
        entities,
        relations,
        next_entity,
        next_relation,
    })
}
