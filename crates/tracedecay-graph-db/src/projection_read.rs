use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use grafeo_common::types::Value;
use grafeo_engine::GrafeoDB;

use crate::schema::{
    ENTITY_ID_PROPERTY, ENTITY_LABEL, RELATION_ID_PROPERTY, RELATION_LABEL,
    entity_projection_domain_label, entity_projection_label, relation_projection_label,
};
use crate::state::{latest_projection, load_entity, load_relation};
use crate::{
    GraphCancellation, GraphDb, GraphDbError, GraphEntity, GraphEntityId, GraphLabel,
    GraphNamespace, GraphProjectionId, GraphRelation, GraphRelationId, GraphSnapshot,
    GraphWatermark, SourceGeneration,
};

const MAX_PROJECTION_PAGE_ITEMS: usize = 100_000;

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

#[derive(Clone, Debug, PartialEq)]
pub struct GraphProjectionLabelPage {
    pub entities: Vec<GraphEntity>,
    pub total_entities: u64,
}

impl GraphDb {
    pub fn read_projection(
        &self,
        request: GraphProjectionReadRequest,
    ) -> Result<GraphProjectionPage, GraphDbError> {
        let guard = self.read_database(request.cancellation.as_ref())?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        read_projection(database, request)
    }

    pub fn projection_telemetry(
        &self,
        request: GraphProjectionTelemetryRequest,
    ) -> Result<Option<GraphProjectionTelemetry>, GraphDbError> {
        let guard = self.read_database(request.cancellation.as_ref())?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        projection_telemetry(database, request)
    }

    /// Reads one bounded exact-label page from a projection-scoped native
    /// label index. The total counts only entities carrying this exact label;
    /// reference-only nodes in the same projection are excluded.
    pub fn projection_entities_by_label(
        &self,
        namespace: &GraphNamespace,
        projection: &GraphProjectionId,
        label: &GraphLabel,
        limit: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<GraphProjectionLabelPage, GraphDbError> {
        validate_page_limit(limit)?;
        let guard = self.read_database(cancellation.as_ref())?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let owner_label = entity_projection_domain_label(namespace, projection, label);
        let total_entities =
            count_labeled_nodes(database, &owner_label, ENTITY_LABEL, cancellation.as_ref())?;
        let identities = query_identity_page(
            database,
            &owner_label,
            ENTITY_LABEL,
            ENTITY_ID_PROPERTY,
            None,
            limit,
            cancellation.as_ref(),
        )?;
        let mut entities = Vec::with_capacity(identities.len());
        for identity in identities {
            let identity = GraphEntityId::new(identity)
                .map_err(|error| persisted_identity_error("entity", error))?;
            let stored = load_entity(database, namespace, &identity)?.ok_or_else(|| {
                GraphDbError::Corrupt {
                    message: "projection label query returned an unindexed entity".to_owned(),
                }
            })?;
            if stored.projection != *projection || !stored.entity.labels.contains(label) {
                return Err(GraphDbError::Corrupt {
                    message: "projection label index does not match entity ownership".to_owned(),
                });
            }
            entities.push(stored.entity);
        }
        check_cancelled(cancellation.as_ref())?;
        Ok(GraphProjectionLabelPage {
            entities,
            total_entities,
        })
    }
}

impl GraphSnapshot {
    pub fn read_projection(
        &self,
        request: GraphProjectionReadRequest,
    ) -> Result<GraphProjectionPage, GraphDbError> {
        self.database.read_projection(request)
    }

