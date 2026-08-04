use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};

use grafeo_common::types::Value;
use grafeo_engine::{GrafeoDB, Session};

use crate::error::rollback_failure;
use crate::schema::{
    ENTITY_KEY_PROPERTY, ENTITY_LABEL, FORMAT_LABEL, PROJECTION_KEY_PROPERTY, PROJECTION_LABEL,
    PUBLICATION_KEY_PROPERTY, PUBLICATION_LABEL, RELATION_KEY_PROPERTY, RELATION_LABEL,
    SEQUENCE_PROPERTY, edge_properties, entity_key_label, entity_labels, entity_properties,
    projection_properties, projection_state_label, publication_key_label, publication_properties,
    relation_locator_labels, relation_properties, relation_type_for_kind, stable_key,
};
use crate::state::{
    FormatState, StoredEntity, StoredRelation, latest_projection, load_entity, load_relation,
    relations_for_entity,
};
use crate::{
    GraphCommit, GraphDbError, GraphEntityId, GraphIdempotencyKey, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphWriteBatch,
};

type EntityChange = Option<GraphProjectionId>;
type RelationChange = Option<(GraphProjectionId, GraphEntityId, GraphEntityId)>;

pub(crate) fn apply(
    database: &GrafeoDB,
    state: &mut FormatState,
    batch: GraphWriteBatch,
    digest: String,
    publication_record: Option<(GraphIdempotencyKey, String)>,
    poisoned: &AtomicBool,
) -> Result<GraphCommit, GraphDbError> {
    validate_references(database, &batch)?;
    let sequence = state
        .sequence
        .checked_add(1)
        .ok_or_else(|| GraphDbError::unavailable("graph commit sequence exhausted"))?;
    let commit = GraphCommit {
        sequence,
        source_generation: batch.source_generation.clone(),
        watermark: batch.next_watermark.clone(),
        digest,
    };
    let previous_projection = latest_projection(database, &batch.namespace, &batch.projection)?;
    let mut session = database.session();
    session
        .begin_transaction()
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    let result = apply_in_transaction(
        database,
        &session,
        state,
        &batch,
        &commit,
        previous_projection
            .as_ref()
            .map(|projection| projection.node),
        publication_record.as_ref(),
    );
    if let Err(error) = result {
        return Err(rollback_or_poison(&mut session, error, poisoned));
    }
    if batch.cancellation.is_cancelled() {
        return Err(rollback_or_poison(
            &mut session,
            GraphDbError::Cancelled,
            poisoned,
        ));
    }
    session.commit().map_err(map_commit_error)?;
    state.sequence = sequence;
    Ok(commit)
}

