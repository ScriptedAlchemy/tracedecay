use std::collections::{BTreeMap, BTreeSet, HashMap};

use grafeo_common::types::{ArcStr, EdgeId, NodeId, Value};
use grafeo_core::graph::lpg::Node;
use grafeo_core::graph::{Direction, GraphStore};
use grafeo_engine::GrafeoDB;

use crate::limits::{
    MAX_GRAPH_IDENTIFIER_BYTES, MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES,
    MAX_VERIFIED_GENERATION_BATCH_MUTATIONS, MAX_VERIFIED_GENERATION_ENTITIES,
    MAX_VERIFIED_GENERATION_RELATIONS, require_generation_capacity,
};
use crate::schema::{
    COMMIT_SEQUENCE_PROPERTY, DIGEST_PROPERTY, ENTITY_ID_PROPERTY, ENTITY_KEY_PROPERTY,
    ENTITY_LABEL, FORMAT_LABEL, GENERATION_DEPENDENCY_DIGEST_PROPERTY, IDEMPOTENCY_KEY_PROPERTY,
    NAMESPACE_PROPERTY, PROJECTION_KEY_PROPERTY, PROJECTION_LABEL, PROJECTION_PROPERTY,
    PUBLICATION_DIGEST_PROPERTY, PUBLICATION_INPUT_DIGEST_PROPERTY, PUBLICATION_KEY_PROPERTY,
    PUBLICATION_LABEL, RELATION_EDGE_PROPERTY, RELATION_FROM_PROPERTY, RELATION_ID_PROPERTY,
    RELATION_KEY_PROPERTY, RELATION_LABEL, RELATION_TO_PROPERTY, SEQUENCE_PROPERTY,
    SOURCE_GENERATION_PROPERTY, WATERMARK_PROPERTY, decode_entity, decode_relation,
    encoded_namespace_key, entity_key_value, entity_projection_label, has_native_label,
    nodes_with_label, nodes_with_label_count, projection_state_key_value, publication_key_value,
    relation_edge_value, relation_key_value, relation_projection_label, required_i64,
    required_string, stable_key_from_encoded,
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
    pub(crate) source: NodeId,
    pub(crate) target: NodeId,
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
    pub(crate) entity_locators: BTreeMap<String, EntityLocator>,
    pub(crate) relations: BTreeMap<String, StoredRelation>,
}

