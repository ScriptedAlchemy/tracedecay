use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use grafeo_common::types::{EdgeId, NodeId, Value};
use grafeo_common::utils::error::ErrorCode;
use grafeo_engine::GrafeoDB;

use crate::location::ValidatedOpen;
use crate::state::{
    COMMIT_LABEL, ENTITY_LABEL, FORMAT_LABEL, FORMAT_VERSION_PROPERTY, PAYLOAD_PROPERTY,
    PUBLICATION_LABEL, RELATION_TYPE, SEQUENCE_PROPERTY, StableKey, StateCache, StoredCommit,
    StoredEntity, StoredPublication, StoredRelation, serialize_payload, stable_key,
};
use crate::traversal;
use crate::vector;
use crate::{
    GraphCommit, GraphDbError, GraphDbOpenOptions, GraphDurability, GraphEntity, GraphEntityId,
    GraphIdempotencyKey, GraphMutation, GraphProjectionId, GraphProperty, GraphPropertyName,
    GraphPublication, GraphRelation, GraphRelationId, GraphWriteBatch, ProjectionReplacement,
    TraversalRequest, TraversalResult, VectorSearchRequest, VectorSearchResult,
};

#[derive(Clone)]
pub struct GraphDb {
    inner: Arc<Inner>,
}

struct Inner {
    database: RwLock<Option<GrafeoDB>>,
    // Derived from Grafeo at open and replaced only after a successful commit.
    // The database write lock prevents readers from observing a cache/database skew.
    state: RwLock<StateCache>,
    durability: GraphDurability,
    closed: AtomicBool,
    poisoned: AtomicBool,
}

pub struct GraphSnapshot {
    database: Arc<GrafeoDB>,
    state: Arc<StateCache>,
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

enum PreparedMutation {
    DeleteRelation(GraphRelationId),
    DeleteEntity(GraphEntityId),
    UpsertEntity(GraphEntity, String),
    UpsertRelation(GraphRelation, String),
}

type EntityDelta = BTreeMap<StableKey, Option<(NodeId, StoredEntity)>>;
type RelationDelta = BTreeMap<StableKey, Option<(EdgeId, StoredRelation)>>;

struct StateOverlay<'a> {
    base: &'a StateCache,
    entities: EntityDelta,
    relations: RelationDelta,
}

impl<'a> StateOverlay<'a> {
    fn new(base: &'a StateCache) -> Self {
        Self {
            base,
            entities: BTreeMap::new(),
            relations: BTreeMap::new(),
        }
    }

    fn entity(&self, key: &StableKey) -> Option<&(NodeId, StoredEntity)> {
        match self.entities.get(key) {
            Some(value) => value.as_ref(),
            None => self.base.entities.get(key),
        }
    }

    fn relation(&self, key: &StableKey) -> Option<&(EdgeId, StoredRelation)> {
        match self.relations.get(key) {
            Some(value) => value.as_ref(),
            None => self.base.relations.get(key),
        }
    }

    fn remove_entity(&mut self, key: StableKey) -> Option<(NodeId, StoredEntity)> {
        let existing = self.entity(&key).cloned();
        self.entities.insert(key, None);
        existing
    }

    fn remove_relation(&mut self, key: StableKey) -> Option<(EdgeId, StoredRelation)> {
        let existing = self.relation(&key).cloned();
        self.relations.insert(key, None);
        existing
    }

    fn upsert_entity(&mut self, key: StableKey, node: NodeId, stored: StoredEntity) {
        self.entities.insert(key, Some((node, stored)));
    }

    fn upsert_relation(&mut self, key: StableKey, edge: EdgeId, stored: StoredRelation) {
        self.relations.insert(key, Some((edge, stored)));
    }

    fn into_delta(self) -> (EntityDelta, RelationDelta) {
        (self.entities, self.relations)
    }
}