#[allow(clippy::too_many_arguments)]
fn apply_in_transaction(
    database: &GrafeoDB,
    session: &Session,
    state: &FormatState,
    batch: &GraphWriteBatch,
    commit: &GraphCommit,
    previous_projection: Option<grafeo_common::types::NodeId>,
    publication_record: Option<&(GraphIdempotencyKey, String)>,
) -> Result<(), GraphDbError> {
    let mut entity_nodes = BTreeMap::<String, Option<grafeo_common::types::NodeId>>::new();
    for mutation in &batch.mutations {
        check_cancelled(batch)?;
        match mutation {
            GraphMutation::DeleteRelation(identity) => {
                if let Some(stored) = load_relation(database, &batch.namespace, identity)? {
                    delete_relation(session, &stored, batch)?;
                }
            }
            GraphMutation::DeleteEntity(identity) => {
                if let Some(stored) = load_entity(database, &batch.namespace, identity)? {
                    delete_entity(session, &stored, batch)?;
                }
                entity_nodes.insert(stable_key(&batch.namespace, identity.as_str()), None);
            }
            GraphMutation::UpsertEntity(entity) => {
                let node = if let Some(stored) =
                    load_entity(database, &batch.namespace, &entity.identity)?
                {
                    replace_entity(session, &stored, entity, batch)?;
                    stored.node
                } else {
                    create_entity(session, entity, batch)?
                };
                entity_nodes.insert(
                    stable_key(&batch.namespace, entity.identity.as_str()),
                    Some(node),
                );
            }
            GraphMutation::UpsertRelation(relation) => {
                if let Some(stored) = load_relation(database, &batch.namespace, &relation.identity)?
                {
                    delete_relation(session, &stored, batch)?;
                }
                let from = entity_node(database, &entity_nodes, &batch.namespace, &relation.from)?
                    .ok_or_else(|| GraphDbError::invalid("relation source disappeared"))?;
                let to = entity_node(database, &entity_nodes, &batch.namespace, &relation.to)?
                    .ok_or_else(|| GraphDbError::invalid("relation target disappeared"))?;
                let edge_properties =
                    edge_properties(&batch.namespace, &batch.projection, relation);
                let edge = session
                    .create_edge_with_props(
                        from,
                        to,
                        &relation_type_for_kind(&relation.kind),
                        edge_properties
                            .iter()
                            .map(|(name, value)| (name.as_str(), value.clone())),
                    )
                    .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
                let locator_properties =
                    relation_properties(&batch.namespace, &batch.projection, relation, edge)?;
                let locator_labels =
                    relation_locator_labels(&batch.namespace, &batch.projection, relation, edge);
                tracked_create_node(session, &locator_labels, &locator_properties, batch)?;
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
        )?,
        None => {
            let label = projection_state_label(&batch.namespace, &batch.projection);
            tracked_create_node(
                session,
                &[PROJECTION_LABEL.to_owned(), label],
                &projection_properties,
                batch,
            )?;
        }
    }
    if let Some((key, digest)) = publication_record {
        let properties = publication_properties(&batch.namespace, key, digest, commit)?;
        let label = publication_key_label(&batch.namespace, key);
        tracked_create_node(
            session,
            &[PUBLICATION_LABEL.to_owned(), label],
            &properties,
            batch,
        )?;
    }
    let sequence = i64::try_from(commit.sequence)
        .map_err(|_| GraphDbError::unavailable("graph commit sequence exceeds i64"))?;
    tracked_set_property(
        session,
        FORMAT_LABEL,
        state.marker,
        SEQUENCE_PROPERTY,
        Value::from(sequence),
        batch,
    )
}

fn create_entity(
    session: &Session,
    entity: &crate::GraphEntity,
    batch: &GraphWriteBatch,
) -> Result<grafeo_common::types::NodeId, GraphDbError> {
    let labels = entity_labels(&batch.namespace, &batch.projection, &entity.labels);
    let mut labels = labels;
    labels.push(entity_key_label(&batch.namespace, &entity.identity));
    let properties = entity_properties(&batch.namespace, &batch.projection, entity);
    tracked_create_node(session, &labels, &properties, batch)
}

fn entity_node(
    database: &GrafeoDB,
    changes: &BTreeMap<String, Option<grafeo_common::types::NodeId>>,
    namespace: &GraphNamespace,
    identity: &GraphEntityId,
) -> Result<Option<grafeo_common::types::NodeId>, GraphDbError> {
    let key = stable_key(namespace, identity.as_str());
    if let Some(node) = changes.get(&key) {
        return Ok(*node);
    }
    Ok(load_entity(database, namespace, identity)?.map(|stored| stored.node))
}

fn replace_entity(
    session: &Session,
    previous: &StoredEntity,
    entity: &crate::GraphEntity,
    batch: &GraphWriteBatch,
) -> Result<(), GraphDbError> {
    let properties = entity_properties(&batch.namespace, &batch.projection, entity);
    let prior_properties =
        entity_properties(&previous.namespace, &previous.projection, &previous.entity);
    let prior_labels = entity_labels(
        &previous.namespace,
        &previous.projection,
        &previous.entity.labels,
    );
    let mut prior_labels = prior_labels;
    prior_labels.push(entity_key_label(
        &previous.namespace,
        &previous.entity.identity,
    ));
    let mut labels = entity_labels(&batch.namespace, &batch.projection, &entity.labels);
    labels.push(entity_key_label(&batch.namespace, &entity.identity));
    tracked_replace_node_properties(
        session,
        ENTITY_LABEL,
        previous.node,
        &properties,
        &prior_properties,
        batch,
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
    )
}

fn delete_entity(
    session: &Session,
    stored: &StoredEntity,
    batch: &GraphWriteBatch,
) -> Result<(), GraphDbError> {
    execute_tracked(
        session,
        &format!(
            "MATCH (n:{ENTITY_LABEL}) WHERE id(n) = {} DELETE n",
            stored.node.as_u64()
        ),
        HashMap::new(),
        batch,
    )
}

fn delete_relation(
    session: &Session,
    stored: &StoredRelation,
    batch: &GraphWriteBatch,
) -> Result<(), GraphDbError> {
    execute_tracked(
        session,
        &format!(
            "MATCH ()-[r:{}]->() WHERE id(r) = {} DELETE r",
            relation_type_for_kind(&stored.relation.kind),
            stored.edge.as_u64()
        ),
        HashMap::new(),
        batch,
    )?;
    execute_tracked(
        session,
        &format!(
            "MATCH (n:{RELATION_LABEL}) WHERE id(n) = {} DELETE n",
            stored.locator.as_u64()
        ),
        HashMap::new(),
        batch,
    )
}

fn tracked_replace_node_properties(
    session: &Session,
    label: &str,
    node: grafeo_common::types::NodeId,
    properties: &[(String, Value)],
    previous: &[(String, Value)],
    batch: &GraphWriteBatch,
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
    execute_tracked(session, &query, params, batch)
}

fn tracked_replace_labels(
    session: &Session,
    anchor: &str,
    node: grafeo_common::types::NodeId,
    previous: &[String],
    labels: &[String],
    batch: &GraphWriteBatch,
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
    execute_tracked(session, &query, HashMap::new(), batch)
}

fn tracked_set_property(
    session: &Session,
    label: &str,
    node: grafeo_common::types::NodeId,
    property: &str,
    value: Value,
    batch: &GraphWriteBatch,
) -> Result<(), GraphDbError> {
    let query = format!(
        "MATCH (n:{label}) WHERE id(n) = {} SET n.{property} = $value",
        node.as_u64()
    );
    execute_tracked(
        session,
        &query,
        HashMap::from([("value".to_owned(), value)]),
        batch,
    )
}

fn tracked_create_node(
    session: &Session,
    labels: &[String],
    properties: &[(String, Value)],
    batch: &GraphWriteBatch,
) -> Result<grafeo_common::types::NodeId, GraphDbError> {
    let mut params = HashMap::new();
    let assignments = properties
        .iter()
        .enumerate()
        .map(|(index, (name, value))| {
            let parameter = format!("value_{index}");
            params.insert(parameter.clone(), value.clone());
            format!("{name}: ${parameter}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "INSERT (n:{} {{{assignments}}}) RETURN id(n)",
        labels.join(":")
    );
    check_cancelled(batch)?;
    session
        .execute_with_params(&query, params)
        .map_err(map_commit_error)?;
    check_cancelled(batch)?;
    let (locator_name, locator_value) = properties
        .iter()
        .find(|(name, _)| {
            matches!(
                name.as_str(),
                ENTITY_KEY_PROPERTY
                    | RELATION_KEY_PROPERTY
                    | PROJECTION_KEY_PROPERTY
                    | PUBLICATION_KEY_PROPERTY
            )
        })
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "tracked Grafeo node creation has no native locator".to_owned(),
        })?;
    let lookup = format!(
        "MATCH (n:{} {{{locator_name}: $locator}}) RETURN id(n)",
        labels[0]
    );
    let result = session
        .execute_with_params(
            &lookup,
            HashMap::from([("locator".to_owned(), locator_value.clone())]),
        )
        .map_err(map_commit_error)?;
    let raw = result
        .rows()
        .first()
        .and_then(|row| row.first())
        .and_then(Value::as_int64)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "tracked Grafeo node creation returned no native identity".to_owned(),
        })?;
    let raw = u64::try_from(raw).map_err(|_| GraphDbError::Corrupt {
        message: "tracked Grafeo node creation returned a negative identity".to_owned(),
    })?;
    Ok(grafeo_common::types::NodeId::new(raw))
}

