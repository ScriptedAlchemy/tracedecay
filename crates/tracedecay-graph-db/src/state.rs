use std::collections::{BTreeMap, BTreeSet};

use grafeo_common::types::{ArcStr, EdgeId, NodeId, Value};
use grafeo_core::graph::Direction;
use grafeo_engine::GrafeoDB;

use crate::limits::{
    MAX_GRAPH_IDENTIFIER_BYTES, MAX_VERIFIED_GENERATION_ENTITIES,
    MAX_VERIFIED_GENERATION_RELATIONS, require_generation_capacity,
};
use crate::schema::{
    COMMIT_SEQUENCE_PROPERTY, DIGEST_PROPERTY, ENTITY_ID_PROPERTY, ENTITY_LABEL, FORMAT_LABEL,
    GENERATION_DEPENDENCY_DIGEST_PROPERTY, IDEMPOTENCY_KEY_PROPERTY, NAMESPACE_PROPERTY,
    PROJECTION_LABEL, PROJECTION_PROPERTY, PUBLICATION_DIGEST_PROPERTY,
    PUBLICATION_INPUT_DIGEST_PROPERTY, PUBLICATION_LABEL, RELATION_EDGE_PROPERTY, RELATION_LABEL,
    SEQUENCE_PROPERTY, SOURCE_GENERATION_PROPERTY, WATERMARK_PROPERTY, decode_entity,
    decode_relation, entity_key_label, entity_projection_label, projection_state_label,
    publication_key_label, relation_edge_label, relation_key_label, relation_projection_label,
    required_i64, required_string, stable_key,
};
use crate::{
    GraphCommit, GraphDbError, GraphEntity, GraphEntityId, GraphIdempotencyKey, GraphMutation,
    GraphNamespace, GraphProjectionId, GraphRelation, GraphRelationId, GraphWatermark,
    GraphWriteBatch, SourceGeneration,
};