fn apply_state_delta(state: &mut StateCache, entities: EntityDelta, relations: RelationDelta) {
    for (key, value) in relations {
        match value {
            Some((edge, stored)) => state.insert_relation(key, edge, stored),
            None => {
                state.remove_relation(&key);
            }
        }
    }
    for (key, value) in entities {
        match value {
            Some((node, stored)) => state.insert_entity(key, node, stored),
            None => {
                state.remove_entity(&key);
            }
        }
    }
}

impl GraphDb {
    pub fn open(options: GraphDbOpenOptions) -> Result<Self, GraphDbError> {
        let cancellation = Arc::clone(&options.cancellation);
        let validated = options.validate()?;
        let database = GrafeoDB::with_config(validated.config.clone())
            .map_err(|error| map_open_error(error, validated.preexisting_file))?;
        validate_or_initialize_format(&database, &validated)?;
        let state = StateCache::load(&database)?;
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                database: RwLock::new(Some(database)),
                state: RwLock::new(state),
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
        let state = StateCache::load(&snapshot)?;
        Ok(GraphSnapshot {
            database: Arc::new(snapshot),
            state: Arc::new(state),
        })
    }

    pub fn apply(&self, mut batch: GraphWriteBatch) -> Result<GraphCommit, GraphDbError> {
        let digest = batch.validate_and_digest()?;
        let guard = self.write_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        if batch.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let mut cached = self.state_write_guard()?;
        self.apply_locked(database, &mut cached, batch, digest, None)
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
        if replacement.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let mut cached = self.state_write_guard()?;
        let retained_entities: BTreeSet<_> = replacement
            .entities
            .iter()
            .map(|entity| entity.identity.clone())
            .collect();
        let mut mutations = Vec::new();
        for (_, stored) in cached.relations.values() {
            if stored.namespace == replacement.namespace
                && stored.projection == replacement.projection
            {
                mutations.push(GraphMutation::DeleteRelation(
                    stored.relation.identity.clone(),
                ));
            }
        }
        for (_, stored) in cached.entities.values() {
            if stored.namespace == replacement.namespace
                && stored.projection == replacement.projection
                && !retained_entities.contains(&stored.entity.identity)
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
        self.apply_locked(database, &mut cached, batch, digest, None)
    }

    pub fn publish(&self, mut publication: GraphPublication) -> Result<GraphCommit, GraphDbError> {
        let publication_digest = publication.validate_and_digest()?;
        let batch_digest = publication.batch.validate_and_digest()?;
        let guard = self.write_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        if publication.cancellation.is_cancelled() || publication.batch.cancellation.is_cancelled()
        {
            return Err(GraphDbError::Cancelled);
        }
        let mut cached = self.state_write_guard()?;
        if let Some(existing) =
            cached.publication(&publication.namespace, &publication.idempotency_key)
        {
            return if existing.digest == publication_digest {
                Ok(existing.commit.clone())
            } else {
                Err(GraphDbError::Conflict)
            };
        }
        let current =
            cached.latest_watermark(&publication.namespace, &publication.batch.projection);
        if current != publication.expected_watermark.as_ref() {
            return Err(GraphDbError::Conflict);
        }
        let publication_record = (publication.idempotency_key, publication_digest);
        self.apply_locked(
            database,
            &mut cached,
            publication.batch,
            batch_digest,
            Some(publication_record),
        )
    }

    pub fn traverse(&self, request: TraversalRequest) -> Result<TraversalResult, GraphDbError> {
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let cached = self.state_read_guard()?;
        traversal::traverse(database, &cached, request)
    }

    pub fn vector_search(
        &self,
        request: VectorSearchRequest,
    ) -> Result<VectorSearchResult, GraphDbError> {
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let cached = self.state_read_guard()?;
        vector::vector_search(database, &cached, request)
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
        state: &mut StateCache,
        batch: GraphWriteBatch,
        digest: String,
        publication: Option<(GraphIdempotencyKey, String)>,
    ) -> Result<GraphCommit, GraphDbError> {
        validate_references(state, &batch)?;
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
        let stored_publication =
            publication
                .as_ref()
                .map(|(key, publication_digest)| StoredPublication {
                    namespace: batch.namespace.clone(),
                    key: key.clone(),
                    digest: publication_digest.clone(),
                    commit: commit.clone(),
                });
        let publication_payload = stored_publication
            .as_ref()
            .map(serialize_payload)
            .transpose()?;
        let sequence_value = i64::try_from(sequence)
            .map_err(|_| GraphDbError::unavailable("graph commit sequence exceeds i64"))?;

        let mut session = database.session();
        session
            .begin_transaction()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let marker = state.marker;
        let mut overlay = StateOverlay::new(state);
        let mutation_result = apply_prepared_mutations(
            &session,
            &mut overlay,
            &batch,
            prepared,
            &commit_payload,
            publication_payload.as_deref(),
            sequence_value,
            marker,
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
        if batch.cancellation.is_cancelled() {
            if let Err(rollback_error) = session.rollback() {
                self.inner.poisoned.store(true, Ordering::Release);
                return Err(GraphDbError::DurabilityUncertain {
                    message: format!(
                        "pre-commit cancellation followed by rollback failure: {rollback_error}"
                    ),
                });
            }
            return Err(GraphDbError::Cancelled);
        }
        session.commit().map_err(map_commit_error)?;
        let (entities, relations) = overlay.into_delta();
        apply_state_delta(state, entities, relations);
        state.record_commit(stored_commit);
        if let Some(stored) = stored_publication {
            state.record_publication(stored);
        }
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
        let guard = self
            .inner
            .database
            .read()
            .map_err(|_| GraphDbError::unavailable("graph database read lock is poisoned"))?;
        self.ensure_available()?;
        Ok(guard)
    }

    fn write_guard(&self) -> Result<RwLockWriteGuard<'_, Option<GrafeoDB>>, GraphDbError> {
        self.ensure_available()?;
        let guard = self
            .inner
            .database
            .write()
            .map_err(|_| GraphDbError::unavailable("graph database write lock is poisoned"))?;
        self.ensure_available()?;
        Ok(guard)
    }

    fn state_read_guard(&self) -> Result<RwLockReadGuard<'_, StateCache>, GraphDbError> {
        self.inner
            .state
            .read()
            .map_err(|_| GraphDbError::unavailable("graph state cache read lock is poisoned"))
    }