fn execute_tracked(
    session: &Session,
    query: &str,
    params: HashMap<String, Value>,
    batch: &GraphWriteBatch,
) -> Result<(), GraphDbError> {
    check_cancelled(batch)?;
    session
        .execute_with_params(query, params)
        .map_err(map_commit_error)?;
    check_cancelled(batch)
}

fn validate_references(database: &GrafeoDB, batch: &GraphWriteBatch) -> Result<(), GraphDbError> {
    let mut entities = BTreeMap::<String, EntityChange>::new();
    let mut relations = BTreeMap::<String, RelationChange>::new();
    let mut mutation_keys = BTreeSet::new();
    for mutation in &batch.mutations {
        let (kind, identity) = mutation.sort_key();
        if !mutation_keys.insert((kind, identity.to_owned())) {
            return Err(GraphDbError::invalid("batch repeats a graph mutation"));
        }
        match mutation {
            GraphMutation::DeleteRelation(identity) => {
                let key = stable_key(&batch.namespace, identity.as_str());
                if let Some(owner) =
                    relation_owner(database, &relations, &batch.namespace, identity)?
                    && owner != batch.projection
                {
                    return Err(GraphDbError::Conflict);
                }
                relations.insert(key, None);
            }
            GraphMutation::DeleteEntity(identity) => {
                let key = stable_key(&batch.namespace, identity.as_str());
                if let Some(owner) = entity_owner(database, &entities, &batch.namespace, identity)?
                    && owner != batch.projection
                {
                    return Err(GraphDbError::Conflict);
                }
                entities.insert(key, None);
            }
            GraphMutation::UpsertEntity(entity) => {
                let key = stable_key(&batch.namespace, entity.identity.as_str());
                if let Some(owner) =
                    entity_owner(database, &entities, &batch.namespace, &entity.identity)?
                    && owner != batch.projection
                {
                    return Err(GraphDbError::Conflict);
                }
                entities.insert(key, Some(batch.projection.clone()));
            }
            GraphMutation::UpsertRelation(relation) => {
                let key = stable_key(&batch.namespace, relation.identity.as_str());
                if let Some(owner) =
                    relation_owner(database, &relations, &batch.namespace, &relation.identity)?
                    && owner != batch.projection
                {
                    return Err(GraphDbError::Conflict);
                }
                relations.insert(
                    key,
                    Some((
                        batch.projection.clone(),
                        relation.from.clone(),
                        relation.to.clone(),
                    )),
                );
            }
        }
    }
    for (_, from, to) in relations.values().flatten() {
        for endpoint in [from, to] {
            if entity_owner(database, &entities, &batch.namespace, endpoint)?.is_none() {
                return Err(GraphDbError::invalid(format!(
                    "relation endpoint `{endpoint}` does not exist in namespace `{}`",
                    batch.namespace
                )));
            }
        }
    }
    for (key, owner) in &entities {
        if owner.is_some() {
            continue;
        }
        let identity = key_identity(key, "entity")?;
        let identity = GraphEntityId::new(identity)?;
        let Some(entity) = load_entity(database, &batch.namespace, &identity)? else {
            continue;
        };
        for relation in relations_for_entity(database, entity.node)? {
            let relation_key = stable_key(&batch.namespace, relation.relation.identity.as_str());
            let logical = relations.get(&relation_key).cloned().unwrap_or(Some((
                relation.projection,
                relation.relation.from,
                relation.relation.to,
            )));
            if let Some((_, from, to)) = logical
                && (from == identity || to == identity)
            {
                return Err(GraphDbError::invalid(format!(
                    "entity `{identity}` remains referenced by relation `{}`",
                    relation.relation.identity
                )));
            }
        }
    }
    Ok(())
}

