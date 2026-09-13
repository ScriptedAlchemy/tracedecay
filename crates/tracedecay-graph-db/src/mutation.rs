use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use grafeo_common::types::Value;
use grafeo_engine::{GrafeoDB, Session};

use crate::error::rollback_failure;
use crate::schema::{
    ENTITY_KEY_PROPERTY, ENTITY_LABEL, PROJECTION_KEY_PROPERTY, PROJECTION_LABEL,
    PUBLICATION_KEY_PROPERTY, PUBLICATION_LABEL, RELATION_KEY_PROPERTY, SEQUENCE_PROPERTY,
    edge_properties, encoded_namespace_key, entity_labels, entity_properties,
    projection_properties, publication_properties, relation_locator_labels, relation_properties,
    relation_type_for_kind, stable_key_from_encoded,
};
use crate::state::{
    ExistingBatchState, FormatState, StoredEntity, StoredRelation, latest_projection,
    relation_references_for_entity,
};
use crate::{
    GraphCommit, GraphDbError, GraphEntityId, GraphIdempotencyKey, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphRelationId, GraphWriteBatch,
};

type EntityChange = Option<GraphProjectionId>;
type RelationChange = Option<(GraphProjectionId, GraphEntityId, GraphEntityId)>;
pub(crate) type RelationEndpointNamespaces =
    BTreeMap<GraphRelationId, (GraphNamespace, GraphNamespace)>;
type ResolvedRelationEndpoints = BTreeMap<
    GraphRelationId,
    (
        Option<grafeo_common::types::NodeId>,
        Option<grafeo_common::types::NodeId>,
    ),
>;

/// Durable identity carried by one applied write batch: the canonical batch
/// digest, the dependency-closure digest for verified generations, and the
/// publication receipt (idempotency key, digest, input digest).
pub(crate) struct CommitMetadata {
    pub(crate) digest: String,
    pub(crate) generation_dependency_digest:
        Option<tracedecay_store::runtime::GraphDependencyGenerationClosureDigestV1>,
    pub(crate) publication_record: Option<(GraphIdempotencyKey, String, String)>,
}

impl CommitMetadata {
    pub(crate) fn for_digest(digest: String) -> Self {
        Self {
            digest,
            generation_dependency_digest: None,
            publication_record: None,
        }
    }
}