#[derive(Clone, Debug)]
pub(crate) struct StoredEntity {
    pub(crate) node: NodeId,
    pub(crate) namespace: GraphNamespace,
    pub(crate) projection: GraphProjectionId,
    pub(crate) entity: GraphEntity,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredRelation {
    pub(crate) locator: NodeId,
    pub(crate) edge: EdgeId,
    pub(crate) projection: GraphProjectionId,
    pub(crate) relation: GraphRelation,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredPublication {
    pub(crate) digest: String,
    pub(crate) input_digest: String,
    pub(crate) commit: GraphCommit,
}

pub(crate) struct ExistingBatchState {
    pub(crate) entities: BTreeMap<String, StoredEntity>,
    pub(crate) relations: BTreeMap<String, StoredRelation>,
}

impl ExistingBatchState {
    pub(crate) fn load(database: &GrafeoDB, batch: &GraphWriteBatch) -> Result<Self, GraphDbError> {
        if batch.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let mut entity_keys = BTreeMap::new();
        let mut relation_keys = BTreeMap::new();
        for mutation in &batch.mutations {
            match mutation {
                GraphMutation::DeleteEntity(identity) => {
                    entity_keys.insert(
                        stable_key(&batch.namespace, identity.as_str()),
                        identity.clone(),
                    );
                }
                GraphMutation::UpsertEntity(entity) => {
                    entity_keys.insert(
                        stable_key(&batch.namespace, entity.identity.as_str()),
                        entity.identity.clone(),
                    );
                }
                GraphMutation::DeleteRelation(identity) => {
                    relation_keys.insert(
                        stable_key(&batch.namespace, identity.as_str()),
                        identity.clone(),
                    );
                }
                GraphMutation::UpsertRelation(relation) => {
                    relation_keys.insert(
                        stable_key(&batch.namespace, relation.identity.as_str()),
                        relation.identity.clone(),
                    );
                    entity_keys.insert(
                        stable_key(&batch.namespace, relation.from.as_str()),
                        relation.from.clone(),
                    );
                    entity_keys.insert(
                        stable_key(&batch.namespace, relation.to.as_str()),
                        relation.to.clone(),
                    );
                }
            }
        }
        let entities = load_requested_entities(database, &batch.namespace, entity_keys, batch)?;
        let relations = load_requested_relations(database, &batch.namespace, relation_keys, batch)?;
        Ok(Self {
            entities,
            relations,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectionState {
    pub(crate) node: NodeId,
    pub(crate) commit: GraphCommit,
}

#[derive(Clone)]
pub(crate) struct FormatState {
    pub(crate) marker: NodeId,
    pub(crate) sequence: u64,
}

impl FormatState {
    pub(crate) fn load(database: &GrafeoDB) -> Result<Self, GraphDbError> {
        let store = database.graph_store();
        let markers = store.nodes_by_label(FORMAT_LABEL);
        if markers.len() != 1 {
            return Err(GraphDbError::Corrupt {
                message: "live store lost its exact format marker".to_owned(),
            });
        }
        let marker_node = store
            .get_node(markers[0])
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "format marker is unreadable".to_owned(),
            })?;
        let sequence_i64 = required_i64(
            marker_node.get_property(SEQUENCE_PROPERTY),
            "format commit sequence",
        )?;
        let sequence = u64::try_from(sequence_i64).map_err(|_| GraphDbError::Corrupt {
            message: "format marker has a negative commit sequence".to_owned(),
        })?;
        Ok(Self {
            marker: markers[0],
            sequence,
        })
    }
}

pub(crate) fn load_entity(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    identity: &GraphEntityId,
) -> Result<Option<StoredEntity>, GraphDbError> {
    let Some(node_id) = unique_labeled_node(
        database,
        &entity_key_label(namespace, identity),
        ENTITY_LABEL,
        "entity identity",
    )?
    else {
        return Ok(None);
    };
    let node = database
        .graph_store()
        .get_node(node_id)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "indexed entity node is unreadable".to_owned(),
        })?;
    let stored_namespace = GraphNamespace::new(required_string(
        node.get_property(NAMESPACE_PROPERTY),
        "entity namespace",
    )?)
    .map_err(|error| persisted_validation_error("entity namespace", error))?;
    let projection = GraphProjectionId::new(required_string(
        node.get_property(PROJECTION_PROPERTY),
        "entity projection",
    )?)
    .map_err(|error| persisted_validation_error("entity projection", error))?;
    let entity = decode_entity(&node)?;
    if stored_namespace != *namespace || entity.identity != *identity {
        return Err(GraphDbError::Corrupt {
            message: "entity native index does not match its scalar identity".to_owned(),
        });
    }
    Ok(Some(StoredEntity {
        node: node_id,
        namespace: stored_namespace,
        projection,
        entity,
    }))
}

fn load_requested_entities(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    requested: BTreeMap<String, GraphEntityId>,
    batch: &GraphWriteBatch,
) -> Result<BTreeMap<String, StoredEntity>, GraphDbError> {
    let mut loaded = BTreeMap::new();
    for (index, (key, identity)) in requested.into_iter().enumerate() {
        if index % 256 == 0 && batch.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        if let Some(entity) = load_entity(database, namespace, &identity)? {
            loaded.insert(key, entity);
        }
    }
    Ok(loaded)
}

fn load_requested_relations(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    requested: BTreeMap<String, GraphRelationId>,
    batch: &GraphWriteBatch,
) -> Result<BTreeMap<String, StoredRelation>, GraphDbError> {
    let mut loaded = BTreeMap::new();
    for (index, (key, identity)) in requested.into_iter().enumerate() {
        if index % 256 == 0 && batch.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        if let Some(relation) = load_relation(database, namespace, &identity)? {
            loaded.insert(key, relation);
        }
    }
    Ok(loaded)
}

pub(crate) fn load_entity_by_node(
    database: &GrafeoDB,
    node_id: NodeId,
) -> Result<StoredEntity, GraphDbError> {
    let node = database
        .graph_store()
        .get_node(node_id)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "entity node is unreadable".to_owned(),
        })?;
    let namespace = GraphNamespace::new(required_string(
        node.get_property(NAMESPACE_PROPERTY),
        "entity namespace",
    )?)
    .map_err(|error| persisted_validation_error("entity namespace", error))?;
    let identity = GraphEntityId::new(required_string(
        node.get_property(ENTITY_ID_PROPERTY),
        "entity identity",
    )?)
    .map_err(|error| persisted_validation_error("entity identity", error))?;
    load_entity(database, &namespace, &identity)?.ok_or_else(|| GraphDbError::Corrupt {
        message: "entity node has no indexed native identity".to_owned(),
    })
}

pub(crate) fn load_relation(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    identity: &GraphRelationId,
) -> Result<Option<StoredRelation>, GraphDbError> {
    load_relation_by_key(database, namespace, identity)
}