    fn state_write_guard(&self) -> Result<RwLockWriteGuard<'_, StateCache>, GraphDbError> {
        self.inner
            .state
            .write()
            .map_err(|_| GraphDbError::unavailable("graph state cache write lock is poisoned"))
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
        traversal::traverse(&self.database, &self.state, request)
    }

    pub fn vector_search(
        &self,
        request: VectorSearchRequest,
    ) -> Result<VectorSearchResult, GraphDbError> {
        vector::vector_search(&self.database, &self.state, request)
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

fn validate_references(state: &StateCache, batch: &GraphWriteBatch) -> Result<(), GraphDbError> {
    let mut entities: BTreeMap<StableKey, Option<GraphProjectionId>> = BTreeMap::new();
    let mut relations: BTreeMap<
        StableKey,
        Option<(GraphProjectionId, GraphEntityId, GraphEntityId)>,
    > = BTreeMap::new();
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
                if let Some((owner, _, _)) = logical_relation(state, &relations, &key)
                    && owner != batch.projection
                {
                    return Err(GraphDbError::Conflict);
                }
                relations.insert(key, None);
            }
            GraphMutation::DeleteEntity(identity) => {
                let key = (namespace.clone(), identity.as_str().to_owned());
                if let Some(owner) = logical_entity_owner(state, &entities, &key)
                    && owner != batch.projection
                {
                    return Err(GraphDbError::Conflict);
                }
                entities.insert(key, None);
            }
            GraphMutation::UpsertEntity(entity) => {
                let key = (namespace.clone(), entity.identity.as_str().to_owned());
                if let Some(owner) = logical_entity_owner(state, &entities, &key)
                    && owner != batch.projection
                {
                    return Err(GraphDbError::Conflict);
                }
                entities.insert(key, Some(batch.projection.clone()));
            }
            GraphMutation::UpsertRelation(relation) => {
                let key = (namespace.clone(), relation.identity.as_str().to_owned());
                if let Some((owner, _, _)) = logical_relation(state, &relations, &key)
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
            require_entity(state, &entities, &batch.namespace, endpoint)?;
        }
    }
    for (entity_key, owner) in &entities {
        if owner.is_some() {
            continue;
        }
        for relation_key in state.relations_for_entity(entity_key) {
            if let Some((_, from, to)) = logical_relation(state, &relations, relation_key)
                && [from, to].iter().any(|endpoint| {
                    endpoint.as_str() == entity_key.1 && batch.namespace.as_str() == entity_key.0
                })
            {
                return Err(GraphDbError::invalid(format!(
                    "entity `{}` remains referenced by relation `{}`",
                    entity_key.1, relation_key.1
                )));
            }
        }
    }
    Ok(())
}