pub(crate) fn apply(
    database: &GrafeoDB,
    state: &mut FormatState,
    batch: GraphWriteBatch,
    metadata: CommitMetadata,
    endpoint_namespaces: &RelationEndpointNamespaces,
    poisoned: &AtomicBool,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphCommit, GraphDbError> {
    check()?;
    let CommitMetadata {
        digest,
        generation_dependency_digest,
        publication_record,
    } = metadata;
    let existing = hotpath::measure_block!(
        "graph_db.mutation.existing_state",
        ExistingBatchState::load(database, &batch)
    )?;
    let external_endpoints = hotpath::measure_block!(
        "graph_db.mutation.validate_references",
        validate_references(database, &batch, &existing, endpoint_namespaces, check)
    )?;
    let sequence = state
        .sequence
        .checked_add(1)
        .ok_or_else(|| GraphDbError::unavailable("graph commit sequence exhausted"))?;
    let commit = GraphCommit {
        sequence,
        source_generation: batch.source_generation.clone(),
        watermark: batch.next_watermark.clone(),
        digest,
        generation_dependency_digest,
    };
    let previous_projection = latest_projection(database, &batch.namespace, &batch.projection)?;
    let mut session = database.session();
    hotpath::measure_block!(
        "graph_db.mutation.begin_transaction",
        session.begin_transaction()
    )
    .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    let result = hotpath::measure_block!(
        "graph_db.mutation.apply_changes",
        apply_in_transaction(
            &session,
            state,
            &batch,
            &commit,
            previous_projection
                .as_ref()
                .map(|projection| projection.node),
            publication_record.as_ref(),
            &existing,
            &external_endpoints,
            check,
        )
    );
    if let Err(error) = result {
        return Err(rollback_or_poison(&mut session, error, poisoned));
    }
    if let Err(error) = check() {
        return Err(rollback_or_poison(&mut session, error, poisoned));
    }
    if batch.cancellation.is_cancelled() {
        return Err(rollback_or_poison(
            &mut session,
            GraphDbError::Cancelled,
            poisoned,
        ));
    }
    hotpath::measure_block!("graph_db.mutation.commit", session.commit())
        .map_err(map_commit_error)?;
    state.sequence = sequence;
    Ok(commit)
}

#[allow(clippy::too_many_arguments)]
fn apply_in_transaction(
    session: &Session,
    state: &FormatState,
    batch: &GraphWriteBatch,
    commit: &GraphCommit,
    previous_projection: Option<grafeo_common::types::NodeId>,
    publication_record: Option<&(GraphIdempotencyKey, String, String)>,
    existing: &ExistingBatchState,
    external_endpoints: &ResolvedRelationEndpoints,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    let encoded_namespace = encoded_namespace_key(&batch.namespace);
    let tracks_relation_endpoints = batch
        .mutations
        .iter()
        .any(|mutation| matches!(mutation, GraphMutation::UpsertRelation(_)));
    let entity_capacity = if tracks_relation_endpoints {
        batch
            .mutations
            .iter()
            .filter(|mutation| {
                matches!(
                    mutation,
                    GraphMutation::DeleteEntity(_) | GraphMutation::UpsertEntity(_)
                )
            })
            .count()
    } else {
        0
    };
    let mut entity_nodes =
        HashMap::<&str, Option<grafeo_common::types::NodeId>>::with_capacity(entity_capacity);
    for mutation in &batch.mutations {
        check_cancelled(batch, check)?;
        match mutation {
            GraphMutation::DeleteRelation(identity) => {
                let key = stable_key_from_encoded(&encoded_namespace, identity.as_str());
                if let Some(stored) = existing.relations.get(&key) {
                    delete_relation(session, stored, batch, check)?;
                }
            }
            GraphMutation::DeleteEntity(identity) => {
                let key = stable_key_from_encoded(&encoded_namespace, identity.as_str());
                if let Some(stored) = existing.entities.get(&key) {
                    delete_entity(session, stored, batch, check)?;
                }
                if tracks_relation_endpoints {
                    entity_nodes.insert(identity.as_str(), None);
                }
            }
            GraphMutation::UpsertEntity(entity) => {
                let key = stable_key_from_encoded(&encoded_namespace, entity.identity.as_str());
                let node = if let Some(stored) = existing.entities.get(&key) {
                    if stored.namespace == batch.namespace
                        && stored.projection == batch.projection
                        && stored.entity == *entity
                    {
                        #[cfg(feature = "hotpath")]
                        hotpath::gauge!("graph_db.mutation.reused_entities_total").inc(1_u64);
                    } else {
                        replace_entity(session, stored, entity, batch, check)?;
                    }
                    stored.node
                } else {
                    create_entity(session, entity, batch, check)?
                };
                if tracks_relation_endpoints {
                    entity_nodes.insert(entity.identity.as_str(), Some(node));
                }
            }
            GraphMutation::UpsertRelation(relation) => {
                let relation_key =
                    stable_key_from_encoded(&encoded_namespace, relation.identity.as_str());
                let external = external_endpoints.get(&relation.identity);
                let from = external
                    .and_then(|(from, _)| *from)
                    .or_else(|| {
                        entity_node(&entity_nodes, existing, &encoded_namespace, &relation.from)
                    })
                    .ok_or_else(|| GraphDbError::invalid("relation source disappeared"))?;
                let to = external
                    .and_then(|(_, to)| *to)
                    .or_else(|| {
                        entity_node(&entity_nodes, existing, &encoded_namespace, &relation.to)
                    })
                    .ok_or_else(|| GraphDbError::invalid("relation target disappeared"))?;
                if let Some(stored) = existing.relations.get(&relation_key) {
                    if stored.projection == batch.projection
                        && stored.relation == *relation
                        && stored.source == from
                        && stored.target == to
                    {
                        #[cfg(feature = "hotpath")]
                        hotpath::gauge!("graph_db.mutation.reused_relations_total").inc(1_u64);
                        continue;
                    }
                    delete_relation(session, stored, batch, check)?;
                }
                let edge_properties =
                    edge_properties(&batch.namespace, &batch.projection, relation);
                let (property_names, property_values): (Vec<_>, Vec<_>) =
                    edge_properties.into_iter().unzip();
                let edge = session
                    .create_edge_with_props(
                        from,
                        to,
                        &relation_type_for_kind(&relation.kind),
                        property_names
                            .iter()
                            .map(String::as_str)
                            .zip(property_values),
                    )
                    .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
                let locator_properties =
                    relation_properties(&batch.namespace, &batch.projection, relation, edge)?;
                let locator_labels = relation_locator_labels(&batch.namespace, &batch.projection);
                tracked_create_node(session, &locator_labels, locator_properties, batch, check)?;
            }
        }
    }
    let projection_properties = projection_properties(&batch.namespace, &batch.projection, commit)?;
    match previous_projection {
        Some(node) => tracked_replace_node_properties(
            session,
            PROJECTION_LABEL,
            node,
            &projection_properties,
            &[],
            batch,
            check,
        )?,
        None => {
            tracked_create_node(
                session,
                &[PROJECTION_LABEL.to_owned()],
                projection_properties,
                batch,
                check,
            )?;
        }
    }
    if let Some((key, digest, input_digest)) = publication_record {
        let properties =
            publication_properties(&batch.namespace, key, digest, input_digest, commit)?;
        tracked_create_node(
            session,
            &[PUBLICATION_LABEL.to_owned()],
            properties,
            batch,
            check,
        )?;
    }
    let sequence = i64::try_from(commit.sequence)
        .map_err(|_| GraphDbError::unavailable("graph commit sequence exceeds i64"))?;
    tracked_set_property(
        session,
        state.marker,
        SEQUENCE_PROPERTY,
        Value::from(sequence),
        batch,
        check,
    )
}

fn create_entity(
    session: &Session,
    entity: &crate::GraphEntity,
    batch: &GraphWriteBatch,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<grafeo_common::types::NodeId, GraphDbError> {
    let labels = entity_labels(&batch.namespace, &batch.projection, &entity.labels);
    let properties = entity_properties(&batch.namespace, &batch.projection, entity);
    tracked_create_node(session, &labels, properties, batch, check)
}

fn entity_node(
    changes: &HashMap<&str, Option<grafeo_common::types::NodeId>>,
    existing: &ExistingBatchState,
    encoded_namespace: &str,
    identity: &GraphEntityId,
) -> Option<grafeo_common::types::NodeId> {
    if let Some(node) = changes.get(identity.as_str()) {
        return *node;
    }
    let key = stable_key_from_encoded(encoded_namespace, identity.as_str());
    existing
        .entities
        .get(&key)
        .map(|stored| stored.node)
        .or_else(|| existing.entity_locators.get(&key).map(|stored| stored.node))
}

fn replace_entity(
    session: &Session,
    previous: &StoredEntity,
    entity: &crate::GraphEntity,
    batch: &GraphWriteBatch,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    let properties = entity_properties(&batch.namespace, &batch.projection, entity);
    let prior_properties =
        entity_properties(&previous.namespace, &previous.projection, &previous.entity);
    let prior_labels = entity_labels(
        &previous.namespace,
        &previous.projection,
        &previous.entity.labels,
    );
    let labels = entity_labels(&batch.namespace, &batch.projection, &entity.labels);
    tracked_replace_node_properties(
        session,
        ENTITY_LABEL,
        previous.node,
        &properties,
        &prior_properties,
        batch,
        check,
    )?;
    // Grafeo 0.5.42's GQL SET path tracks the node write but does not persist
    // a vector parameter on an existing node. Replay vector scalars inside the
    // same tracked transaction so commit/rollback remains authoritative. HNSW
    // maintenance still happens only after commit in `GraphDb::apply_locked`.
    for (name, value) in &properties {
        if matches!(value, Value::Vector(_)) {
            session
                .set_node_property(previous.node, name, value.clone())
                .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        }
    }
    tracked_replace_labels(
        session,
        ENTITY_LABEL,
        previous.node,
        &prior_labels,
        &labels,
        batch,
        check,
    )
}

/// Deletes one tracked node through the session's direct-id path.
///
/// A GQL `MATCH … WHERE id(n) = …` statement enumerates and sorts every
/// visible node id before filtering, so a statement-per-row deletion is
/// O(store) per row — generation retirement at corpus scale exceeded every
/// deadline it ran under (issue #762). The session delete is the same
/// transactional, WAL-logged mutation at O(row).
///
/// The target was resolved from the store under the exclusive write gate in
/// this same transaction, so an absent node is store corruption, not a race.
fn tracked_delete_node(
    session: &Session,
    node: grafeo_common::types::NodeId,
    batch: &GraphWriteBatch,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    check_cancelled(batch, check)?;
    if !session.delete_node(node) {
        return Err(GraphDbError::Corrupt {
            message: "a tracked node resolved under the write gate refused deletion".to_owned(),
        });
    }
    check_cancelled(batch, check)
}

fn delete_entity(
    session: &Session,
    stored: &StoredEntity,
    batch: &GraphWriteBatch,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    tracked_delete_node(session, stored.node, batch, check)
}

fn delete_relation(
    session: &Session,
    stored: &StoredRelation,
    batch: &GraphWriteBatch,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    check_cancelled(batch, check)?;
    if !session.delete_edge(stored.edge) {
        return Err(GraphDbError::Corrupt {
            message: "a stored relation edge resolved under the write gate refused deletion"
                .to_owned(),
        });
    }
    tracked_delete_node(session, stored.locator, batch, check)
}

fn tracked_replace_node_properties(
    session: &Session,
    label: &str,
    node: grafeo_common::types::NodeId,
    properties: &[(String, Value)],
    previous: &[(String, Value)],
    batch: &GraphWriteBatch,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    let next: BTreeSet<_> = properties.iter().map(|(name, _)| name.as_str()).collect();
    let removed: Vec<_> = previous
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| !next.contains(name))
        .collect();
    let mut query = format!("MATCH (n:{label}) WHERE id(n) = {}", node.as_u64());
    if !removed.is_empty() {
        query.push_str(" REMOVE ");
        query.push_str(
            &removed
                .iter()
                .map(|name| format!("n.{name}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    let mut params = HashMap::new();
    if !properties.is_empty() {
        query.push_str(" SET ");
        let assignments = properties
            .iter()
            .enumerate()
            .map(|(index, (name, value))| {
                let parameter = format!("value_{index}");
                params.insert(parameter.clone(), value.clone());
                format!("n.{name} = ${parameter}")
            })
            .collect::<Vec<_>>();
        query.push_str(&assignments.join(", "));
    }
    execute_tracked(session, &query, params, batch, check)
}

fn tracked_replace_labels(
    session: &Session,
    anchor: &str,
    node: grafeo_common::types::NodeId,
    previous: &[String],
    labels: &[String],
    batch: &GraphWriteBatch,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    let previous: BTreeSet<_> = previous
        .iter()
        .filter(|label| label.as_str() != anchor)
        .collect();
    let labels: BTreeSet<_> = labels
        .iter()
        .filter(|label| label.as_str() != anchor)
        .collect();
    let mut query = format!("MATCH (n:{anchor}) WHERE id(n) = {}", node.as_u64());
    let removed: Vec<_> = previous.difference(&labels).collect();
    if !removed.is_empty() {
        query.push_str(" REMOVE ");
        query.push_str(
            &removed
                .iter()
                .map(|label| format!("n:{label}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    let added: Vec<_> = labels.difference(&previous).collect();
    if !added.is_empty() {
        query.push_str(" SET ");
        query.push_str(
            &added
                .iter()
                .map(|label| format!("n:{label}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if removed.is_empty() && added.is_empty() {
        return Ok(());
    }
    execute_tracked(session, &query, HashMap::new(), batch, check)
}

/// Sets one property through the session's direct-id path; the GQL
/// `MATCH … WHERE id(n) = …` equivalent scans every visible node id per
/// statement, which this marker bump would otherwise pay on every batch.
fn tracked_set_property(
    session: &Session,
    node: grafeo_common::types::NodeId,
    property: &str,
    value: Value,
    batch: &GraphWriteBatch,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    check_cancelled(batch, check)?;
    session
        .set_node_property(node, property, value)
        .map_err(map_commit_error)?;
    check_cancelled(batch, check)
}

fn tracked_create_node(
    session: &Session,
    labels: &[String],
    properties: Vec<(String, Value)>,
    batch: &GraphWriteBatch,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<grafeo_common::types::NodeId, GraphDbError> {
    if !properties.iter().any(|(name, _)| {
        matches!(
            name.as_str(),
            ENTITY_KEY_PROPERTY
                | RELATION_KEY_PROPERTY
                | PROJECTION_KEY_PROPERTY
                | PUBLICATION_KEY_PROPERTY
        )
    }) {
        return Err(GraphDbError::Corrupt {
            message: "tracked Grafeo node creation has no native locator".to_owned(),
        });
    }
    check_cancelled(batch, check)?;
    let labels = labels.iter().map(String::as_str).collect::<Vec<_>>();
    let (property_names, property_values): (Vec<_>, Vec<_>) = properties.into_iter().unzip();
    let node = session
        .create_node_with_props(
            &labels,
            property_names
                .iter()
                .map(String::as_str)
                .zip(property_values),
        )
        .map_err(map_commit_error)?;
    check_cancelled(batch, check)?;
    Ok(node)
}

fn execute_tracked(
    session: &Session,
    query: &str,
    params: HashMap<String, Value>,
    batch: &GraphWriteBatch,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    check_cancelled(batch, check)?;
    session
        .execute_with_params(query, params)
        .map_err(map_commit_error)?;
    check_cancelled(batch, check)
}

fn validate_references(
    database: &GrafeoDB,
    batch: &GraphWriteBatch,
    existing: &ExistingBatchState,
    endpoint_namespaces: &RelationEndpointNamespaces,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<ResolvedRelationEndpoints, GraphDbError> {
    let encoded_namespace = encoded_namespace_key(&batch.namespace);
    let entity_count = batch
        .mutations
        .iter()
        .filter(|mutation| {
            matches!(
                mutation,
                GraphMutation::DeleteEntity(_) | GraphMutation::UpsertEntity(_)
            )
        })
        .count();
    let relation_count = batch.mutations.len().saturating_sub(entity_count);
    let mut entities = HashMap::<&str, EntityChange>::with_capacity(entity_count);
    let mut relations = HashMap::<&str, RelationChange>::with_capacity(relation_count);
    let mut mutation_keys = HashSet::with_capacity(batch.mutations.len());
    for mutation in &batch.mutations {
        check_cancelled(batch, check)?;
        let (kind, identity) = mutation.sort_key();
        if !mutation_keys.insert((kind, identity)) {
            return Err(GraphDbError::invalid("batch repeats a graph mutation"));
        }
        match mutation {
            GraphMutation::DeleteRelation(identity) => {
                if let Some(owner) = relation_owner(
                    &relations,
                    &existing.relations,
                    &encoded_namespace,
                    identity,
                ) && owner != batch.projection
                {
                    return Err(GraphDbError::conflict("mutation.validate_references"));
                }
                relations.insert(identity.as_str(), None);
            }
            GraphMutation::DeleteEntity(identity) => {
                if let Some(owner) = entity_owner(&entities, existing, &encoded_namespace, identity)
                    && owner != batch.projection
                {
                    return Err(GraphDbError::conflict("mutation.validate_references"));
                }
                entities.insert(identity.as_str(), None);
            }
            GraphMutation::UpsertEntity(entity) => {
                if let Some(owner) =
                    entity_owner(&entities, existing, &encoded_namespace, &entity.identity)
                    && owner != batch.projection
                {
                    return Err(GraphDbError::conflict("mutation.validate_references"));
                }
                entities.insert(entity.identity.as_str(), Some(batch.projection.clone()));
            }
            GraphMutation::UpsertRelation(relation) => {
                if let Some(owner) = relation_owner(
                    &relations,
                    &existing.relations,
                    &encoded_namespace,
                    &relation.identity,
                ) && owner != batch.projection
                {
                    return Err(GraphDbError::conflict("mutation.validate_references"));
                }
                relations.insert(
                    relation.identity.as_str(),
                    Some((
                        batch.projection.clone(),
                        relation.from.clone(),
                        relation.to.clone(),
                    )),
                );
            }
        }
    }
    for mutation in &batch.mutations {
        let GraphMutation::UpsertRelation(relation) = mutation else {
            continue;
        };
        let endpoint_namespaces_for_relation = endpoint_namespaces.get(&relation.identity);
        for (endpoint, endpoint_namespace) in [
            (
                &relation.from,
                endpoint_namespaces_for_relation.map(|(from, _)| from),
            ),
            (
                &relation.to,
                endpoint_namespaces_for_relation.map(|(_, to)| to),
            ),
        ] {
            if endpoint_namespace.is_none_or(|namespace| namespace == &batch.namespace)
                && entity_owner(&entities, existing, &encoded_namespace, endpoint).is_none()
            {
                return Err(GraphDbError::invalid(format!(
                    "relation endpoint `{endpoint}` does not exist in namespace `{}`",
                    batch.namespace
                )));
            }
        }
    }
    let mut external_endpoints = BTreeMap::new();
    for mutation in &batch.mutations {
        let GraphMutation::UpsertRelation(relation) = mutation else {
            continue;
        };
        let Some((from_namespace, to_namespace)) = endpoint_namespaces.get(&relation.identity)
        else {
            continue;
        };
        let from = resolve_generation_endpoint(
            database,
            &batch.namespace,
            from_namespace,
            &relation.from,
            &entities,
            existing,
            &encoded_namespace,
        )?;
        let to = resolve_generation_endpoint(
            database,
            &batch.namespace,
            to_namespace,
            &relation.to,
            &entities,
            existing,
            &encoded_namespace,
        )?;
        external_endpoints.insert(relation.identity.clone(), (from, to));
    }
    for mutation in &batch.mutations {
        let GraphMutation::DeleteEntity(identity) = mutation else {
            continue;
        };
        if entities.get(identity.as_str()).is_some_and(Option::is_some) {
            continue;
        }
        let key = stable_key_from_encoded(&encoded_namespace, identity.as_str());
        let Some(entity) = existing.entities.get(&key) else {
            continue;
        };
        for relation in relation_references_for_entity(database, entity.node)? {
            let logical = relations
                .get(relation.identity.as_str())
                .cloned()
                .unwrap_or(Some((relation.projection, relation.from, relation.to)));
            if let Some((_, from, to)) = logical
                && (from == *identity || to == *identity)
            {
                return Err(GraphDbError::invalid(format!(
                    "entity `{identity}` remains referenced by relation `{}`",
                    relation.identity
                )));
            }
        }
    }
    Ok(external_endpoints)
}

fn resolve_generation_endpoint(
    database: &GrafeoDB,
    candidate_namespace: &GraphNamespace,
    endpoint_namespace: &GraphNamespace,
    identity: &GraphEntityId,
    changes: &HashMap<&str, EntityChange>,
    existing: &ExistingBatchState,
    encoded_candidate_namespace: &str,
) -> Result<Option<grafeo_common::types::NodeId>, GraphDbError> {
    if endpoint_namespace == candidate_namespace {
        if entity_owner(changes, existing, encoded_candidate_namespace, identity).is_none() {
            return Err(GraphDbError::invalid(format!(
                "local generation endpoint `{identity}` does not exist"
            )));
        }
        return Ok(None);
    }
    crate::state::load_entity_locator(database, endpoint_namespace, identity)?
        .map(|stored| stored.node)
        .map(Some)
        .ok_or_else(|| {
            GraphDbError::invalid(format!(
                "dependency generation endpoint `{identity}` does not exist"
            ))
        })
}

fn entity_owner(
    changes: &HashMap<&str, EntityChange>,
    existing: &ExistingBatchState,
    encoded_namespace: &str,
    identity: &GraphEntityId,
) -> Option<GraphProjectionId> {
    if let Some(owner) = changes.get(identity.as_str()) {
        return owner.clone();
    }
    let key = stable_key_from_encoded(encoded_namespace, identity.as_str());
    existing
        .entities
        .get(&key)
        .map(|stored| stored.projection.clone())
        .or_else(|| {
            existing
                .entity_locators
                .get(&key)
                .map(|stored| stored.projection.clone())
        })
}

fn relation_owner(
    changes: &HashMap<&str, RelationChange>,
    existing: &BTreeMap<String, StoredRelation>,
    encoded_namespace: &str,
    identity: &crate::GraphRelationId,
) -> Option<GraphProjectionId> {
    if let Some(relation) = changes.get(identity.as_str()) {
        return relation.as_ref().map(|(owner, _, _)| owner.clone());
    }
    let key = stable_key_from_encoded(encoded_namespace, identity.as_str());
    existing.get(&key).map(|stored| stored.projection.clone())
}

fn check_cancelled(
    batch: &GraphWriteBatch,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    check()?;
    if batch.cancellation.is_cancelled() {
        Err(GraphDbError::Cancelled)
    } else {
        Ok(())
    }
}

fn rollback_or_poison(
    session: &mut Session,
    error: GraphDbError,
    poisoned: &AtomicBool,
) -> GraphDbError {
    match session.rollback() {
        Ok(()) => error,
        Err(rollback_error) => {
            poisoned.store(true, Ordering::Release);
            rollback_failure("pre-commit", error, rollback_error)
        }
    }
}

fn map_commit_error(error: grafeo_common::utils::error::Error) -> GraphDbError {
    use grafeo_common::utils::error::ErrorCode;
    match error.error_code() {
        ErrorCode::TransactionConflict
        | ErrorCode::TransactionSerialization
        | ErrorCode::TransactionDeadlock => GraphDbError::conflict("mutation.map_commit_error"),
        _ => GraphDbError::unavailable(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use grafeo_common::types::Value;
    use grafeo_engine::GrafeoDB;

    use super::tracked_create_node;
    use crate::schema::{FORMAT_LABEL, SEQUENCE_PROPERTY};
    use crate::state::FormatState;
    use crate::{
        GraphDbError, GraphNamespace, GraphProjectionId, GraphWatermark, GraphWriteBatch,
        NeverCancelled, SourceGeneration,
    };

    #[test]
    fn native_create_rejects_missing_locator_without_mutating_graph_or_sequence() {
        let database = GrafeoDB::new_in_memory();
        database
            .session()
            .create_node_with_props(&[FORMAT_LABEL], [(SEQUENCE_PROPERTY, Value::from(7_i64))])
            .unwrap();
        let before = FormatState::load(&database).unwrap();
        let batch = GraphWriteBatch::new(
            GraphNamespace::new("project").unwrap(),
            GraphProjectionId::new("code").unwrap(),
            SourceGeneration::new("generation").unwrap(),
            GraphWatermark::new("watermark").unwrap(),
            Vec::new(),
            Arc::new(NeverCancelled),
        )
        .unwrap();
        let mut session = database.session();
        session.begin_transaction().unwrap();

        assert_eq!(
            tracked_create_node(
                &session,
                &["Broken".to_owned()],
                vec![("unindexed".to_owned(), Value::from("value"))],
                &batch,
                &|| Ok(()),
            ),
            Err(GraphDbError::Corrupt {
                message: "tracked Grafeo node creation has no native locator".to_owned(),
            })
        );
        session.rollback().unwrap();

        assert!(database.graph_store().nodes_by_label("Broken").is_empty());
        assert_eq!(
            FormatState::load(&database).unwrap().sequence,
            before.sequence
        );
    }
}
