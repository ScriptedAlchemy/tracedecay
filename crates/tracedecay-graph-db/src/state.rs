use std::collections::{BTreeMap, BTreeSet, HashMap};

use grafeo_common::types::{ArcStr, EdgeId, NodeId, Value};
use grafeo_core::graph::lpg::Node;
use grafeo_core::graph::{Direction, GraphStore};
use grafeo_engine::GrafeoDB;
use sha2::{Digest, Sha256};

use crate::limits::{
    MAX_GRAPH_IDENTIFIER_BYTES, MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES,
    MAX_VERIFIED_GENERATION_BATCH_MUTATIONS, MAX_VERIFIED_GENERATION_ENTITIES,
    MAX_VERIFIED_GENERATION_RELATIONS, require_generation_capacity,
};
use crate::schema::{
    COMMIT_SEQUENCE_PROPERTY, COMPACT_IDENTITY_MARKER, DIGEST_PROPERTY, ENTITY_ID_PROPERTY,
    ENTITY_KEY_PROPERTY, ENTITY_LABEL, FORMAT_LABEL, GENERATION_DEPENDENCY_DIGEST_PROPERTY,
    IDEMPOTENCY_KEY_PROPERTY, NAMESPACE_PROPERTY, PROJECTION_KEY_PROPERTY, PROJECTION_LABEL,
    PROJECTION_PROPERTY, PUBLICATION_DIGEST_PROPERTY, PUBLICATION_INPUT_DIGEST_PROPERTY,
    PUBLICATION_KEY_PROPERTY, PUBLICATION_LABEL, RELATION_EDGE_PROPERTY, RELATION_FROM_PROPERTY,
    RELATION_ID_PROPERTY, RELATION_KEY_PROPERTY, RELATION_LABEL, RELATION_TO_PROPERTY,
    SEQUENCE_PROPERTY, SOURCE_GENERATION_PROPERTY, WATERMARK_PROPERTY, decode_entity,
    decode_identity, decode_relation, entity_key_value, entity_projection_label, has_native_label,
    key_value, namespace_key_id, nodes_with_label, nodes_with_label_count,
    projection_state_key_value, publication_key_value, relation_edge_value, relation_key_value,
    relation_projection_label, required_i64, required_string, stable_key,
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
    pub(crate) entities: BTreeMap<Vec<u8>, StoredEntity>,
    pub(crate) entity_locators: BTreeMap<Vec<u8>, EntityLocator>,
    pub(crate) relations: BTreeMap<Vec<u8>, StoredRelation>,
}

impl ExistingBatchState {
    pub(crate) fn load(database: &GrafeoDB, batch: &GraphWriteBatch) -> Result<Self, GraphDbError> {
        if batch.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let namespace_id = namespace_key_id(&batch.namespace);
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
                    entity_keys.insert(stable_key(&namespace_id, identity.as_str()), identity);
                }
                GraphMutation::UpsertEntity(entity) => {
                    entity_keys.insert(
                        stable_key(&namespace_id, entity.identity.as_str()),
                        &entity.identity,
                    );
                }
                GraphMutation::DeleteRelation(identity) => {
                    relation_keys.insert(stable_key(&namespace_id, identity.as_str()), identity);
                }
                GraphMutation::UpsertRelation(relation) => {
                    relation_keys.insert(
                        stable_key(&namespace_id, relation.identity.as_str()),
                        &relation.identity,
                    );
                    let endpoint_keys = if physical_generation {
                        &mut entity_locator_keys
                    } else {
                        &mut entity_keys
                    };
                    endpoint_keys.insert(
                        stable_key(&namespace_id, relation.from.as_str()),
                        &relation.from,
                    );
                    endpoint_keys.insert(
                        stable_key(&namespace_id, relation.to.as_str()),
                        &relation.to,
                    );
                }
            }
        }
        entity_locator_keys.retain(|key, _| !entity_keys.contains_key(key));
        let entities = {
            let _span =
                tracing::trace_span!("graph_db.mutation.existing_state.entity_records").entered();
            load_requested(entity_keys, batch, |identity| {
                load_entity(database, &batch.namespace, identity)
            })
        }?;
        let entity_locators = {
            let _span = tracing::trace_span!("graph_db.mutation.existing_state.endpoint_locators")
                .entered();
            load_requested(entity_locator_keys, batch, |identity| {
                load_entity_locator(database, &batch.namespace, identity)
            })
        }?;
        let mut endpoints = EndpointIdentityCache::default();
        let relations = {
            let _span =
                tracing::trace_span!("graph_db.mutation.existing_state.relation_records").entered();
            load_requested(relation_keys, batch, |identity| {
                load_relation_by_key(database, &batch.namespace, identity, &mut endpoints)
            })
        }?;
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
    /// enumerations, the recovered-generation proof in particular, can
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
    let identity = GraphEntityId::new(decode_identity(
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
    let Some(node_id) = indexed_entity_node(database.graph_store().as_ref(), namespace, identity)?
    else {
        return Ok(None);
    };
    let node = database
        .graph_store()
        .get_node(node_id)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "indexed entity node is unreadable".to_owned(),
        })?;
    let (stored_namespace, projection) = verify_indexed_entity_owner(
        node.get_property(NAMESPACE_PROPERTY),
        node.get_property(PROJECTION_PROPERTY),
        node.get_property(ENTITY_ID_PROPERTY),
        namespace,
        identity,
    )?;
    Ok(Some((node_id, node, stored_namespace, projection)))
}

