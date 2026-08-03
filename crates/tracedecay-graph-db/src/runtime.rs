use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use grafeo_common::types::{EdgeId, NodeId, Value};
use grafeo_engine::GrafeoDB;
use serde::{Deserialize, Serialize};

use crate::location::ValidatedOpen;
use crate::traversal;
use crate::vector;
use crate::{
    GraphCommit, GraphDbError, GraphDbOpenOptions, GraphDurability, GraphEntity, GraphEntityId,
    GraphIdempotencyKey, GraphMutation, GraphNamespace, GraphProjectionId, GraphProperty,
    GraphPropertyName, GraphPublication, GraphRelation, GraphRelationId, GraphWatermark,
    GraphWriteBatch, ProjectionReplacement, TraversalRequest, TraversalResult, VectorSearchRequest,
    VectorSearchResult,
};

const FORMAT_LABEL: &str = "__tracedecay_graph_db_format";
const FORMAT_VERSION_PROPERTY: &str = "__tracedecay_graph_db_version";
const SEQUENCE_PROPERTY: &str = "__tracedecay_graph_db_sequence";
pub(crate) const ENTITY_LABEL: &str = "__tracedecay_graph_db_entity";
const COMMIT_LABEL: &str = "__tracedecay_graph_db_commit";
const PUBLICATION_LABEL: &str = "__tracedecay_graph_db_publication";
const RELATION_TYPE: &str = "__tracedecay_graph_db_relation";
const PAYLOAD_PROPERTY: &str = "__tracedecay_graph_db_payload";

pub struct GraphDb {
    inner: Arc<Inner>,
}

struct Inner {
    database: RwLock<Option<GrafeoDB>>,
    durability: GraphDurability,
    closed: AtomicBool,
    poisoned: AtomicBool,
}

pub struct GraphSnapshot {
    database: Arc<GrafeoDB>,
}