fn logical_entity_owner(
    state: &StateCache,
    changes: &BTreeMap<StableKey, Option<GraphProjectionId>>,
    key: &StableKey,
) -> Option<GraphProjectionId> {
    changes.get(key).cloned().flatten().or_else(|| {
        (!changes.contains_key(key))
            .then(|| {
                state
                    .entities
                    .get(key)
                    .map(|(_, stored)| stored.projection.clone())
            })
            .flatten()
    })
}

fn logical_relation(
    state: &StateCache,
    changes: &BTreeMap<StableKey, Option<(GraphProjectionId, GraphEntityId, GraphEntityId)>>,
    key: &StableKey,
) -> Option<(GraphProjectionId, GraphEntityId, GraphEntityId)> {
    changes.get(key).cloned().flatten().or_else(|| {
        (!changes.contains_key(key))
            .then(|| {
                state.relations.get(key).map(|(_, stored)| {
                    (
                        stored.projection.clone(),
                        stored.relation.from.clone(),
                        stored.relation.to.clone(),
                    )
                })
            })
            .flatten()
    })
}

fn require_entity(
    state: &StateCache,
    changes: &BTreeMap<StableKey, Option<GraphProjectionId>>,
    namespace: &crate::GraphNamespace,
    endpoint: &GraphEntityId,
) -> Result<(), GraphDbError> {
    if logical_entity_owner(state, changes, &stable_key(namespace, endpoint.as_str())).is_none() {
        return Err(GraphDbError::invalid(format!(
            "relation endpoint `{endpoint}` does not exist in namespace `{namespace}`"
        )));
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
    state: &mut StateOverlay<'_>,
    batch: &GraphWriteBatch,
    mutations: Vec<PreparedMutation>,
    commit_payload: &str,
    publication_payload: Option<&str>,
    sequence: i64,
    marker: NodeId,
) -> Result<(), GraphDbError> {
    let namespace = batch.namespace.as_str().to_owned();
    for mutation in mutations {
        if batch.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        match mutation {
            PreparedMutation::DeleteRelation(identity) => {
                if let Some((edge_id, _)) =
                    state.remove_relation((namespace.clone(), identity.as_str().to_owned()))
                {
                    session.delete_edge(edge_id);
                }
            }
            PreparedMutation::DeleteEntity(identity) => {
                if let Some((node_id, _)) =
                    state.remove_entity((namespace.clone(), identity.as_str().to_owned()))
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
        .set_node_property(marker, SEQUENCE_PROPERTY, sequence.into())
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
    state: &mut StateOverlay<'_>,
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
    if let Some((node_id, previous)) = state.entity(&key) {
        for (name, property) in &previous.entity.properties {
            if let GraphProperty::Vector(vector) = property {
                session
                    .set_node_property(
                        *node_id,
                        &vector_property_key(name, vector.dimension, vector.metric),
                        Value::Null,
                    )
                    .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
            }
        }
        session
            .set_node_property(*node_id, PAYLOAD_PROPERTY, payload.into())
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        set_vector_properties(session, *node_id, &entity)?;
        state.upsert_entity(key, *node_id, stored);
    } else {
        let mut properties = vec![(PAYLOAD_PROPERTY.to_owned(), Value::from(payload))];
        properties.extend(entity.properties.iter().filter_map(|(name, property)| {
            if let GraphProperty::Vector(vector) = property {
                Some((
                    vector_property_key(name, vector.dimension, vector.metric),
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
        state.upsert_entity(key, node_id, stored);
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
                    &vector_property_key(name, vector.dimension, vector.metric),
                    Value::Vector(vector.values.clone().into()),
                )
                .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        }
    }
    Ok(())
}

fn upsert_relation(
    session: &grafeo_engine::Session,
    state: &mut StateOverlay<'_>,
    batch: &GraphWriteBatch,
    relation: GraphRelation,
    payload: String,
) -> Result<(), GraphDbError> {
    let key = (
        batch.namespace.as_str().to_owned(),
        relation.identity.as_str().to_owned(),
    );
    if let Some((edge_id, _)) = state.remove_relation(key.clone()) {
        session.delete_edge(edge_id);
    }
    let from = state
        .entity(&(
            batch.namespace.as_str().to_owned(),
            relation.from.as_str().to_owned(),
        ))
        .map(|(node, _)| *node)
        .ok_or_else(|| GraphDbError::invalid("relation source disappeared"))?;
    let to = state
        .entity(&(
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
    state.upsert_relation(
        key,
        edge_id,
        StoredRelation {
            namespace: batch.namespace.clone(),
            projection: batch.projection.clone(),
            relation,
        },
    );
    Ok(())
}

pub(crate) fn vector_property_key(
    name: &GraphPropertyName,
    dimension: usize,
    metric: crate::VectorMetric,
) -> String {
    format!(
        "__tracedecay_graph_db_vector_{}_{}_{}",
        hex::encode(name.as_str().as_bytes()),
        dimension,
        metric.storage_tag()
    )
}

fn durability_uncertain() -> GraphDbError {
    GraphDbError::DurabilityUncertain {
        message: "the handle was poisoned after a post-commit durability failure".to_owned(),
    }
}

fn map_open_error(
    error: grafeo_common::utils::error::Error,
    preexisting_file: bool,
) -> GraphDbError {
    let malformed_io = matches!(
        &error,
        grafeo_common::utils::error::Error::Io(io)
            if preexisting_file
                && matches!(
                    io.kind(),
                    std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof
                )
    );
    let message = error.to_string();
    if malformed_io {
        return GraphDbError::Corrupt { message };
    }
    match error.error_code() {
        ErrorCode::StorageCorrupted
        | ErrorCode::StorageRecoveryFailed
        | ErrorCode::SerializationError
            if preexisting_file =>
        {
            GraphDbError::Corrupt { message }
        }
        _ => GraphDbError::unavailable(message),
    }
}

fn map_commit_error(error: grafeo_common::utils::error::Error) -> GraphDbError {
    // Grafeo 0.5.42 returns commit errors before version finalization and rolls
    // conflicts back. Its post-finalization WAL failures are warning-only, so
    // the mandatory Sync checkpoint above is the observable uncertainty gate.
    match error.error_code() {
        ErrorCode::TransactionConflict
        | ErrorCode::TransactionSerialization
        | ErrorCode::TransactionDeadlock => GraphDbError::Conflict,
        _ => GraphDbError::unavailable(error.to_string()),
    }
}

#[cfg(test)]
mod tests;