/// Resolves the exact entity identity within its compact property-index bucket.
pub(crate) fn indexed_entity_node(
    store: &dyn GraphStore,
    namespace: &GraphNamespace,
    identity: &GraphEntityId,
) -> Result<Option<NodeId>, GraphDbError> {
    let key = entity_key_value(namespace, identity);
    unique_property_node(
        store,
        ENTITY_KEY_PROPERTY,
        &key,
        ENTITY_LABEL,
        "entity identity",
        |node| {
            matches_indexed_identity(node, namespace, identity.as_str(), ENTITY_ID_PROPERTY, &key)
        },
    )
}

/// Checks the owner scalars an indexed entity node carries against the
/// identity it was resolved from and returns the owner it belongs to. The
/// unique-key index is derived from exactly these scalars, so a disagreement
/// means the index and the row have drifted apart.
fn verify_indexed_entity_owner(
    stored_namespace: Option<&Value>,
    stored_projection: Option<&Value>,
    stored_identity: Option<&Value>,
    namespace: &GraphNamespace,
    identity: &GraphEntityId,
) -> Result<(GraphNamespace, GraphProjectionId), GraphDbError> {
    let stored_namespace =
        GraphNamespace::new(required_string(stored_namespace, "entity namespace")?)
            .map_err(|error| persisted_validation_error("entity namespace", error))?;
    let projection =
        GraphProjectionId::new(required_string(stored_projection, "entity projection")?)
            .map_err(|error| persisted_validation_error("entity projection", error))?;
    let stored_identity = GraphEntityId::new(decode_identity(stored_identity, "entity identity")?)
        .map_err(|error| persisted_validation_error("entity identity", error))?;
    if stored_namespace != *namespace || stored_identity != *identity {
        return Err(GraphDbError::Corrupt {
            message: "entity native index does not match its scalar identity".to_owned(),
        });
    }
    Ok((stored_namespace, projection))
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

fn load_requested<K, V>(
    requested: HashMap<Vec<u8>, &K>,
    batch: &GraphWriteBatch,
    mut load: impl FnMut(&K) -> Result<Option<V>, GraphDbError>,
) -> Result<BTreeMap<Vec<u8>, V>, GraphDbError> {
    let mut loaded = BTreeMap::new();
    for (index, (key, identity)) in requested.into_iter().enumerate() {
        if index % 256 == 0 && batch.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        if let Some(value) = load(identity)? {
            loaded.insert(key, value);
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
    let identity = GraphEntityId::new(decode_identity(
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
    load_relation_by_key(
        database,
        namespace,
        identity,
        &mut EndpointIdentityCache::default(),
    )
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
        database.graph_store().as_ref(),
        RELATION_EDGE_PROPERTY,
        &relation_edge_value(edge_id)?,
        RELATION_LABEL,
        "relation edge identity",
        |_| Ok(true),
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
    let indexed_key = relation_key_value(namespace, identity);
    let Some(locator) = unique_property_node(
        database.graph_store().as_ref(),
        RELATION_KEY_PROPERTY,
        &indexed_key,
        RELATION_LABEL,
        "relation identity",
        |node| {
            matches_indexed_identity(
                node,
                namespace,
                identity.as_str(),
                RELATION_ID_PROPERTY,
                &indexed_key,
            )
        },
    )?
    else {
        return Ok(None);
    };
    let (stored_namespace, relation) =
        load_owned_relation_by_locator(database.graph_store().as_ref(), locator, cache)?;
    if stored_namespace != *namespace || relation.relation.identity != *identity {
        return Err(GraphDbError::Corrupt {
            message: "relation native index does not match its scalar identity".to_owned(),
        });
    }
    Ok(Some(relation))
}

/// Takes the graph store rather than the database handle so the recovered
/// proof's worker threads can load relations while sharing only the store.
pub(crate) fn load_relation_by_locator_cached(
    store: &dyn GraphStore,
    locator_id: NodeId,
    cache: &mut EndpointIdentityCache,
) -> Result<StoredRelation, GraphDbError> {
    load_owned_relation_by_locator(store, locator_id, cache).map(|(_, relation)| relation)
}

fn load_owned_relation_by_locator(
    store: &dyn GraphStore,
    locator_id: NodeId,
    cache: &mut EndpointIdentityCache,
) -> Result<(GraphNamespace, StoredRelation), GraphDbError> {
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
    Ok((
        namespace,
        StoredRelation {
            locator: locator_id,
            edge: edge_id,
            source: edge.src,
            target: edge.dst,
            projection,
            relation,
        },
    ))
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
        database.graph_store().as_ref(),
        RELATION_EDGE_PROPERTY,
        &relation_edge_value(edge_id)?,
        RELATION_LABEL,
        "relation edge identity",
        |_| Ok(true),
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
    let identity = GraphRelationId::new(decode_identity(
        locator.get_property(RELATION_ID_PROPERTY),
        "relation identity",
    )?)
    .map_err(|error| persisted_validation_error("relation identity", error))?;
    let projection = GraphProjectionId::new(required_string(
        locator.get_property(PROJECTION_PROPERTY),
        "relation projection",
    )?)
    .map_err(|error| persisted_validation_error("relation projection", error))?;
    let from = GraphEntityId::new(decode_identity(
        locator.get_property(RELATION_FROM_PROPERTY),
        "relation source",
    )?)
    .map_err(|error| persisted_validation_error("relation source", error))?;
    let to = GraphEntityId::new(decode_identity(
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
    labeled_projection_identities_sorted_checked(
        database,
        &entity_projection_label(namespace, projection),
        ENTITY_LABEL,
        ENTITY_ID_PROPERTY,
        MAX_VERIFIED_GENERATION_ENTITIES,
        check,
    )
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

#[cfg(test)]
thread_local! {
    /// Records read by retirement page scans on this thread; the retirement
    /// tests pin that a whole generation is read once across all its pages.
    static RETIREMENT_PAGE_RECORD_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_retirement_page_record_reads() {
    RETIREMENT_PAGE_RECORD_READS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn retirement_page_record_reads() -> usize {
    RETIREMENT_PAGE_RECORD_READS.with(std::cell::Cell::get)
}

/// One bounded page of identities to retire from a projection.
///
/// Retirement deletes a generation page by page, so this scan must cost one
/// page, not the projection: it reads owner-label candidates in index order
/// and stops as soon as the page is full. Filtering every candidate first
/// (the `labeled_projection_nodes_checked` shape) re-read the whole
/// projection per page, measured at ~9.5 s per 4,096-row page against a
/// 3.4M-row staging release, an O(rows² / page) sweep that kept the
/// publishing thread, and the serving seat behind it, busy for hours.
#[tracing::instrument(name = "graph_db.projection.deletion_page", level = "trace", skip_all)]
fn projection_identity_deletion_page_checked(
    database: &GrafeoDB,
    owner_label: &str,
    record_label: &str,
    identity_property: &str,
    maximum_records: usize,
    description: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<String>, GraphDbError> {
    check()?;
    let store = database.graph_store();
    require_generation_capacity(
        if record_label == ENTITY_LABEL {
            "entities"
        } else {
            "relations"
        },
        nodes_with_label_count(store.as_ref(), owner_label),
        0,
        maximum_records,
    )?;
    let mut identities = BTreeSet::new();
    let mut live_bytes = 0usize;
    for node in nodes_with_label(store.as_ref(), owner_label) {
        check()?;
        #[cfg(test)]
        RETIREMENT_PAGE_RECORD_READS.with(|count| count.set(count.get() + 1));
        // A candidate carrying only the owner label is a reference node, not
        // a record of this kind; the labeled scan skips those the same way.
        let Some(record) = store
            .get_node(node)
            .filter(|record| has_native_label(record, record_label))
        else {
            continue;
        };
        let identity = identity_arc(
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
    labeled_projection_identities_sorted_checked(
        database,
        &relation_projection_label(namespace, projection),
        RELATION_LABEL,
        crate::schema::RELATION_ID_PROPERTY,
        MAX_VERIFIED_GENERATION_RELATIONS,
        check,
    )
}

fn labeled_projection_identities_sorted_checked(
    database: &GrafeoDB,
    owner_label: &str,
    record_label: &str,
    identity_property: &str,
    maximum: usize,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<(ArcStr, NodeId)>, GraphDbError> {
    let (capacity_kind, identity_description, repeat_kind) = if record_label == ENTITY_LABEL {
        ("entities", "native graph entity identity", "entity")
    } else {
        ("relations", "native graph relation identity", "relation")
    };
    check()?;
    let store = database.graph_store();
    {
        let _span = tracing::trace_span!("graph_db.projection.labeled_nodes.capacity").entered();
        require_generation_capacity(
            capacity_kind,
            nodes_with_label_count(store.as_ref(), owner_label),
            0,
            maximum,
        )?;
    }
    let candidates = {
        let _span = tracing::trace_span!("graph_db.projection.labeled_nodes.scan").entered();
        nodes_with_label(store.as_ref(), owner_label)
    };
    check()?;
    require_generation_capacity(capacity_kind, candidates.len(), 0, maximum)?;
    let mut keyed = Vec::new();
    {
        let _span = tracing::trace_span!("graph_db.projection.labeled_nodes.reserve").entered();
        keyed.try_reserve_exact(candidates.len()).map_err(|_| {
            GraphDbError::unavailable(format!(
                "native graph {repeat_kind} identity sort is too large"
            ))
        })?;
    }
    {
        let _span = tracing::trace_span!("graph_db.projection.labeled_nodes.filter").entered();
        for node in candidates {
            check()?;
            let Some(record) = store
                .get_node(node)
                .filter(|record| has_native_label(record, record_label))
            else {
                continue;
            };
            keyed.push((
                identity_arc(record.get_property(identity_property), identity_description)?,
                node,
            ));
        }
    }
    check()?;
    keyed.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    if keyed.windows(2).any(|window| window[0].0 == window[1].0) {
        return Err(GraphDbError::Corrupt {
            message: format!("native graph generation repeats a {repeat_kind} identity"),
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
    let indexed_key = projection_state_key_value(namespace, projection);
    let Some(node) = unique_property_node(
        database.graph_store().as_ref(),
        PROJECTION_KEY_PROPERTY,
        &indexed_key,
        PROJECTION_LABEL,
        "projection identity",
        |node| {
            matches_indexed_identity(
                node,
                namespace,
                projection.as_str(),
                PROJECTION_PROPERTY,
                &indexed_key,
            )
        },
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
    let indexed_key = publication_key_value(namespace, key);
    let Some(node) = unique_property_node(
        database.graph_store().as_ref(),
        PUBLICATION_KEY_PROPERTY,
        &indexed_key,
        PUBLICATION_LABEL,
        "publication identity",
        |node| {
            matches_indexed_identity(
                node,
                namespace,
                key.as_str(),
                IDEMPOTENCY_KEY_PROPERTY,
                &indexed_key,
            )
        },
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

#[tracing::instrument(name = "graph_db.projection.labeled_nodes", level = "trace", skip_all)]
pub(crate) fn labeled_projection_nodes(
    database: &GrafeoDB,
    owner_label: &str,
    label: &str,
) -> Result<Vec<NodeId>, GraphDbError> {
    let store = database.graph_store();
    let candidates = {
        let _span = tracing::trace_span!("graph_db.projection.labeled_nodes.scan").entered();
        nodes_with_label(store.as_ref(), owner_label)
    };
    let nodes = {
        let _span = tracing::trace_span!("graph_db.projection.labeled_nodes.filter").entered();
        {
            candidates
                .into_iter()
                .filter(|node| {
                    store
                        .get_node(*node)
                        .is_some_and(|record| has_native_label(&record, label))
                })
                .collect::<Vec<_>>()
        }
    };
    Ok(nodes)
}

#[tracing::instrument(name = "graph_db.projection.labeled_nodes", level = "trace", skip_all)]
fn labeled_projection_nodes_checked(
    database: &GrafeoDB,
    owner_label: &str,
    label: &str,
    maximum: usize,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<NodeId>, GraphDbError> {
    check()?;
    let store = database.graph_store();
    {
        let _span = tracing::trace_span!("graph_db.projection.labeled_nodes.capacity").entered();
        {
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
        }
    }?;
    let candidates = {
        let _span = tracing::trace_span!("graph_db.projection.labeled_nodes.scan").entered();
        nodes_with_label(store.as_ref(), owner_label)
    };
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
    {
        let _span = tracing::trace_span!("graph_db.projection.labeled_nodes.reserve").entered();
        {
            nodes.try_reserve_exact(candidates.len()).map_err(|_| {
                GraphDbError::unavailable("native graph generation identity scan is too large")
            })
        }
    }?;
    {
        let _span = tracing::trace_span!("graph_db.projection.labeled_nodes.filter").entered();
        {
            for node in candidates {
                check()?;
                if store
                    .get_node(node)
                    .is_some_and(|record| has_native_label(&record, label))
                {
                    nodes.push(node);
                }
            }
        }
    };
    check()?;
    Ok(nodes)
}

/// An identity scalar as a shared string: verbatim strings are shared as-is,
/// compact relation identities are decoded once.
fn identity_arc(value: Option<&Value>, description: &str) -> Result<ArcStr, GraphDbError> {
    match value {
        Some(Value::String(value))
            if value.len() <= MAX_GRAPH_IDENTIFIER_BYTES
                && !value.starts_with(COMPACT_IDENTITY_MARKER) =>
        {
            Ok(value.clone())
        }
        value => decode_identity(value, description).map(ArcStr::from),
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

/// Compact keys can collide for distinct identities, including hex-encoded
/// names whose first 16 bytes agree. The complete persisted scalars own identity.
fn matches_indexed_identity(
    node: &Node,
    namespace: &GraphNamespace,
    identity: &str,
    identity_property: &str,
    indexed_key: &Value,
) -> Result<bool, GraphDbError> {
    let stored_namespace = parse_namespace(node, "indexed node")?;
    let stored_identity =
        decode_identity(node.get_property(identity_property), "indexed identity")?;
    if stored_namespace == *namespace && stored_identity == identity {
        return Ok(true);
    }
    if key_value(&stored_namespace, &stored_identity) != *indexed_key {
        return Err(GraphDbError::Corrupt {
            message: "native index does not match its scalar identity".to_owned(),
        });
    }
    Ok(false)
}

/// Selects one exact identity from a property-index bucket. Deleted rows and
/// other complete identities in the same bucket are not duplicate records.
#[tracing::instrument(name = "graph_db.read.index_lookup", level = "trace", skip_all)]
fn unique_property_node(
    store: &dyn GraphStore,
    property: &str,
    value: &Value,
    label: &str,
    description: &str,
    matches_identity: impl Fn(&Node) -> Result<bool, GraphDbError>,
) -> Result<Option<NodeId>, GraphDbError> {
    let mut found: Option<NodeId> = None;
    for node_id in store.find_nodes_by_property(property, value) {
        let Some(node) = store.get_node(node_id) else {
            continue;
        };
        if !has_native_label(&node, label) {
            continue;
        }
        if node.get_property(property) != Some(value) {
            return Err(GraphDbError::Corrupt {
                message: "native property index does not match its stored key".to_owned(),
            });
        }
        if !matches_identity(&node)? {
            continue;
        }
        if let Some(first) = found {
            let key_fingerprint = Sha256::digest(format!("{value:?}").as_bytes());
            return Err(GraphDbError::Corrupt {
                message: format!(
                    "duplicate native {description} (property `{property}`, label `{label}`, key_sha256={}, native nodes {} and {})",
                    hex::encode(key_fingerprint),
                    first.as_u64(),
                    node_id.as_u64(),
                ),
            });
        }
        found = Some(node_id);
    }
    Ok(found)
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

    #[test]
    fn indexed_entity_lookup_distinguishes_hex_encoded_names_with_shared_prefixes() {
        let database = GrafeoDB::new_in_memory();
        database.create_property_index(ENTITY_KEY_PROPERTY);
        let namespace = GraphNamespace::new("memory").unwrap();
        let projection = GraphProjectionId::new("facts").unwrap();
        let mut expected = Vec::new();
        for name in [
            "bench-sup-old-b-1791165733039390",
            "bench-sup-old-b-1791165733039391",
        ] {
            let identity =
                GraphEntityId::new(format!("memory-entity:{}", hex::encode(name))).unwrap();
            let entity =
                GraphEntity::new(identity.clone(), BTreeSet::new(), BTreeMap::new()).unwrap();
            let labels = entity_labels(&namespace, &projection, &entity.labels);
            let properties = entity_properties(&namespace, &projection, &entity);
            let node = database
                .session()
                .create_node_with_props(
                    &labels.iter().map(String::as_str).collect::<Vec<_>>(),
                    properties
                        .iter()
                        .map(|(key, value)| (key.as_str(), value.clone())),
                )
                .unwrap();
            expected.push((identity, node));
        }
        for (identity, node) in expected {
            assert_eq!(
                super::indexed_entity_node(database.graph_store().as_ref(), &namespace, &identity)
                    .unwrap(),
                Some(node),
            );
        }
    }

    #[test]
    fn indexed_entity_lookup_rejects_duplicates_and_malformed_candidates() {
        let database = GrafeoDB::new_in_memory();
        database.create_property_index(ENTITY_KEY_PROPERTY);
        let namespace = GraphNamespace::new("memory").unwrap();
        let projection = GraphProjectionId::new("facts").unwrap();
        let identity = GraphEntityId::new("entity").unwrap();
        let entity = GraphEntity::new(identity.clone(), BTreeSet::new(), BTreeMap::new()).unwrap();
        let labels = entity_labels(&namespace, &projection, &entity.labels);
        let properties = entity_properties(&namespace, &projection, &entity);
        let nodes = (0..2)
            .map(|_| {
                database
                    .session()
                    .create_node_with_props(
                        &labels.iter().map(String::as_str).collect::<Vec<_>>(),
                        properties
                            .iter()
                            .map(|(key, value)| (key.as_str(), value.clone())),
                    )
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            super::indexed_entity_node(database.graph_store().as_ref(), &namespace, &identity),
            Err(GraphDbError::Corrupt { .. }),
        ));
        assert!(database.session().delete_node(nodes[0]));
        assert_eq!(
            super::indexed_entity_node(database.graph_store().as_ref(), &namespace, &identity)
                .unwrap(),
            Some(nodes[1]),
        );
        database
            .session()
            .set_node_property(
                nodes[1],
                crate::schema::ENTITY_ID_PROPERTY,
                Value::from("different-identity"),
            )
            .unwrap();
        assert!(matches!(
            super::indexed_entity_node(database.graph_store().as_ref(), &namespace, &identity),
            Err(GraphDbError::Corrupt { .. }),
        ));
        database
            .session()
            .set_node_property(
                nodes[1],
                crate::schema::ENTITY_ID_PROPERTY,
                Value::from(7_i64),
            )
            .unwrap();
        assert!(matches!(
            super::indexed_entity_node(database.graph_store().as_ref(), &namespace, &identity),
            Err(GraphDbError::Corrupt { .. }),
        ));
    }

    #[test]
    fn indexed_entity_lookup_rejects_key_drift_despite_matching_identity() {
        let database = GrafeoDB::new_in_memory();
        database.create_property_index(ENTITY_KEY_PROPERTY);
        let namespace = GraphNamespace::new("memory").unwrap();
        let projection = GraphProjectionId::new("facts").unwrap();
        let identity = GraphEntityId::new("entity").unwrap();
        let entity = GraphEntity::new(identity.clone(), BTreeSet::new(), BTreeMap::new()).unwrap();
        let labels = entity_labels(&namespace, &projection, &entity.labels);
        let properties = entity_properties(&namespace, &projection, &entity);
        let node = database
            .session()
            .create_node_with_props(
                &labels.iter().map(String::as_str).collect::<Vec<_>>(),
                properties
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.clone())),
            )
            .unwrap();
        let key = crate::schema::entity_key_value(&namespace, &identity);
        let property = grafeo_common::types::PropertyKey::new(ENTITY_KEY_PROPERTY);
        let store = database.store();
        assert_eq!(
            store.drain_node_property_column(&property),
            vec![(node, key.clone())]
        );
        store.restore_node_property_column(
            &property,
            [(node, Value::from("different-key"))].into_iter(),
        );
        assert_eq!(
            store.find_nodes_by_property(ENTITY_KEY_PROPERTY, &key),
            vec![node]
        );
        assert_eq!(
            super::indexed_entity_node(database.graph_store().as_ref(), &namespace, &identity),
            Err(GraphDbError::Corrupt {
                message: "native property index does not match its stored key".to_owned(),
            }),
        );
    }

    #[test]
    fn colliding_native_keys_keep_publication_reads_and_mutations_isolated_after_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let options = crate::GraphDbOpenOptions {
            location: crate::GraphDbLocation::Persistent(directory.path().join("identity.grafeo")),
            expected_format: crate::GraphFormatVersion::new(2).unwrap(),
            durability: crate::GraphDurability::WalSync,
            cancellation: Arc::new(NeverCancelled),
        };
        let namespace = GraphNamespace::new("memory").unwrap();
        let owner = crate::GraphDbOwner::open(options.clone()).unwrap();
        let database = owner.issue_lease().unwrap();
        let mut publications = Vec::new();
        for (name, watermark) in [
            ("bench-sup-old-b-1791165733039390", "first"),
            ("bench-sup-old-b-1791165733039391", "second"),
        ] {
            let encoded = hex::encode(name);
            let projection = GraphProjectionId::new(format!("projection:{encoded}")).unwrap();
            let entity = GraphEntity::new(
                GraphEntityId::new(format!("memory-entity:{encoded}")).unwrap(),
                BTreeSet::new(),
                BTreeMap::new(),
            )
            .unwrap();
            let relation = GraphRelation::new(
                GraphRelationId::new(format!("relation:{encoded}")).unwrap(),
                entity.identity.clone(),
                entity.identity.clone(),
                GraphRelationKind::new("self").unwrap(),
                BTreeMap::new(),
            )
            .unwrap();
            let batch = GraphWriteBatch::new(
                namespace.clone(),
                projection,
                SourceGeneration::new(watermark).unwrap(),
                GraphWatermark::new(watermark).unwrap(),
                vec![
                    GraphMutation::UpsertEntity(entity.clone()),
                    GraphMutation::UpsertRelation(relation.clone()),
                ],
                Arc::new(NeverCancelled),
            )
            .unwrap();
            let publication = crate::GraphPublication {
                namespace: namespace.clone(),
                idempotency_key: crate::GraphIdempotencyKey::new(format!("publication:{encoded}"))
                    .unwrap(),
                input_digest: crate::GraphPublicationInputDigest::new(format!(
                    "sha256:{}",
                    "a".repeat(64)
                ))
                .unwrap(),
                source_generation: batch.source_generation.clone(),
                expected_watermark: None,
                next_watermark: batch.next_watermark.clone(),
                batch,
                cancellation: Arc::new(NeverCancelled),
            };
            database.publish_unverified(publication.clone()).unwrap();
            publications.push((publication, entity, relation));
        }
        drop(database);
        owner.close().unwrap();
        let owner = crate::GraphDbOwner::open(options).unwrap();
        let database = owner.issue_lease().unwrap();
        for ((publication, entity, relation), watermark) in
            publications.iter().zip(["first", "second"])
        {
            assert_eq!(
                database
                    .entity(&namespace, &entity.identity, Arc::new(NeverCancelled))
                    .unwrap(),
                Some(entity.clone())
            );
            assert_eq!(
                database
                    .relation(&namespace, &relation.identity, Arc::new(NeverCancelled))
                    .unwrap(),
                Some(relation.clone())
            );
            let receipt = database
                .publication_receipt(
                    &namespace,
                    &publication.idempotency_key,
                    Arc::new(NeverCancelled),
                )
                .unwrap()
                .unwrap();
            assert_eq!(receipt.commit.watermark.as_str(), watermark);
            assert_eq!(
                database.publish_unverified(publication.clone()).unwrap(),
                receipt.commit
            );
            let traversed = database
                .traverse(crate::TraversalRequest {
                    namespace: namespace.clone(),
                    start: entity.identity.clone(),
                    relation_kinds: BTreeSet::from([GraphRelationKind::new("self").unwrap()]),
                    direction: crate::GraphTraversalDirection::Outgoing,
                    max_depth: 1,
                    max_visits: 2,
                    max_results: 2,
                    cancellation: Arc::new(NeverCancelled),
                })
                .unwrap();
            assert_eq!(
                traversed
                    .visits
                    .into_iter()
                    .map(|visit| visit.entity)
                    .collect::<Vec<_>>(),
                vec![entity.identity.clone()]
            );
        }
        let (first, entity, relation) = &publications[0];
        database
            .apply_unverified(
                GraphWriteBatch::new(
                    namespace.clone(),
                    first.batch.projection.clone(),
                    SourceGeneration::new("deleted").unwrap(),
                    GraphWatermark::new("deleted").unwrap(),
                    vec![
                        GraphMutation::DeleteRelation(relation.identity.clone()),
                        GraphMutation::DeleteEntity(entity.identity.clone()),
                    ],
                    Arc::new(NeverCancelled),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            database
                .entity(&namespace, &entity.identity, Arc::new(NeverCancelled))
                .unwrap(),
            None
        );
        assert_eq!(
            database
                .relation(&namespace, &relation.identity, Arc::new(NeverCancelled))
                .unwrap(),
            None
        );
        assert_eq!(
            database
                .entity(
                    &namespace,
                    &publications[1].1.identity,
                    Arc::new(NeverCancelled)
                )
                .unwrap(),
            Some(publications[1].1.clone())
        );
        database.apply_unverified(first.batch.clone()).unwrap();
        assert_eq!(
            database
                .entity(&namespace, &entity.identity, Arc::new(NeverCancelled))
                .unwrap(),
            Some(entity.clone())
        );
        assert_eq!(
            database
                .relation(&namespace, &relation.identity, Arc::new(NeverCancelled))
                .unwrap(),
            Some(relation.clone())
        );
        drop(database);
        owner.close().unwrap();
    }
}