impl std::fmt::Debug for GraphDb {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GraphDb")
            .field("closed", &self.inner.closed.load(Ordering::Acquire))
            .field("poisoned", &self.inner.poisoned.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for GraphSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GraphSnapshot")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct StoredEntity {
    pub(crate) namespace: GraphNamespace,
    pub(crate) projection: GraphProjectionId,
    pub(crate) entity: GraphEntity,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct StoredRelation {
    pub(crate) namespace: GraphNamespace,
    pub(crate) projection: GraphProjectionId,
    pub(crate) relation: GraphRelation,
}

type StableKey = (String, String);
pub(crate) type EntityIndex = BTreeMap<StableKey, (NodeId, StoredEntity)>;
type RelationIndex = BTreeMap<StableKey, (EdgeId, StoredRelation)>;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredCommit {
    namespace: GraphNamespace,
    projection: GraphProjectionId,
    commit: GraphCommit,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredPublication {
    namespace: GraphNamespace,
    key: GraphIdempotencyKey,
    digest: String,
    commit: GraphCommit,
}

struct LoadedState {
    marker: NodeId,
    sequence: u64,
    entities: EntityIndex,
    relations: RelationIndex,
    commits: Vec<StoredCommit>,
    publications: Vec<StoredPublication>,
}

enum PreparedMutation {
    DeleteRelation(GraphRelationId),
    DeleteEntity(GraphEntityId),
    UpsertEntity(GraphEntity, String),
    UpsertRelation(GraphRelation, String),
}

impl GraphDb {
    pub fn open(options: GraphDbOpenOptions) -> Result<Self, GraphDbError> {
        let cancellation = Arc::clone(&options.cancellation);
        let validated = options.validate()?;
        let database = GrafeoDB::with_config(validated.config.clone()).map_err(|error| {
            if validated.preexisting_file {
                GraphDbError::Corrupt {
                    message: error.to_string(),
                }
            } else {
                GraphDbError::unavailable(error.to_string())
            }
        })?;
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        validate_or_initialize_format(&database, &validated)?;
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                database: RwLock::new(Some(database)),
                durability: validated.durability,
                closed: AtomicBool::new(false),
                poisoned: AtomicBool::new(false),
            }),
        })
    }

    pub fn snapshot(&self) -> Result<GraphSnapshot, GraphDbError> {
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let bytes = database
            .export_snapshot()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let snapshot = GrafeoDB::import_snapshot(&bytes)
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        Ok(GraphSnapshot {
            database: Arc::new(snapshot),
        })
    }

    pub fn apply(&self, mut batch: GraphWriteBatch) -> Result<GraphCommit, GraphDbError> {
        let digest = batch.validate_and_digest()?;
        let guard = self.write_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let state = load_state(database)?;
        self.apply_locked(database, state, batch, digest, None)
    }

    pub fn replace_projection(
        &self,
        replacement: ProjectionReplacement,
    ) -> Result<GraphCommit, GraphDbError> {
        if replacement.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        for entity in &replacement.entities {
            entity.validate()?;
        }
        for relation in &replacement.relations {
            relation.validate()?;
        }
        let guard = self.write_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let state = load_state(database)?;
        let mut mutations = Vec::new();
        for (_, stored) in state.relations.values() {
            if stored.namespace == replacement.namespace
                && stored.projection == replacement.projection
            {
                mutations.push(GraphMutation::DeleteRelation(
                    stored.relation.identity.clone(),
                ));
            }
        }
        for (_, stored) in state.entities.values() {
            if stored.namespace == replacement.namespace
                && stored.projection == replacement.projection
            {
                mutations.push(GraphMutation::DeleteEntity(stored.entity.identity.clone()));
            }
        }
        mutations.extend(
            replacement
                .entities
                .into_iter()
                .map(GraphMutation::UpsertEntity),
        );
        mutations.extend(
            replacement
                .relations
                .into_iter()
                .map(GraphMutation::UpsertRelation),
        );
        let mut batch = GraphWriteBatch::new(
            replacement.namespace,
            replacement.projection,
            replacement.source_generation,
            replacement.next_watermark,
            mutations,
            replacement.cancellation,
        )?;
        let digest = batch.validate_and_digest()?;
        self.apply_locked(database, state, batch, digest, None)
    }

    pub fn publish(&self, mut publication: GraphPublication) -> Result<GraphCommit, GraphDbError> {
        let publication_digest = publication.validate_and_digest()?;
        let batch_digest = publication.batch.validate_and_digest()?;
        let guard = self.write_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let state = load_state(database)?;
        if let Some(existing) = state.publications.iter().find(|existing| {
            existing.namespace == publication.namespace
                && existing.key == publication.idempotency_key
        }) {
            return if existing.digest == publication_digest {
                Ok(existing.commit.clone())
            } else {
                Err(GraphDbError::Conflict)
            };
        }
        let current = latest_watermark(
            &state,
            &publication.namespace,
            &publication.batch.projection,
        );
        if current.as_ref() != publication.expected_watermark.as_ref() {
            return Err(GraphDbError::Conflict);
        }
        let publication_record = (publication.idempotency_key, publication_digest);
        self.apply_locked(
            database,
            state,
            publication.batch,
            batch_digest,
            Some(publication_record),
        )
    }

    pub fn traverse(&self, request: TraversalRequest) -> Result<TraversalResult, GraphDbError> {
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        traversal::traverse(database, request)
    }

    pub fn vector_search(
        &self,
        request: VectorSearchRequest,
    ) -> Result<VectorSearchResult, GraphDbError> {
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        vector::vector_search(database, request)
    }

    pub fn close(&self) -> Result<(), GraphDbError> {
        if self.inner.poisoned.load(Ordering::Acquire) {
            return Err(durability_uncertain());
        }
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let mut guard = self
            .inner
            .database
            .write()
            .map_err(|_| GraphDbError::unavailable("graph database write lock is poisoned"))?;
        let Some(database) = guard.take() else {
            return Ok(());
        };
        if let Err(error) = database.close() {
            self.inner.poisoned.store(true, Ordering::Release);
            return Err(GraphDbError::DurabilityUncertain {
                message: error.to_string(),
            });
        }
        Ok(())
    }

    fn apply_locked(
        &self,
        database: &GrafeoDB,
        mut state: LoadedState,
        batch: GraphWriteBatch,
        digest: String,
        publication: Option<(GraphIdempotencyKey, String)>,
    ) -> Result<GraphCommit, GraphDbError> {
        validate_references(&state, &batch)?;
        let prepared = prepare_mutations(&batch)?;
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
        let stored_commit = StoredCommit {
            namespace: batch.namespace.clone(),
            projection: batch.projection.clone(),
            commit: commit.clone(),
        };
        let commit_payload = serialize_payload(&stored_commit)?;
        let publication_payload = publication
            .as_ref()
            .map(|(key, publication_digest)| {
                serialize_payload(&StoredPublication {
                    namespace: batch.namespace.clone(),
                    key: key.clone(),
                    digest: publication_digest.clone(),
                    commit: commit.clone(),
                })
            })
            .transpose()?;
        let sequence_value = i64::try_from(sequence)
            .map_err(|_| GraphDbError::unavailable("graph commit sequence exceeds i64"))?;

        let mut session = database.session();
        session
            .begin_transaction()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let mutation_result = apply_prepared_mutations(
            &session,
            &mut state,
            &batch,
            prepared,
            &commit_payload,
            publication_payload.as_deref(),
            sequence_value,
        );
        if let Err(error) = mutation_result {
            if let Err(rollback_error) = session.rollback() {
                self.inner.poisoned.store(true, Ordering::Release);
                return Err(GraphDbError::DurabilityUncertain {
                    message: format!(
                        "pre-commit failure `{error}` followed by rollback failure: {rollback_error}"
                    ),
                });
            }
            return Err(error);
        }
        session
            .commit()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        if self.inner.durability == GraphDurability::Sync
            && let Err(error) = database.wal_checkpoint()
        {
            self.inner.poisoned.store(true, Ordering::Release);
            return Err(GraphDbError::DurabilityUncertain {
                message: error.to_string(),
            });
        }
        Ok(commit)
    }

    fn read_guard(&self) -> Result<RwLockReadGuard<'_, Option<GrafeoDB>>, GraphDbError> {
        self.ensure_available()?;
        self.inner
            .database
            .read()
            .map_err(|_| GraphDbError::unavailable("graph database read lock is poisoned"))
    }

    fn write_guard(&self) -> Result<RwLockWriteGuard<'_, Option<GrafeoDB>>, GraphDbError> {
        self.ensure_available()?;
        self.inner
            .database
            .write()
            .map_err(|_| GraphDbError::unavailable("graph database write lock is poisoned"))
    }

    fn ensure_available(&self) -> Result<(), GraphDbError> {
        if self.inner.poisoned.load(Ordering::Acquire) {
            return Err(durability_uncertain());
        }
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(GraphDbError::Closed);
        }
        Ok(())
    }
}