fn entity_owner(
    database: &GrafeoDB,
    changes: &BTreeMap<String, EntityChange>,
    namespace: &GraphNamespace,
    identity: &GraphEntityId,
) -> Result<Option<GraphProjectionId>, GraphDbError> {
    let key = stable_key(namespace, identity.as_str());
    if let Some(owner) = changes.get(&key) {
        return Ok(owner.clone());
    }
    Ok(load_entity(database, namespace, identity)?.map(|stored| stored.projection))
}

fn relation_owner(
    database: &GrafeoDB,
    changes: &BTreeMap<String, RelationChange>,
    namespace: &GraphNamespace,
    identity: &crate::GraphRelationId,
) -> Result<Option<GraphProjectionId>, GraphDbError> {
    let key = stable_key(namespace, identity.as_str());
    if let Some(relation) = changes.get(&key) {
        return Ok(relation.as_ref().map(|(owner, _, _)| owner.clone()));
    }
    Ok(load_relation(database, namespace, identity)?.map(|stored| stored.projection))
}

fn key_identity(key: &str, description: &str) -> Result<String, GraphDbError> {
    let (_, encoded) = key.split_once(':').ok_or_else(|| GraphDbError::Corrupt {
        message: format!("native {description} key is malformed"),
    })?;
    let bytes = hex::decode(encoded).map_err(|error| GraphDbError::Corrupt {
        message: format!("native {description} key is malformed: {error}"),
    })?;
    String::from_utf8(bytes).map_err(|error| GraphDbError::Corrupt {
        message: format!("native {description} key is not UTF-8: {error}"),
    })
}

fn check_cancelled(batch: &GraphWriteBatch) -> Result<(), GraphDbError> {
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
        | ErrorCode::TransactionDeadlock => GraphDbError::Conflict,
        _ => GraphDbError::unavailable(error.to_string()),
    }
}