pub(crate) fn load_relation_by_edge(
    database: &GrafeoDB,
    edge_id: EdgeId,
) -> Result<Option<StoredRelation>, GraphDbError> {
    let Some(locator) = unique_labeled_node(
        database,
        &relation_edge_label(edge_id),
        RELATION_LABEL,
        "relation edge identity",
    )?
    else {
        return Ok(None);
    };
    load_relation_by_locator(database, locator).map(Some)
}

fn load_relation_by_key(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    identity: &GraphRelationId,
) -> Result<Option<StoredRelation>, GraphDbError> {
    let Some(locator) = unique_labeled_node(
        database,
        &relation_key_label(namespace, identity),
        RELATION_LABEL,
        "relation identity",
    )?
    else {
        return Ok(None);
    };
    load_relation_by_locator(database, locator).map(Some)
}

pub(crate) fn load_relation_by_locator(
    database: &GrafeoDB,
    locator_id: NodeId,
) -> Result<StoredRelation, GraphDbError> {
    let store = database.graph_store();
    let locator = store
        .get_node(locator_id)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "indexed relation locator is unreadable".to_owned(),
        })?;
    let edge_i64 = required_i64(
        locator.get_property(RELATION_EDGE_PROPERTY),
        "relation edge identity",
    )?;
    let edge_u64 = u64::try_from(edge_i64).map_err(|_| GraphDbError::Corrupt {
        message: "relation edge identity is negative".to_owned(),
    })?;
    let edge_id = EdgeId::new(edge_u64);
    let edge = store
        .get_edge(edge_id)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "relation locator points to an unreadable edge".to_owned(),
        })?;
    let namespace = GraphNamespace::new(required_string(
        locator.get_property(NAMESPACE_PROPERTY),
        "relation namespace",
    )?)
    .map_err(|error| persisted_validation_error("relation namespace", error))?;
    let projection = GraphProjectionId::new(required_string(
        locator.get_property(PROJECTION_PROPERTY),
        "relation projection",
    )?)
    .map_err(|error| persisted_validation_error("relation projection", error))?;
    let relation = decode_relation(&locator, &edge)?;
    let source = load_entity_by_node(database, edge.src)?;
    let target = load_entity_by_node(database, edge.dst)?;
    let same_namespace = source.namespace == namespace && target.namespace == namespace;
    let generation_scoped = crate::generation::is_physical_generation_namespace(&namespace)
        && crate::generation::is_physical_generation_namespace(&source.namespace)
        && crate::generation::is_physical_generation_namespace(&target.namespace);
    if (!same_namespace && !generation_scoped)
        || source.entity.identity != relation.from
        || target.entity.identity != relation.to
    {
        return Err(GraphDbError::Corrupt {
            message: "relation scalar endpoints do not match native topology".to_owned(),
        });
    }
    Ok(StoredRelation {
        locator: locator_id,
        edge: edge_id,
        projection,
        relation,
    })
}