impl ExistingBatchState {
    pub(crate) fn load(database: &GrafeoDB, batch: &GraphWriteBatch) -> Result<Self, GraphDbError> {
        if batch.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let encoded_namespace = encoded_namespace_key(&batch.namespace);
        let physical_generation =
            crate::generation::is_physical_generation_namespace(&batch.namespace);
        let (entity_count, relation_count, relation_endpoint_count) =
            batch
                .mutations
                .iter()
                .fold((0usize, 0usize, 0usize), |counts, mutation| {
                    let (entities, relations, endpoints) = counts;
                    match mutation {
                        GraphMutation::DeleteEntity(_) | GraphMutation::UpsertEntity(_) => {
                            (entities.saturating_add(1), relations, endpoints)
                        }
                        GraphMutation::DeleteRelation(_) => {
                            (entities, relations.saturating_add(1), endpoints)
                        }
                        GraphMutation::UpsertRelation(_) => (
                            entities,
                            relations.saturating_add(1),
                            endpoints.saturating_add(2),
                        ),
                    }
                });
        let (local_endpoint_count, locator_count) = if physical_generation {
            (0, relation_endpoint_count)
        } else {
            (relation_endpoint_count, 0)
        };
        let mut entity_keys =
            HashMap::with_capacity(entity_count.saturating_add(local_endpoint_count));
        let mut entity_locator_keys = HashMap::with_capacity(locator_count);
        let mut relation_keys = HashMap::with_capacity(relation_count);
        for mutation in &batch.mutations {
            match mutation {
                GraphMutation::DeleteEntity(identity) => {
                    entity_keys.insert(
                        stable_key_from_encoded(&encoded_namespace, identity.as_str()),
                        identity,
                    );
                }
                GraphMutation::UpsertEntity(entity) => {
                    entity_keys.insert(
                        stable_key_from_encoded(&encoded_namespace, entity.identity.as_str()),
                        &entity.identity,
                    );
                }
                GraphMutation::DeleteRelation(identity) => {
                    relation_keys.insert(
                        stable_key_from_encoded(&encoded_namespace, identity.as_str()),
                        identity,
                    );
                }
                GraphMutation::UpsertRelation(relation) => {
                    relation_keys.insert(
                        stable_key_from_encoded(&encoded_namespace, relation.identity.as_str()),
                        &relation.identity,
                    );
                    let endpoint_keys = if physical_generation {
                        &mut entity_locator_keys
                    } else {
                        &mut entity_keys
                    };
                    endpoint_keys.insert(
                        stable_key_from_encoded(&encoded_namespace, relation.from.as_str()),
                        &relation.from,
                    );
                    endpoint_keys.insert(
                        stable_key_from_encoded(&encoded_namespace, relation.to.as_str()),
                        &relation.to,
                    );
                }
            }
        }
        entity_locator_keys.retain(|key, _| !entity_keys.contains_key(key));
        let entities = hotpath::measure_block!(
            "graph_db.mutation.existing_state.entity_records",
            load_requested_entities(database, &batch.namespace, entity_keys, batch)
        )?;
        let entity_locators = hotpath::measure_block!(
            "graph_db.mutation.existing_state.endpoint_locators",
            load_requested_entity_locators(database, &batch.namespace, entity_locator_keys, batch,)
        )?;
        let relations = hotpath::measure_block!(
            "graph_db.mutation.existing_state.relation_records",
            load_requested_relations(database, &batch.namespace, relation_keys, batch)
        )?;
        Ok(Self {
            entities,
            entity_locators,
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
        let markers = nodes_with_label(store.as_ref(), FORMAT_LABEL);
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

pub(crate) struct EntityLocator {
    pub(crate) node: NodeId,
    pub(crate) namespace: GraphNamespace,
    pub(crate) projection: GraphProjectionId,
}

/// Verified `(namespace, identity)` for relation endpoints, memoized by
/// `NodeId` for one bulk load. Hub entities otherwise re-decode on every
/// incident edge.
#[derive(Default)]
pub(crate) struct EndpointIdentityCache {
    identities: HashMap<NodeId, (GraphNamespace, GraphEntityId)>,
}

impl EndpointIdentityCache {
    /// Takes the graph store rather than the database handle so bulk
    /// enumerations — the recovered-generation proof in particular — can
    /// resolve endpoints from worker threads that share only the store.
    pub(crate) fn identity(
        &mut self,
        store: &dyn GraphStore,
        node_id: NodeId,
    ) -> Result<(GraphNamespace, GraphEntityId), GraphDbError> {
        if let Some(cached) = self.identities.get(&node_id) {
            return Ok(cached.clone());
        }
        let identity = entity_endpoint_identity(store, node_id)?;
        self.identities.insert(node_id, identity.clone());
        Ok(identity)
    }
}

/// Identity + owner fields from one already-loaded node, without decoding
/// labels or graph properties.
fn entity_endpoint_identity(
    store: &dyn GraphStore,
    node_id: NodeId,
) -> Result<(GraphNamespace, GraphEntityId), GraphDbError> {
    let node = store
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
    Ok((namespace, identity))
}

fn load_indexed_entity_node(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    identity: &GraphEntityId,
) -> Result<Option<(NodeId, Node, GraphNamespace, GraphProjectionId)>, GraphDbError> {
    let Some(node_id) = unique_property_node(
        database,
        ENTITY_KEY_PROPERTY,
        &entity_key_value(namespace, identity),
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
    let stored_identity = GraphEntityId::new(required_string(
        node.get_property(ENTITY_ID_PROPERTY),
        "entity identity",
    )?)
    .map_err(|error| persisted_validation_error("entity identity", error))?;
    if stored_namespace != *namespace || stored_identity != *identity {
        return Err(GraphDbError::Corrupt {
            message: "entity native index does not match its scalar identity".to_owned(),
        });
    }
    Ok(Some((node_id, node, stored_namespace, projection)))
}

pub(crate) fn load_entity_locator(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    identity: &GraphEntityId,
) -> Result<Option<EntityLocator>, GraphDbError> {
    Ok(
        load_indexed_entity_node(database, namespace, identity)?.map(
            |(node, _, stored_namespace, projection)| EntityLocator {
                node,
                namespace: stored_namespace,
                projection,
            },
        ),
    )
}

pub(crate) fn load_entity(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    identity: &GraphEntityId,
) -> Result<Option<StoredEntity>, GraphDbError> {
    let Some((node_id, node, stored_namespace, projection)) =
        load_indexed_entity_node(database, namespace, identity)?
    else {
        return Ok(None);
    };
    Ok(Some(StoredEntity {
        node: node_id,
        namespace: stored_namespace,
        projection,
        entity: decode_entity(&node)?,
    }))
}

fn load_requested_entities(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    requested: HashMap<String, &GraphEntityId>,
    batch: &GraphWriteBatch,
) -> Result<BTreeMap<String, StoredEntity>, GraphDbError> {
    let mut loaded = BTreeMap::new();
    for (index, (key, identity)) in requested.into_iter().enumerate() {
        if index % 256 == 0 && batch.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        if let Some(entity) = load_entity(database, namespace, identity)? {
            loaded.insert(key, entity);
        }
    }
    Ok(loaded)
}

fn load_requested_entity_locators(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    requested: HashMap<String, &GraphEntityId>,
    batch: &GraphWriteBatch,
) -> Result<BTreeMap<String, EntityLocator>, GraphDbError> {
    let mut loaded = BTreeMap::new();
    for (index, (key, identity)) in requested.into_iter().enumerate() {
        if index % 256 == 0 && batch.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        if let Some(entity) = load_entity_locator(database, namespace, identity)? {
            loaded.insert(key, entity);
        }
    }
    Ok(loaded)
}

fn load_requested_relations(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    requested: HashMap<String, &GraphRelationId>,
    batch: &GraphWriteBatch,
) -> Result<BTreeMap<String, StoredRelation>, GraphDbError> {
    let mut loaded = BTreeMap::new();
    let mut endpoints = EndpointIdentityCache::default();
    for (index, (key, identity)) in requested.into_iter().enumerate() {
        if index % 256 == 0 && batch.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        if let Some(relation) = load_relation_cached(database, namespace, identity, &mut endpoints)?
        {
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
    load_relation_cached(
        database,
        namespace,
        identity,
        &mut EndpointIdentityCache::default(),
    )
}

pub(crate) fn load_relation_cached(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    identity: &GraphRelationId,
    cache: &mut EndpointIdentityCache,
) -> Result<Option<StoredRelation>, GraphDbError> {
    load_relation_by_key(database, namespace, identity, cache)
}

pub(crate) fn load_relation_by_edge(
    database: &GrafeoDB,
    edge_id: EdgeId,
) -> Result<Option<StoredRelation>, GraphDbError> {
    load_relation_by_edge_cached(database, edge_id, &mut EndpointIdentityCache::default())
}

pub(crate) fn load_relation_by_edge_cached(
    database: &GrafeoDB,
    edge_id: EdgeId,
    cache: &mut EndpointIdentityCache,
) -> Result<Option<StoredRelation>, GraphDbError> {
    let Some(locator) = unique_property_node(
        database,
        RELATION_EDGE_PROPERTY,
        &relation_edge_value(edge_id)?,
        RELATION_LABEL,
        "relation edge identity",
    )?
    else {
        return Ok(None);
    };
    load_relation_by_locator_cached(database.graph_store().as_ref(), locator, cache).map(Some)
}

fn load_relation_by_key(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    identity: &GraphRelationId,
    cache: &mut EndpointIdentityCache,
) -> Result<Option<StoredRelation>, GraphDbError> {
    let Some(locator) = unique_property_node(
        database,
        RELATION_KEY_PROPERTY,
        &relation_key_value(namespace, identity),
        RELATION_LABEL,
        "relation identity",
    )?
    else {
        return Ok(None);
    };
    load_relation_by_locator_cached(database.graph_store().as_ref(), locator, cache).map(Some)
}

/// Takes the graph store rather than the database handle so the recovered
/// proof's worker threads can load relations while sharing only the store.
pub(crate) fn load_relation_by_locator_cached(
    store: &dyn GraphStore,
    locator_id: NodeId,
    cache: &mut EndpointIdentityCache,
) -> Result<StoredRelation, GraphDbError> {
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
    let (source_namespace, source_identity) = cache.identity(store, edge.src)?;
    let (target_namespace, target_identity) = cache.identity(store, edge.dst)?;
    let same_namespace = source_namespace == namespace && target_namespace == namespace;
    let generation_scoped = crate::generation::is_physical_generation_namespace(&namespace)
        && crate::generation::is_physical_generation_namespace(&source_namespace)
        && crate::generation::is_physical_generation_namespace(&target_namespace);
    if (!same_namespace && !generation_scoped)
        || source_identity != relation.from
        || target_identity != relation.to
    {
        return Err(GraphDbError::Corrupt {
            message: "relation scalar endpoints do not match native topology".to_owned(),
        });
    }
    Ok(StoredRelation {
        locator: locator_id,
        edge: edge_id,
        source: edge.src,
        target: edge.dst,
        projection,
        relation,
    })
}

pub(crate) struct RelationReference {
    pub(crate) identity: GraphRelationId,
    pub(crate) projection: GraphProjectionId,
    pub(crate) from: GraphEntityId,
    pub(crate) to: GraphEntityId,
}

fn incident_edge_ids(
    database: &GrafeoDB,
    entity: NodeId,
    directions: &[Direction],
) -> BTreeSet<EdgeId> {
    let store = database.graph_store();
    let mut edge_ids = BTreeSet::new();
    for direction in directions {
        edge_ids.extend(
            store
                .edges_from(entity, *direction)
                .into_iter()
                .map(|(_, edge)| edge),
        );
    }
    edge_ids
}

pub(crate) fn outgoing_relation_projections(
    database: &GrafeoDB,
    entity: NodeId,
) -> Result<Vec<GraphProjectionId>, GraphDbError> {
    let mut projections = Vec::new();
    for edge in incident_edge_ids(database, entity, &[Direction::Outgoing]) {
        if let Some(projection) = load_relation_projection_by_edge(database, edge)? {
            projections.push(projection);
        }
    }
    Ok(projections)
}

pub(crate) fn relation_references_for_entity(
    database: &GrafeoDB,
    entity: NodeId,
) -> Result<Vec<RelationReference>, GraphDbError> {
    incident_edge_ids(
        database,
        entity,
        &[Direction::Outgoing, Direction::Incoming],
    )
    .into_iter()
    .filter_map(
        |edge| match load_relation_reference_by_edge(database, edge) {
            Ok(Some(relation)) => Some(Ok(relation)),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        },
    )
    .collect()
}

fn load_relation_projection_by_edge(
    database: &GrafeoDB,
    edge_id: EdgeId,
) -> Result<Option<GraphProjectionId>, GraphDbError> {
    Ok(load_relation_by_edge(database, edge_id)?.map(|stored| stored.projection))
}

fn load_relation_reference_by_edge(
    database: &GrafeoDB,
    edge_id: EdgeId,
) -> Result<Option<RelationReference>, GraphDbError> {
    let Some(locator_id) = unique_property_node(
        database,
        RELATION_EDGE_PROPERTY,
        &relation_edge_value(edge_id)?,
        RELATION_LABEL,
        "relation edge identity",
    )?
    else {
        return Ok(None);
    };
    let locator =
        database
            .graph_store()
            .get_node(locator_id)
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "indexed relation locator is unreadable".to_owned(),
            })?;
    let identity = GraphRelationId::new(required_string(
        locator.get_property(RELATION_ID_PROPERTY),
        "relation identity",
    )?)
    .map_err(|error| persisted_validation_error("relation identity", error))?;
    let projection = GraphProjectionId::new(required_string(
        locator.get_property(PROJECTION_PROPERTY),
        "relation projection",
    )?)
    .map_err(|error| persisted_validation_error("relation projection", error))?;
    let from = GraphEntityId::new(required_string(
        locator.get_property(RELATION_FROM_PROPERTY),
        "relation source",
    )?)
    .map_err(|error| persisted_validation_error("relation source", error))?;
    let to = GraphEntityId::new(required_string(
        locator.get_property(RELATION_TO_PROPERTY),
        "relation target",
    )?)
    .map_err(|error| persisted_validation_error("relation target", error))?;
    Ok(Some(RelationReference {
        identity,
        projection,
        from,
        to,
    }))
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

#[cfg(test)]
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
    let locators = labeled_projection_nodes(
        database,
        &relation_projection_label(namespace, projection),
        RELATION_LABEL,
    )?;
    let store = database.graph_store();
    let mut endpoints = EndpointIdentityCache::default();
    locators
        .into_iter()
        .map(|locator| load_relation_by_locator_cached(store.as_ref(), locator, &mut endpoints))
        .collect()
}

#[cfg(test)]
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
    let store = database.graph_store();
    let mut endpoints = EndpointIdentityCache::default();
    for locator in locators {
        check()?;
        relations.push(load_relation_by_locator_cached(
            store.as_ref(),
            locator,
            &mut endpoints,
        )?);
    }
    check()?;
    Ok(relations)
}

pub(crate) fn projection_entity_deletion_page_checked(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<GraphMutation>, GraphDbError> {
    projection_identity_deletion_page_checked(
        database,
        &entity_projection_label(namespace, projection),
        ENTITY_LABEL,
        ENTITY_ID_PROPERTY,
        MAX_VERIFIED_GENERATION_ENTITIES,
        "entity",
        check,
    )?
    .into_iter()
    .map(|identity| GraphEntityId::new(identity).map(GraphMutation::DeleteEntity))
    .collect()
}

pub(crate) fn projection_relation_deletion_page_checked(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<GraphMutation>, GraphDbError> {
    projection_identity_deletion_page_checked(
        database,
        &relation_projection_label(namespace, projection),
        RELATION_LABEL,
        crate::schema::RELATION_ID_PROPERTY,
        MAX_VERIFIED_GENERATION_RELATIONS,
        "relation",
        check,
    )?
    .into_iter()
    .map(|identity| GraphRelationId::new(identity).map(GraphMutation::DeleteRelation))
    .collect()
}

fn projection_identity_deletion_page_checked(
    database: &GrafeoDB,
    owner_label: &str,
    record_label: &str,
    identity_property: &str,
    maximum_records: usize,
    description: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<String>, GraphDbError> {
    let nodes = labeled_projection_nodes_checked(
        database,
        owner_label,
        record_label,
        maximum_records,
        check,
    )?;
    let store = database.graph_store();
    let mut identities = BTreeSet::new();
    let mut live_bytes = 0usize;
    for node in nodes {
        check()?;
        let record = store.get_node(node).ok_or_else(|| GraphDbError::Corrupt {
            message: format!("native graph {description} disappeared during retirement"),
        })?;
        let identity = required_arc_string(
            record.get_property(identity_property),
            &format!("native graph {description} identity"),
        )?;
        let next_live_bytes = live_bytes.checked_add(identity.len()).ok_or_else(|| {
            GraphDbError::budget_exhausted_count(
                crate::GraphBudgetKind::Write,
                MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES,
            )
        })?;
        if identities.len() == MAX_VERIFIED_GENERATION_BATCH_MUTATIONS
            || next_live_bytes > MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES
        {
            break;
        }
        live_bytes = next_live_bytes;
        if !identities.insert(identity.as_str().to_owned()) {
            return Err(GraphDbError::Corrupt {
                message: format!("native graph generation repeats a {description} identity"),
            });
        }
    }
    check()?;
    Ok(identities.into_iter().collect())
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
    let entities = nodes_with_label_count(
        store.as_ref(),
        &entity_projection_label(namespace, projection),
    );
    let relations = nodes_with_label_count(
        store.as_ref(),
        &relation_projection_label(namespace, projection),
    );
    require_generation_capacity("entities", entities, 0, MAX_VERIFIED_GENERATION_ENTITIES)?;
    require_generation_capacity("relations", relations, 0, MAX_VERIFIED_GENERATION_RELATIONS)?;
    Ok((entities, relations))
}

pub(crate) fn latest_projection(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
) -> Result<Option<ProjectionState>, GraphDbError> {
    let Some(node) = unique_property_node(
        database,
        PROJECTION_KEY_PROPERTY,
        &projection_state_key_value(namespace, projection),
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
    let Some(node) = unique_property_node(
        database,
        PUBLICATION_KEY_PROPERTY,
        &publication_key_value(namespace, key),
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

#[hotpath::measure(label = "graph_db.projection.labeled_nodes")]
pub(crate) fn labeled_projection_nodes(
    database: &GrafeoDB,
    owner_label: &str,
    label: &str,
) -> Result<Vec<NodeId>, GraphDbError> {
    let store = database.graph_store();
    let candidates = hotpath::measure_block!(
        "graph_db.projection.labeled_nodes.scan",
        nodes_with_label(store.as_ref(), owner_label)
    );
    hotpath::gauge!("graph_db.projection.labeled_nodes.candidates").set(candidates.len());
    let nodes = hotpath::measure_block!("graph_db.projection.labeled_nodes.filter", {
        candidates
            .into_iter()
            .filter(|node| {
                store
                    .get_node(*node)
                    .is_some_and(|record| has_native_label(&record, label))
            })
            .collect::<Vec<_>>()
    });
    hotpath::gauge!("graph_db.projection.labeled_nodes.results").set(nodes.len());
    Ok(nodes)
}

#[hotpath::measure(label = "graph_db.projection.labeled_nodes")]
fn labeled_projection_nodes_checked(
    database: &GrafeoDB,
    owner_label: &str,
    label: &str,
    maximum: usize,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<NodeId>, GraphDbError> {
    check()?;
    let store = database.graph_store();
    hotpath::measure_block!("graph_db.projection.labeled_nodes.capacity", {
        require_generation_capacity(
            if label == ENTITY_LABEL {
                "entities"
            } else {
                "relations"
            },
            nodes_with_label_count(store.as_ref(), owner_label),
            0,
            maximum,
        )
    })?;
    let candidates = hotpath::measure_block!(
        "graph_db.projection.labeled_nodes.scan",
        nodes_with_label(store.as_ref(), owner_label)
    );
    hotpath::gauge!("graph_db.projection.labeled_nodes.candidates").set(candidates.len());
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
    hotpath::measure_block!("graph_db.projection.labeled_nodes.reserve", {
        nodes.try_reserve_exact(candidates.len()).map_err(|_| {
            GraphDbError::unavailable("native graph generation identity scan is too large")
        })
    })?;
    hotpath::measure_block!("graph_db.projection.labeled_nodes.filter", {
        for node in candidates {
            check()?;
            if store
                .get_node(node)
                .is_some_and(|record| has_native_label(&record, label))
            {
                nodes.push(node);
            }
        }
    });
    hotpath::gauge!("graph_db.projection.labeled_nodes.results").set(nodes.len());
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

/// Every node currently carrying `value` in the unique-key index `property`
/// and bearing `label`.
///
/// The `label` filter is load-bearing and not a redundant sanity check.
/// grafeo's `delete_node` clears the label index and drops the node's
/// properties, but never calls `update_property_index_on_remove`, so a
/// property index keeps returning the `NodeId` of a deleted node
/// (`grafeo-core/src/graph/lpg/store/node_ops.rs:528`). Re-reading each
/// candidate discards those tombstones: a deleted node reads back as `None`,
/// and one that somehow survives no longer carries its record label. This is
/// what preserves the absence and duplicate semantics the synthetic key-label
/// lookup had, where the label index *was* maintained on delete.
pub(crate) fn indexed_nodes(
    store: &dyn grafeo_core::graph::GraphStore,
    property: &str,
    value: &Value,
    label: &str,
) -> Vec<NodeId> {
    store
        .find_nodes_by_property(property, value)
        .into_iter()
        .filter(|node| {
            store
                .get_node(*node)
                .is_some_and(|record| has_native_label(&record, label))
        })
        .collect()
}

/// Resolves the one node that carries `value` in the unique-key index
/// `property` and bears `label`.
///
/// Absence is `Ok(None)` and a duplicate is `Corrupt`, exactly as the synthetic
/// key-label lookup this replaced. The index is a *property* and not a label
/// because grafeo files one columnar node table per distinct label: a key label
/// per entity mints one table per entity and exhausts the `u16` table id at
/// 32,767 rows, long before a real repository graph is loaded.
#[hotpath::measure(label = "graph_db.read.index_lookup")]
fn unique_property_node(
    database: &GrafeoDB,
    property: &str,
    value: &Value,
    label: &str,
    description: &str,
) -> Result<Option<NodeId>, GraphDbError> {
    let mut nodes =
        indexed_nodes(database.graph_store().as_ref(), property, value, label).into_iter();
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
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use grafeo_common::types::Value;
    use grafeo_engine::GrafeoDB;

    use super::{ExistingBatchState, projection_entities_checked, projection_relations_checked};
    use crate::schema::{
        ENTITY_KEY_PROPERTY, ENTITY_LABEL, RELATION_LABEL, entity_labels, entity_projection_label,
        entity_properties, relation_projection_label,
    };
    use crate::{
        GraphDbError, GraphEntity, GraphEntityId, GraphGenerationId, GraphMutation, GraphNamespace,
        GraphProjectionId, GraphRelation, GraphRelationId, GraphRelationKind, GraphWatermark,
        GraphWriteBatch, NeverCancelled, SourceGeneration,
    };

    #[test]
    fn physical_relation_stage_loads_endpoint_identity_without_decoding_payload() {
        let database = GrafeoDB::new_in_memory();
        database.create_property_index(ENTITY_KEY_PROPERTY);
        let logical_namespace = GraphNamespace::new("project").unwrap();
        let projection = GraphProjectionId::new("code").unwrap();
        let physical_namespace = crate::generation::physical_namespace(
            &logical_namespace,
            &projection,
            &GraphGenerationId::new("generation").unwrap(),
        )
        .unwrap();
        let endpoint = GraphEntity::new(
            GraphEntityId::new("endpoint").unwrap(),
            BTreeSet::new(),
            BTreeMap::new(),
        )
        .unwrap();
        let labels = entity_labels(&physical_namespace, &projection, &endpoint.labels);
        let mut properties = entity_properties(&physical_namespace, &projection, &endpoint);
        properties.push((
            "__tracedecay_graph_db_property_str_zz".to_owned(),
            Value::from("payload whose malformed property name must not be decoded"),
        ));
        let label_refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
        database
            .session()
            .create_node_with_props(
                &label_refs,
                properties
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.clone())),
            )
            .unwrap();
        let relation = GraphRelation::new(
            GraphRelationId::new("edge").unwrap(),
            endpoint.identity.clone(),
            endpoint.identity.clone(),
            GraphRelationKind::new("calls").unwrap(),
            BTreeMap::new(),
        )
        .unwrap();
        let batch = GraphWriteBatch::new(
            physical_namespace,
            projection.clone(),
            SourceGeneration::new("source").unwrap(),
            GraphWatermark::new("watermark").unwrap(),
            vec![GraphMutation::UpsertRelation(relation)],
            Arc::new(NeverCancelled),
        )
        .unwrap();

        let loaded = ExistingBatchState::load(&database, &batch).unwrap();

        assert!(loaded.entities.is_empty());

        let labels = entity_labels(&logical_namespace, &projection, &endpoint.labels);
        let mut properties = entity_properties(&logical_namespace, &projection, &endpoint);
        properties.push((
            "__tracedecay_graph_db_property_str_zz".to_owned(),
            Value::from("ordinary mutable graphs must still validate endpoint payloads"),
        ));
        let label_refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
        database
            .session()
            .create_node_with_props(
                &label_refs,
                properties
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.clone())),
            )
            .unwrap();
        let relation = GraphRelation::new(
            GraphRelationId::new("ordinary-edge").unwrap(),
            endpoint.identity.clone(),
            endpoint.identity.clone(),
            GraphRelationKind::new("calls").unwrap(),
            BTreeMap::new(),
        )
        .unwrap();
        let ordinary_batch = GraphWriteBatch::new(
            logical_namespace,
            projection,
            SourceGeneration::new("ordinary-source").unwrap(),
            GraphWatermark::new("ordinary-watermark").unwrap(),
            vec![GraphMutation::UpsertRelation(relation)],
            Arc::new(NeverCancelled),
        )
        .unwrap();

        assert!(matches!(
            ExistingBatchState::load(&database, &ordinary_batch),
            Err(GraphDbError::Corrupt { .. })
        ));
    }

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