impl GraphSnapshot {
    pub fn traverse(&self, request: TraversalRequest) -> Result<TraversalResult, GraphDbError> {
        traversal::traverse(&self.database, request)
    }

    pub fn vector_search(
        &self,
        request: VectorSearchRequest,
    ) -> Result<VectorSearchResult, GraphDbError> {
        vector::vector_search(&self.database, request)
    }
}

fn validate_or_initialize_format(
    database: &GrafeoDB,
    validated: &ValidatedOpen,
) -> Result<(), GraphDbError> {
    let store = database.graph_store();
    let markers = store.nodes_by_label(FORMAT_LABEL);
    if markers.is_empty() {
        if store.node_count() != 0 || validated.preexisting_file {
            return Err(GraphDbError::ResetRequired {
                message: "existing Grafeo store has no TraceDecay format marker".to_owned(),
            });
        }
        let mut session = database.session();
        session
            .begin_transaction()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let version = i64::from(validated.expected_format.get());
        if let Err(error) = session.create_node_with_props(
            &[FORMAT_LABEL],
            [
                (FORMAT_VERSION_PROPERTY, Value::from(version)),
                (SEQUENCE_PROPERTY, Value::from(0_i64)),
            ],
        ) {
            let _ = session.rollback();
            return Err(GraphDbError::unavailable(error.to_string()));
        }
        session
            .commit()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        if validated.durability == GraphDurability::Sync
            && let Err(error) = database.wal_checkpoint()
        {
            return Err(GraphDbError::DurabilityUncertain {
                message: error.to_string(),
            });
        }
        return Ok(());
    }
    if markers.len() != 1 {
        return Err(GraphDbError::ResetRequired {
            message: "TraceDecay format marker count is not exactly one".to_owned(),
        });
    }
    let marker = store
        .get_node(markers[0])
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "TraceDecay format marker is unreadable".to_owned(),
        })?;
    let actual = marker
        .get_property(FORMAT_VERSION_PROPERTY)
        .and_then(Value::as_int64);
    if actual != Some(i64::from(validated.expected_format.get())) {
        return Err(GraphDbError::ResetRequired {
            message: format!(
                "TraceDecay graph format mismatch: expected {}, found {actual:?}",
                validated.expected_format.get()
            ),
        });
    }
    Ok(())
}