pub(crate) fn relations_for_entity(
    database: &GrafeoDB,
    entity: NodeId,
) -> Result<Vec<StoredRelation>, GraphDbError> {
    let store = database.graph_store();
    let mut edge_ids = BTreeSet::new();
    edge_ids.extend(
        store
            .edges_from(entity, Direction::Outgoing)
            .into_iter()
            .map(|(_, edge)| edge),
    );
    edge_ids.extend(
        store
            .edges_from(entity, Direction::Incoming)
            .into_iter()
            .map(|(_, edge)| edge),
    );
    edge_ids
        .into_iter()
        .filter_map(|edge| match load_relation_by_edge(database, edge) {
            Ok(Some(relation)) => Some(Ok(relation)),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

pub(crate) fn projection_entities(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
) -> Result<Vec<StoredEntity>, GraphDbError> {
    labeled_projection_nodes(
        database,
        &entity_projection_label(namespace, projection),
        ENTITY_LABEL,
    )?
    .into_iter()
    .map(|node| load_entity_by_node(database, node))
    .collect()
}

pub(crate) fn projection_entities_checked(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<StoredEntity>, GraphDbError> {
    let nodes = labeled_projection_nodes_checked(
        database,
        &entity_projection_label(namespace, projection),
        ENTITY_LABEL,
        MAX_VERIFIED_GENERATION_ENTITIES,
        check,
    )?;
    let mut entities = Vec::with_capacity(nodes.len());
    for node in nodes {
        check()?;
        entities.push(load_entity_by_node(database, node)?);
    }
    check()?;
    Ok(entities)
}

pub(crate) fn projection_entity_nodes_sorted_checked(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<(ArcStr, NodeId)>, GraphDbError> {
    let nodes = labeled_projection_nodes_checked(
        database,
        &entity_projection_label(namespace, projection),
        ENTITY_LABEL,
        MAX_VERIFIED_GENERATION_ENTITIES,
        check,
    )?;
    let store = database.graph_store();
    let mut keyed = Vec::new();
    keyed
        .try_reserve_exact(nodes.len())
        .map_err(|_| GraphDbError::unavailable("native graph entity identity sort is too large"))?;
    for node in nodes {
        check()?;
        let record = store.get_node(node).ok_or_else(|| GraphDbError::Corrupt {
            message: "native graph entity disappeared during verification".to_owned(),
        })?;
        keyed.push((
            required_arc_string(
                record.get_property(ENTITY_ID_PROPERTY),
                "native graph entity identity",
            )?,
            node,
        ));
    }
    check()?;
    keyed.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    if keyed.windows(2).any(|window| window[0].0 == window[1].0) {
        return Err(GraphDbError::Corrupt {
            message: "native graph generation repeats an entity identity".to_owned(),
        });
    }
    Ok(keyed)
}

pub(crate) fn projection_relations(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
) -> Result<Vec<StoredRelation>, GraphDbError> {
    labeled_projection_nodes(
        database,
        &relation_projection_label(namespace, projection),
        RELATION_LABEL,
    )?
    .into_iter()
    .map(|locator| load_relation_by_locator(database, locator))
    .collect()
}

pub(crate) fn projection_relations_checked(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<StoredRelation>, GraphDbError> {
    let locators = labeled_projection_nodes_checked(
        database,
        &relation_projection_label(namespace, projection),
        RELATION_LABEL,
        MAX_VERIFIED_GENERATION_RELATIONS,
        check,
    )?;
    let mut relations = Vec::with_capacity(locators.len());
    for locator in locators {
        check()?;
        relations.push(load_relation_by_locator(database, locator)?);
    }
    check()?;
    Ok(relations)
}

pub(crate) fn projection_relation_nodes_sorted_checked(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<(ArcStr, NodeId)>, GraphDbError> {
    let nodes = labeled_projection_nodes_checked(
        database,
        &relation_projection_label(namespace, projection),
        RELATION_LABEL,
        MAX_VERIFIED_GENERATION_RELATIONS,
        check,
    )?;
    let store = database.graph_store();
    let mut keyed = Vec::new();
    keyed.try_reserve_exact(nodes.len()).map_err(|_| {
        GraphDbError::unavailable("native graph relation identity sort is too large")
    })?;
    for node in nodes {
        check()?;
        let record = store.get_node(node).ok_or_else(|| GraphDbError::Corrupt {
            message: "native graph relation disappeared during verification".to_owned(),
        })?;
        keyed.push((
            required_arc_string(
                record.get_property(crate::schema::RELATION_ID_PROPERTY),
                "native graph relation identity",
            )?,
            node,
        ));
    }
    check()?;
    keyed.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    if keyed.windows(2).any(|window| window[0].0 == window[1].0) {
        return Err(GraphDbError::Corrupt {
            message: "native graph generation repeats a relation identity".to_owned(),
        });
    }
    Ok(keyed)
}

pub(crate) fn projection_node_counts(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
) -> Result<(usize, usize), GraphDbError> {
    let store = database.graph_store();
    let entities = store.nodes_by_label_count(&entity_projection_label(namespace, projection));
    let relations = store.nodes_by_label_count(&relation_projection_label(namespace, projection));
    require_generation_capacity("entities", entities, 0, MAX_VERIFIED_GENERATION_ENTITIES)?;
    require_generation_capacity("relations", relations, 0, MAX_VERIFIED_GENERATION_RELATIONS)?;
    Ok((entities, relations))
}

pub(crate) fn latest_projection(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
) -> Result<Option<ProjectionState>, GraphDbError> {
    let Some(node) = unique_labeled_node(
        database,
        &projection_state_label(namespace, projection),
        PROJECTION_LABEL,
        "projection identity",
    )?
    else {
        return Ok(None);
    };
    decode_projection(database, node, namespace, projection).map(Some)
}

pub(crate) fn publication(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    key: &GraphIdempotencyKey,
) -> Result<Option<StoredPublication>, GraphDbError> {
    let Some(node) = unique_labeled_node(
        database,
        &publication_key_label(namespace, key),
        PUBLICATION_LABEL,
        "publication identity",
    )?
    else {
        return Ok(None);
    };
    let record = database
        .graph_store()
        .get_node(node)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "indexed publication is unreadable".to_owned(),
        })?;
    let stored_namespace = parse_namespace(&record, "publication")?;
    let stored_key = GraphIdempotencyKey::new(required_string(
        record.get_property(IDEMPOTENCY_KEY_PROPERTY),
        "publication idempotency key",
    )?)
    .map_err(|error| persisted_validation_error("publication idempotency key", error))?;
    if stored_namespace != *namespace || stored_key != *key {
        return Err(GraphDbError::Corrupt {
            message: "publication index does not match its scalar identity".to_owned(),
        });
    }
    Ok(Some(StoredPublication {
        digest: required_string(
            record.get_property(PUBLICATION_DIGEST_PROPERTY),
            "publication digest",
        )?,
        input_digest: required_string(
            record.get_property(PUBLICATION_INPUT_DIGEST_PROPERTY),
            "publication input digest",
        )?,
        commit: decode_commit(&record)?,
    }))
}

fn labeled_projection_nodes(
    database: &GrafeoDB,
    owner_label: &str,
    label: &str,
) -> Result<Vec<NodeId>, GraphDbError> {
    let store = database.graph_store();
    Ok(store
        .nodes_by_label(owner_label)
        .into_iter()
        .filter(|node| {
            store
                .get_node(*node)
                .is_some_and(|record| record.has_label(label))
        })
        .collect())
}

fn labeled_projection_nodes_checked(
    database: &GrafeoDB,
    owner_label: &str,
    label: &str,
    maximum: usize,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<NodeId>, GraphDbError> {
    check()?;
    let store = database.graph_store();
    require_generation_capacity(
        if label == ENTITY_LABEL {
            "entities"
        } else {
            "relations"
        },
        store.nodes_by_label_count(owner_label),
        0,
        maximum,
    )?;
    let candidates = store.nodes_by_label(owner_label);
    check()?;
    require_generation_capacity(
        if label == ENTITY_LABEL {
            "entities"
        } else {
            "relations"
        },
        candidates.len(),
        0,
        maximum,
    )?;
    let mut nodes = Vec::new();
    nodes.try_reserve_exact(candidates.len()).map_err(|_| {
        GraphDbError::unavailable("native graph generation identity scan is too large")
    })?;
    for node in candidates {
        check()?;
        if store
            .get_node(node)
            .is_some_and(|record| record.has_label(label))
        {
            nodes.push(node);
        }
    }
    check()?;
    Ok(nodes)
}

fn required_arc_string(value: Option<&Value>, description: &str) -> Result<ArcStr, GraphDbError> {
    match value {
        Some(Value::String(value)) if value.len() <= MAX_GRAPH_IDENTIFIER_BYTES => {
            Ok(value.clone())
        }
        _ => Err(GraphDbError::Corrupt {
            message: format!(
                "native {description} is missing, not a string, or exceeds its product bound"
            ),
        }),
    }
}

fn decode_projection(
    database: &GrafeoDB,
    node: NodeId,
    expected_namespace: &GraphNamespace,
    expected_projection: &GraphProjectionId,
) -> Result<ProjectionState, GraphDbError> {
    let record = database
        .graph_store()
        .get_node(node)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "indexed projection state is unreadable".to_owned(),
        })?;
    let namespace = parse_namespace(&record, "projection")?;
    let projection = GraphProjectionId::new(required_string(
        record.get_property(PROJECTION_PROPERTY),
        "projection identity",
    )?)
    .map_err(|error| persisted_validation_error("projection identity", error))?;
    if namespace != *expected_namespace || projection != *expected_projection {
        return Err(GraphDbError::Corrupt {
            message: "projection locator does not match its scalar identity".to_owned(),
        });
    }
    Ok(ProjectionState {
        node,
        commit: decode_commit(&record)?,
    })
}

fn decode_commit(node: &grafeo_core::graph::lpg::Node) -> Result<GraphCommit, GraphDbError> {
    let sequence = u64::try_from(required_i64(
        node.get_property(COMMIT_SEQUENCE_PROPERTY),
        "commit sequence",
    )?)
    .map_err(|_| GraphDbError::Corrupt {
        message: "native commit sequence is negative".to_owned(),
    })?;
    let source_generation = SourceGeneration::new(required_string(
        node.get_property(SOURCE_GENERATION_PROPERTY),
        "commit source generation",
    )?)
    .map_err(|error| persisted_validation_error("commit source generation", error))?;
    let watermark = GraphWatermark::new(required_string(
        node.get_property(WATERMARK_PROPERTY),
        "commit watermark",
    )?)
    .map_err(|error| persisted_validation_error("commit watermark", error))?;
    let digest = required_string(node.get_property(DIGEST_PROPERTY), "commit digest")?;
    let generation_dependency_digest = node
        .get_property(GENERATION_DEPENDENCY_DIGEST_PROPERTY)
        .map(|_| {
            tracedecay_store::runtime::GraphDependencyGenerationClosureDigestV1::new(
                required_string(
                    node.get_property(GENERATION_DEPENDENCY_DIGEST_PROPERTY),
                    "generation dependency digest",
                )?,
            )
            .map_err(|error| GraphDbError::Corrupt {
                message: format!("invalid persisted generation dependency digest: {error}"),
            })
        })
        .transpose()?;
    Ok(GraphCommit {
        sequence,
        source_generation,
        watermark,
        digest,
        generation_dependency_digest,
    })
}

fn parse_namespace(
    node: &grafeo_core::graph::lpg::Node,
    description: &str,
) -> Result<GraphNamespace, GraphDbError> {
    GraphNamespace::new(required_string(
        node.get_property(NAMESPACE_PROPERTY),
        &format!("{description} namespace"),
    )?)
    .map_err(|error| persisted_validation_error(&format!("{description} namespace"), error))
}

fn unique_labeled_node(
    database: &GrafeoDB,
    key_label: &str,
    label: &str,
    description: &str,
) -> Result<Option<NodeId>, GraphDbError> {
    let store = database.graph_store();
    let mut nodes = store.nodes_by_label(key_label).into_iter().filter(|node| {
        store
            .get_node(*node)
            .is_some_and(|record| record.has_label(label))
    });
    let first = nodes.next();
    if nodes.next().is_some() {
        return Err(GraphDbError::Corrupt {
            message: format!("duplicate native {description}"),
        });
    }
    Ok(first)
}

fn persisted_validation_error(description: &str, error: GraphDbError) -> GraphDbError {
    GraphDbError::Corrupt {
        message: format!("invalid persisted {description}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use grafeo_common::types::Value;
    use grafeo_engine::GrafeoDB;

    use super::{projection_entities_checked, projection_relations_checked};
    use crate::schema::{
        ENTITY_LABEL, RELATION_LABEL, entity_projection_label, relation_projection_label,
    };
    use crate::{GraphDbError, GraphNamespace, GraphProjectionId};

    #[test]
    fn checked_projection_extraction_cancels_before_decoding_rows() {
        let database = GrafeoDB::new_in_memory();
        let namespace = GraphNamespace::new("project").unwrap();
        let projection = GraphProjectionId::new("code").unwrap();
        database
            .session()
            .create_node_with_props(
                &[
                    ENTITY_LABEL,
                    &entity_projection_label(&namespace, &projection),
                ],
                [("malformed", Value::from(true))],
            )
            .unwrap();
        database
            .session()
            .create_node_with_props(
                &[
                    RELATION_LABEL,
                    &relation_projection_label(&namespace, &projection),
                ],
                [("malformed", Value::from(true))],
            )
            .unwrap();

        let entity_polls = Cell::new(0);
        let entity_check = || {
            let poll = entity_polls.get() + 1;
            entity_polls.set(poll);
            if poll == 5 {
                Err(GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        assert!(matches!(
            projection_entities_checked(&database, &namespace, &projection, &entity_check),
            Err(GraphDbError::Cancelled)
        ));

        let relation_polls = Cell::new(0);
        let relation_check = || {
            let poll = relation_polls.get() + 1;
            relation_polls.set(poll);
            if poll == 5 {
                Err(GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        assert!(matches!(
            projection_relations_checked(&database, &namespace, &projection, &relation_check),
            Err(GraphDbError::Cancelled)
        ));
    }
}