    pub fn projection_telemetry(
        &self,
        request: GraphProjectionTelemetryRequest,
    ) -> Result<Option<GraphProjectionTelemetry>, GraphDbError> {
        self.database.projection_telemetry(request)
    }
}

fn projection_telemetry(
    database: &GrafeoDB,
    request: GraphProjectionTelemetryRequest,
) -> Result<Option<GraphProjectionTelemetry>, GraphDbError> {
    check_cancelled(request.cancellation.as_ref())?;
    let Some(projection) = latest_projection(database, &request.namespace, &request.projection)?
    else {
        return Ok(None);
    };
    let entity_count = count_labeled_nodes(
        database,
        &entity_projection_label(&request.namespace, &request.projection),
        ENTITY_LABEL,
        request.cancellation.as_ref(),
    )?;
    let relation_count = count_labeled_nodes(
        database,
        &relation_projection_label(&request.namespace, &request.projection),
        RELATION_LABEL,
        request.cancellation.as_ref(),
    )?;
    check_cancelled(request.cancellation.as_ref())?;
    Ok(Some(GraphProjectionTelemetry {
        source_generation: projection.commit.source_generation,
        watermark: projection.commit.watermark,
        commit_sequence: projection.commit.sequence,
        entity_count,
        relation_count,
    }))
}

fn read_projection(
    database: &GrafeoDB,
    request: GraphProjectionReadRequest,
) -> Result<GraphProjectionPage, GraphDbError> {
    check_cancelled(request.cancellation.as_ref())?;
    if request.max_entities == 0 && request.max_relations == 0 {
        return Err(GraphDbError::BudgetExhausted);
    }
    validate_optional_page_limit(request.max_entities)?;
    validate_optional_page_limit(request.max_relations)?;

    let (entities, next_entity) = read_entity_page(database, &request)?;
    check_cancelled(request.cancellation.as_ref())?;
    let (relations, next_relation) = read_relation_page(database, &request)?;
    check_cancelled(request.cancellation.as_ref())?;
    Ok(GraphProjectionPage {
        entities,
        relations,
        next_entity,
        next_relation,
    })
}

fn read_entity_page(
    database: &GrafeoDB,
    request: &GraphProjectionReadRequest,
) -> Result<(Vec<GraphEntity>, Option<GraphEntityId>), GraphDbError> {
    if request.max_entities == 0 {
        return Ok((Vec::new(), None));
    }
    authenticate_entity_cursor(database, request)?;
    let owner_label = entity_projection_label(&request.namespace, &request.projection);
    let identities = query_identity_page(
        database,
        &owner_label,
        ENTITY_LABEL,
        ENTITY_ID_PROPERTY,
        request.after_entity.as_ref().map(GraphEntityId::as_str),
        request.max_entities.saturating_add(1),
        request.cancellation.as_ref(),
    )?;
    let has_more = identities.len() > request.max_entities;
    let mut entities = Vec::with_capacity(identities.len().min(request.max_entities));
    for identity in identities.into_iter().take(request.max_entities) {
        check_cancelled(request.cancellation.as_ref())?;
        let identity = GraphEntityId::new(identity)
            .map_err(|error| persisted_identity_error("entity", error))?;
        let stored = load_entity(database, &request.namespace, &identity)?.ok_or_else(|| {
            GraphDbError::Corrupt {
                message: "projection query returned an unindexed entity".to_owned(),
            }
        })?;
        if stored.projection != request.projection {
            return Err(GraphDbError::Corrupt {
                message: "projection entity index does not match ownership".to_owned(),
            });
        }
        entities.push(stored.entity);
    }
    let next = has_more
        .then(|| entities.last().map(|entity| entity.identity.clone()))
        .flatten();
    Ok((entities, next))
}

fn read_relation_page(
    database: &GrafeoDB,
    request: &GraphProjectionReadRequest,
) -> Result<(Vec<GraphRelation>, Option<GraphRelationId>), GraphDbError> {
    if request.max_relations == 0 {
        return Ok((Vec::new(), None));
    }
    authenticate_relation_cursor(database, request)?;
    let owner_label = relation_projection_label(&request.namespace, &request.projection);
    let identities = query_identity_page(
        database,
        &owner_label,
        RELATION_LABEL,
        RELATION_ID_PROPERTY,
        request.after_relation.as_ref().map(GraphRelationId::as_str),
        request.max_relations.saturating_add(1),
        request.cancellation.as_ref(),
    )?;
    let has_more = identities.len() > request.max_relations;
    let mut relations = Vec::with_capacity(identities.len().min(request.max_relations));
    for identity in identities.into_iter().take(request.max_relations) {
        check_cancelled(request.cancellation.as_ref())?;
        let identity = GraphRelationId::new(identity)
            .map_err(|error| persisted_identity_error("relation", error))?;
        let stored = load_relation(database, &request.namespace, &identity)?.ok_or_else(|| {
            GraphDbError::Corrupt {
                message: "projection query returned an unindexed relation".to_owned(),
            }
        })?;
        if stored.projection != request.projection {
            return Err(GraphDbError::Corrupt {
                message: "projection relation index does not match ownership".to_owned(),
            });
        }
        relations.push(stored.relation);
    }
    let next = has_more
        .then(|| relations.last().map(|relation| relation.identity.clone()))
        .flatten();
    Ok((relations, next))
}

fn authenticate_entity_cursor(
    database: &GrafeoDB,
    request: &GraphProjectionReadRequest,
) -> Result<(), GraphDbError> {
    let Some(cursor) = &request.after_entity else {
        return Ok(());
    };
    let stored = load_entity(database, &request.namespace, cursor)?;
    if stored.is_some_and(|stored| stored.projection == request.projection) {
        Ok(())
    } else {
        Err(GraphDbError::invalid(
            "entity cursor does not belong to the requested projection",
        ))
    }
}

fn authenticate_relation_cursor(
    database: &GrafeoDB,
    request: &GraphProjectionReadRequest,
) -> Result<(), GraphDbError> {
    let Some(cursor) = &request.after_relation else {
        return Ok(());
    };
    let stored = load_relation(database, &request.namespace, cursor)?;
    if stored.is_some_and(|stored| stored.projection == request.projection) {
        Ok(())
    } else {
        Err(GraphDbError::invalid(
            "relation cursor does not belong to the requested projection",
        ))
    }
}

fn query_identity_page(
    database: &GrafeoDB,
    owner_label: &str,
    record_label: &str,
    identity_property: &str,
    after: Option<&str>,
    limit: usize,
    cancellation: &dyn GraphCancellation,
) -> Result<Vec<String>, GraphDbError> {
    check_cancelled(cancellation)?;
    let mut params = HashMap::from([(
        "limit".to_owned(),
        Value::from(i64::try_from(limit).map_err(|_| GraphDbError::BudgetExhausted)?),
    )]);
    let predicate = if let Some(after) = after {
        params.insert("after".to_owned(), Value::from(after));
        format!(" WHERE n.{identity_property} > $after")
    } else {
        String::new()
    };
    let query = format!(
        "MATCH (n:{owner_label}:{record_label}){predicate} \
         RETURN n.{identity_property} ORDER BY n.{identity_property} LIMIT $limit"
    );
    let result = database
        .execute_with_params(&query, params)
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    let identities = result
        .rows()
        .iter()
        .map(|row| {
            row.first()
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or_else(|| GraphDbError::Corrupt {
                    message: format!(
                        "projection query returned a non-string `{identity_property}`"
                    ),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    check_cancelled(cancellation)?;
    Ok(identities)
}

fn count_labeled_nodes(
    database: &GrafeoDB,
    owner_label: &str,
    record_label: &str,
    cancellation: &dyn GraphCancellation,
) -> Result<u64, GraphDbError> {
    check_cancelled(cancellation)?;
    let query = format!("MATCH (n:{owner_label}:{record_label}) RETURN count(n)");
    let result = database
        .execute(&query)
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    let count = result
        .rows()
        .first()
        .and_then(|row| row.first())
        .and_then(Value::as_int64)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "projection count query returned no integer cardinality".to_owned(),
        })?;
    let count = u64::try_from(count).map_err(|_| GraphDbError::Corrupt {
        message: "projection count query returned a negative cardinality".to_owned(),
    })?;
    check_cancelled(cancellation)?;
    Ok(count)
}

fn validate_page_limit(limit: usize) -> Result<(), GraphDbError> {
    if limit == 0 || limit > MAX_PROJECTION_PAGE_ITEMS {
        Err(GraphDbError::BudgetExhausted)
    } else {
        Ok(())
    }
}

fn validate_optional_page_limit(limit: usize) -> Result<(), GraphDbError> {
    if limit > MAX_PROJECTION_PAGE_ITEMS {
        Err(GraphDbError::BudgetExhausted)
    } else {
        Ok(())
    }
}

fn check_cancelled(cancellation: &dyn GraphCancellation) -> Result<(), GraphDbError> {
    if cancellation.is_cancelled() {
        Err(GraphDbError::Cancelled)
    } else {
        Ok(())
    }
}

fn persisted_identity_error(description: &str, error: GraphDbError) -> GraphDbError {
    GraphDbError::Corrupt {
        message: format!("invalid persisted {description} identity: {error}"),
    }
}