pub(crate) fn load_entities(database: &GrafeoDB) -> Result<EntityIndex, GraphDbError> {
    let store = database.graph_store();
    let mut entities = BTreeMap::new();
    for node_id in store.nodes_by_label(ENTITY_LABEL) {
        let node = store
            .get_node(node_id)
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "entity node is unreadable".to_owned(),
            })?;
        let stored: StoredEntity =
            parse_payload(node.get_property(PAYLOAD_PROPERTY), "entity node payload")?;
        let key = (
            stored.namespace.as_str().to_owned(),
            stored.entity.identity.as_str().to_owned(),
        );
        if entities.insert(key, (node_id, stored)).is_some() {
            return Err(GraphDbError::Corrupt {
                message: "duplicate entity identity".to_owned(),
            });
        }
    }
    Ok(entities)
}

pub(crate) fn parse_relation(
    edge: &grafeo_core::graph::lpg::Edge,
) -> Result<StoredRelation, GraphDbError> {
    parse_payload(edge.get_property(PAYLOAD_PROPERTY), "relation edge payload")
}

fn load_state(database: &GrafeoDB) -> Result<LoadedState, GraphDbError> {
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
    let sequence_i64 = marker_node
        .get_property(SEQUENCE_PROPERTY)
        .and_then(Value::as_int64)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "format marker has no valid commit sequence".to_owned(),
        })?;
    let sequence = u64::try_from(sequence_i64).map_err(|_| GraphDbError::Corrupt {
        message: "format marker has a negative commit sequence".to_owned(),
    })?;
    let entities = load_entities(database)?;
    let mut relations = BTreeMap::new();
    let mut seen_edges = HashSet::new();
    for (node_id, _) in entities.values() {
        for (_, edge_id) in store.edges_from(*node_id, grafeo_core::graph::Direction::Outgoing) {
            if !seen_edges.insert(edge_id) {
                continue;
            }
            let edge = store
                .get_edge(edge_id)
                .ok_or_else(|| GraphDbError::Corrupt {
                    message: "relation edge is unreadable".to_owned(),
                })?;
            if edge.edge_type.as_str() != RELATION_TYPE {
                continue;
            }
            let stored = parse_relation(&edge)?;
            let key = (
                stored.namespace.as_str().to_owned(),
                stored.relation.identity.as_str().to_owned(),
            );
            if relations.insert(key, (edge_id, stored)).is_some() {
                return Err(GraphDbError::Corrupt {
                    message: "duplicate relation identity".to_owned(),
                });
            }
        }
    }
    let commits = load_labeled_payloads(database, COMMIT_LABEL, "commit payload")?;
    let publications = load_labeled_payloads(database, PUBLICATION_LABEL, "publication payload")?;
    Ok(LoadedState {
        marker: markers[0],
        sequence,
        entities,
        relations,
        commits,
        publications,
    })
}

fn load_labeled_payloads<T: for<'de> Deserialize<'de>>(
    database: &GrafeoDB,
    label: &str,
    description: &str,
) -> Result<Vec<T>, GraphDbError> {
    let store = database.graph_store();
    store
        .nodes_by_label(label)
        .into_iter()
        .map(|node_id| {
            let node = store
                .get_node(node_id)
                .ok_or_else(|| GraphDbError::Corrupt {
                    message: format!("{description} node is unreadable"),
                })?;
            parse_payload(node.get_property(PAYLOAD_PROPERTY), description)
        })
        .collect()
}

fn parse_payload<T: for<'de> Deserialize<'de>>(
    value: Option<&Value>,
    description: &str,
) -> Result<T, GraphDbError> {
    let json = value
        .and_then(Value::as_str)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: format!("{description} is missing or not a string"),
        })?;
    serde_json::from_str(json).map_err(|error| GraphDbError::Corrupt {
        message: format!("invalid {description}: {error}"),
    })
}

fn serialize_payload<T: Serialize>(value: &T) -> Result<String, GraphDbError> {
    serde_json::to_string(value)
        .map_err(|error| GraphDbError::invalid(format!("payload serialization failed: {error}")))
}

fn latest_watermark(
    state: &LoadedState,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
) -> Option<GraphWatermark> {
    state
        .commits
        .iter()
        .filter(|stored| &stored.namespace == namespace && &stored.projection == projection)
        .max_by_key(|stored| stored.commit.sequence)
        .map(|stored| stored.commit.watermark.clone())
}

fn validate_references(state: &LoadedState, batch: &GraphWriteBatch) -> Result<(), GraphDbError> {
    let mut entities: BTreeMap<(String, String), GraphProjectionId> = state
        .entities
        .iter()
        .map(|(key, (_, stored))| (key.clone(), stored.projection.clone()))
        .collect();
    let mut relations: BTreeMap<
        (String, String),
        (GraphProjectionId, GraphEntityId, GraphEntityId),
    > = state
        .relations
        .iter()
        .map(|(key, (_, stored))| {
            (
                key.clone(),
                (
                    stored.projection.clone(),
                    stored.relation.from.clone(),
                    stored.relation.to.clone(),
                ),
            )
        })
        .collect();
    let namespace = batch.namespace.as_str().to_owned();
    let mut mutation_keys = BTreeSet::new();
    for mutation in &batch.mutations {
        let (kind, identity) = mutation.sort_key();
        if !mutation_keys.insert((kind, identity.to_owned())) {
            return Err(GraphDbError::invalid("batch repeats a graph mutation"));
        }
        match mutation {
            GraphMutation::DeleteRelation(identity) => {
                let key = (namespace.clone(), identity.as_str().to_owned());
                if let Some((owner, _, _)) = relations.get(&key)
                    && owner != &batch.projection
                {
                    return Err(GraphDbError::Conflict);
                }
                relations.remove(&key);
            }
            GraphMutation::DeleteEntity(identity) => {
                let key = (namespace.clone(), identity.as_str().to_owned());
                if let Some(owner) = entities.get(&key)
                    && owner != &batch.projection
                {
                    return Err(GraphDbError::Conflict);
                }
                entities.remove(&key);
            }
            GraphMutation::UpsertEntity(entity) => {
                let key = (namespace.clone(), entity.identity.as_str().to_owned());
                if let Some(owner) = entities.get(&key)
                    && owner != &batch.projection
                {
                    return Err(GraphDbError::Conflict);
                }
                entities.insert(key, batch.projection.clone());
            }
            GraphMutation::UpsertRelation(relation) => {
                let key = (namespace.clone(), relation.identity.as_str().to_owned());
                if let Some((owner, _, _)) = relations.get(&key)
                    && owner != &batch.projection
                {
                    return Err(GraphDbError::Conflict);
                }
                relations.insert(
                    key,
                    (
                        batch.projection.clone(),
                        relation.from.clone(),
                        relation.to.clone(),
                    ),
                );
            }
        }
    }
    for (_, from, to) in relations.values() {
        for endpoint in [from, to] {
            if !entities.contains_key(&(namespace.clone(), endpoint.as_str().to_owned())) {
                return Err(GraphDbError::invalid(format!(
                    "relation endpoint `{endpoint}` does not exist in namespace `{}`",
                    batch.namespace
                )));
            }
        }
    }
    Ok(())
}

fn prepare_mutations(batch: &GraphWriteBatch) -> Result<Vec<PreparedMutation>, GraphDbError> {
    batch
        .mutations
        .iter()
        .map(|mutation| match mutation {
            GraphMutation::DeleteRelation(identity) => {
                Ok(PreparedMutation::DeleteRelation(identity.clone()))
            }
            GraphMutation::DeleteEntity(identity) => {
                Ok(PreparedMutation::DeleteEntity(identity.clone()))
            }
            GraphMutation::UpsertEntity(entity) => {
                let stored = StoredEntity {
                    namespace: batch.namespace.clone(),
                    projection: batch.projection.clone(),
                    entity: entity.clone(),
                };
                Ok(PreparedMutation::UpsertEntity(
                    entity.clone(),
                    serialize_payload(&stored)?,
                ))
            }
            GraphMutation::UpsertRelation(relation) => {
                let stored = StoredRelation {
                    namespace: batch.namespace.clone(),
                    projection: batch.projection.clone(),
                    relation: relation.clone(),
                };
                Ok(PreparedMutation::UpsertRelation(
                    relation.clone(),
                    serialize_payload(&stored)?,
                ))
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn apply_prepared_mutations(
    session: &grafeo_engine::Session,
    state: &mut LoadedState,
    batch: &GraphWriteBatch,
    mutations: Vec<PreparedMutation>,
    commit_payload: &str,
    publication_payload: Option<&str>,
    sequence: i64,
) -> Result<(), GraphDbError> {
    let namespace = batch.namespace.as_str().to_owned();
    for mutation in mutations {
        if batch.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        match mutation {
            PreparedMutation::DeleteRelation(identity) => {
                if let Some((edge_id, _)) = state
                    .relations
                    .remove(&(namespace.clone(), identity.as_str().to_owned()))
                {
                    session.delete_edge(edge_id);
                }
            }
            PreparedMutation::DeleteEntity(identity) => {
                if let Some((node_id, _)) = state
                    .entities
                    .remove(&(namespace.clone(), identity.as_str().to_owned()))
                {
                    session.delete_node(node_id);
                }
            }
            PreparedMutation::UpsertEntity(entity, payload) => {
                upsert_entity(session, state, batch, entity, payload)?;
            }
            PreparedMutation::UpsertRelation(relation, payload) => {
                upsert_relation(session, state, batch, relation, payload)?;
            }
        }
    }
    session
        .set_node_property(state.marker, SEQUENCE_PROPERTY, sequence.into())
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    session
        .create_node_with_props(
            &[COMMIT_LABEL],
            [(PAYLOAD_PROPERTY, Value::from(commit_payload))],
        )
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    if let Some(payload) = publication_payload {
        session
            .create_node_with_props(
                &[PUBLICATION_LABEL],
                [(PAYLOAD_PROPERTY, Value::from(payload))],
            )
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    }
    Ok(())
}

fn upsert_entity(
    session: &grafeo_engine::Session,
    state: &mut LoadedState,
    batch: &GraphWriteBatch,
    entity: GraphEntity,
    payload: String,
) -> Result<(), GraphDbError> {
    let key = (
        batch.namespace.as_str().to_owned(),
        entity.identity.as_str().to_owned(),
    );
    let stored = StoredEntity {
        namespace: batch.namespace.clone(),
        projection: batch.projection.clone(),
        entity: entity.clone(),
    };
    if let Some((node_id, previous)) = state.entities.get(&key) {
        for (name, property) in &previous.entity.properties {
            if matches!(property, GraphProperty::Vector(_)) {
                session
                    .set_node_property(*node_id, &vector_property_key(name), Value::Null)
                    .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
            }
        }
        session
            .set_node_property(*node_id, PAYLOAD_PROPERTY, payload.into())
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        set_vector_properties(session, *node_id, &entity)?;
        state.entities.insert(key, (*node_id, stored));
    } else {
        let mut properties = vec![(PAYLOAD_PROPERTY.to_owned(), Value::from(payload))];
        properties.extend(entity.properties.iter().filter_map(|(name, property)| {
            if let GraphProperty::Vector(vector) = property {
                Some((
                    vector_property_key(name),
                    Value::Vector(vector.values.clone().into()),
                ))
            } else {
                None
            }
        }));
        let node_id = session
            .create_node_with_props(
                &[ENTITY_LABEL],
                properties
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.clone())),
            )
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        state.entities.insert(key, (node_id, stored));
    }
    Ok(())
}

fn set_vector_properties(
    session: &grafeo_engine::Session,
    node_id: NodeId,
    entity: &GraphEntity,
) -> Result<(), GraphDbError> {
    for (name, property) in &entity.properties {
        if let GraphProperty::Vector(vector) = property {
            session
                .set_node_property(
                    node_id,
                    &vector_property_key(name),
                    Value::Vector(vector.values.clone().into()),
                )
                .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        }
    }
    Ok(())
}

fn upsert_relation(
    session: &grafeo_engine::Session,
    state: &mut LoadedState,
    batch: &GraphWriteBatch,
    relation: GraphRelation,
    payload: String,
) -> Result<(), GraphDbError> {
    let key = (
        batch.namespace.as_str().to_owned(),
        relation.identity.as_str().to_owned(),
    );
    if let Some((edge_id, _)) = state.relations.remove(&key) {
        session.delete_edge(edge_id);
    }
    let from = state
        .entities
        .get(&(
            batch.namespace.as_str().to_owned(),
            relation.from.as_str().to_owned(),
        ))
        .map(|(node, _)| *node)
        .ok_or_else(|| GraphDbError::invalid("relation source disappeared"))?;
    let to = state
        .entities
        .get(&(
            batch.namespace.as_str().to_owned(),
            relation.to.as_str().to_owned(),
        ))
        .map(|(node, _)| *node)
        .ok_or_else(|| GraphDbError::invalid("relation target disappeared"))?;
    let edge_id = session
        .create_edge_with_props(
            from,
            to,
            RELATION_TYPE,
            [(PAYLOAD_PROPERTY, Value::from(payload))],
        )
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    state.relations.insert(
        key,
        (
            edge_id,
            StoredRelation {
                namespace: batch.namespace.clone(),
                projection: batch.projection.clone(),
                relation,
            },
        ),
    );
    Ok(())
}

pub(crate) fn vector_property_key(name: &GraphPropertyName) -> String {
    format!(
        "__tracedecay_graph_db_vector_{}",
        hex::encode(name.as_str().as_bytes())
    )
}

fn durability_uncertain() -> GraphDbError {
    GraphDbError::DurabilityUncertain {
        message: "the handle was poisoned after a post-commit durability failure".to_owned(),
    }
}
