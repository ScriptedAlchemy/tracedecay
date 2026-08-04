use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use grafeo_common::types::Value;
use grafeo_common::utils::error::ErrorCode;
use grafeo_engine::GrafeoDB;
use parking_lot::lock_api::ArcRwLockReadGuard;
use parking_lot::{RawRwLock, RwLock as ParkingRwLock};

use crate::location::ValidatedOpen;
use crate::schema::{
    FINAL_SCHEMA, FORMAT_LABEL, FORMAT_VERSION_PROPERTY, INDEXED_PROPERTIES, SCHEMA_PROPERTY,
    SEQUENCE_PROPERTY,
};
use crate::state::{
    StateCache, latest_projection, projection_entities, projection_relations, publication,
};
use crate::{
    GraphCancellation, GraphCommit, GraphDbError, GraphDbOpenOptions, GraphDurability,
    GraphEntityId, GraphIdempotencyKey, GraphMutation, GraphNamespace, GraphProjectionId,
    GraphProperty, GraphPublication, GraphRelation, GraphRelationId, GraphRelationKind,
    GraphVectorIndexRequest, GraphVectorIndexStatus, GraphWriteBatch, ProjectionReplacement,
    TraversalRequest, TraversalResult, VectorSearchRequest, VectorSearchResult, mutation,
    traversal, vector,
};

#[derive(Clone)]
pub struct GraphDb {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) database: RwLock<Option<GrafeoDB>>,
    state: RwLock<StateCache>,
    durability: GraphDurability,
    snapshot_gate: Arc<ParkingRwLock<()>>,
    closed: AtomicBool,
    pub(crate) poisoned: AtomicBool,
}

pub struct GraphSnapshot {
    pub(crate) database: GraphDb,
    _lease: ArcRwLockReadGuard<RawRwLock, ()>,
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
                snapshot_gate: Arc::new(ParkingRwLock::new(())),
                closed: AtomicBool::new(false),
                poisoned: AtomicBool::new(false),
            }),
        })
    }

    pub fn snapshot(&self) -> Result<GraphSnapshot, GraphDbError> {
        let lease = self.inner.snapshot_gate.read_arc();
        let guard = self.read_guard()?;
        guard.as_ref().ok_or(GraphDbError::Closed)?;
        drop(guard);
        Ok(GraphSnapshot {
            database: self.clone(),
            _lease: lease,
        })
    }

    pub fn apply(&self, mut batch: GraphWriteBatch) -> Result<GraphCommit, GraphDbError> {
        let digest = batch.validate_and_digest()?;
        let _snapshot_gate = self.inner.snapshot_gate.write();
        let guard = self.write_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        if batch.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let mut state = self.state_write_guard()?;
        self.apply_locked(database, &mut state, batch, digest, None)
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
        let _snapshot_gate = self.inner.snapshot_gate.write();
        let guard = self.write_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let retained: BTreeSet<_> = replacement
            .entities
            .iter()
            .map(|entity| entity.identity.clone())
            .collect();
        let mut mutations = Vec::new();
        for relation in
            projection_relations(database, &replacement.namespace, &replacement.projection)?
        {
            mutations.push(GraphMutation::DeleteRelation(relation.relation.identity));
        }
        for entity in
            projection_entities(database, &replacement.namespace, &replacement.projection)?
        {
            if !retained.contains(&entity.entity.identity) {
                mutations.push(GraphMutation::DeleteEntity(entity.entity.identity));
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
        let mut state = self.state_write_guard()?;
        self.apply_locked(database, &mut state, batch, digest, None)
    }

    pub fn publish(
        &self,
        mut publication_request: GraphPublication,
    ) -> Result<GraphCommit, GraphDbError> {
        let publication_digest = publication_request.validate_and_digest()?;
        let batch_digest = publication_request.batch.validate_and_digest()?;
        let _snapshot_gate = self.inner.snapshot_gate.write();
        let guard = self.write_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        if publication_request.cancellation.is_cancelled()
            || publication_request.batch.cancellation.is_cancelled()
        {
            return Err(GraphDbError::Cancelled);
        }
        if let Some(existing) = publication(
            database,
            &publication_request.namespace,
            &publication_request.idempotency_key,
        )? {
            return if existing.digest == publication_digest {
                Ok(existing.commit)
            } else {
                Err(GraphDbError::Conflict)
            };
        }
        let current = latest_projection(
            database,
            &publication_request.namespace,
            &publication_request.batch.projection,
        )?
        .map(|state| state.commit.watermark);
        if current.as_ref() != publication_request.expected_watermark.as_ref() {
            return Err(GraphDbError::Conflict);
        }
        let publication_record = (publication_request.idempotency_key, publication_digest);
        let mut state = self.state_write_guard()?;
        self.apply_locked(
            database,
            &mut state,
            publication_request.batch,
            batch_digest,
            Some(publication_record),
        )
    }

    pub fn traverse(&self, request: TraversalRequest) -> Result<TraversalResult, GraphDbError> {
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        traversal::traverse(database, request)
    }

    pub fn outgoing_relation_ids(
        &self,
        namespace: &GraphNamespace,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<GraphRelationKind>,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelationId>>, GraphDbError> {
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        traversal::outgoing_relation_ids(
            database,
            namespace,
            starts,
            relation_kinds,
            max_relations,
            cancellation.as_ref(),
        )
    }

    pub fn outgoing_relations(
        &self,
        namespace: &GraphNamespace,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<GraphRelationKind>,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelation>>, GraphDbError> {
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        traversal::outgoing_relations(
            database,
            namespace,
            starts,
            relation_kinds,
            max_relations,
            cancellation.as_ref(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reachable_entities(
        &self,
        namespace: &GraphNamespace,
        projection: &GraphProjectionId,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<GraphRelationKind>,
        outgoing_overrides: &BTreeMap<GraphEntityId, BTreeSet<GraphEntityId>>,
        max_visits: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<BTreeSet<GraphEntityId>>, GraphDbError> {
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        traversal::reachable_entities(
            database,
            namespace,
            projection,
            starts,
            relation_kinds,
            outgoing_overrides,
            max_visits,
            cancellation.as_ref(),
        )
    }

    pub fn vector_search(
        &self,
        request: VectorSearchRequest,
    ) -> Result<VectorSearchResult, GraphDbError> {
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        vector::vector_search(database, request)
    }

    pub fn vector_index_status(
        &self,
        request: GraphVectorIndexRequest,
    ) -> Result<GraphVectorIndexStatus, GraphDbError> {
        request.validate()?;
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let label = vector::native_vector_label(&request.namespace, &request.projection);
        let property = vector_property_key(&request.property, request.dimension, request.metric);
        Ok(
            if database.graph_store().has_vector_index(&label, &property) {
                GraphVectorIndexStatus::Available
            } else {
                GraphVectorIndexStatus::Missing
            },
        )
    }

    /// Rebuilds one missing exact-projection HNSW index outside admission.
    pub fn ensure_vector_index(
        &self,
        request: GraphVectorIndexRequest,
    ) -> Result<GraphVectorIndexStatus, GraphDbError> {
        request.validate()?;
        let _snapshot_gate = self.inner.snapshot_gate.write();
        let guard = self.write_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        if request.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let label = vector::native_vector_label(&request.namespace, &request.projection);
        let property = vector_property_key(&request.property, request.dimension, request.metric);
        if !database.graph_store().has_vector_index(&label, &property) {
            database
                .create_vector_index(
                    &label,
                    &property,
                    Some(request.dimension),
                    Some(request.metric.engine_name()),
                    None,
                    None,
                    None,
                )
                .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        }
        if request.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        Ok(GraphVectorIndexStatus::Available)
    }

    pub fn close(&self) -> Result<(), GraphDbError> {
        let _snapshot_gate = self.inner.snapshot_gate.write();
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
        publication_record: Option<(GraphIdempotencyKey, String)>,
    ) -> Result<GraphCommit, GraphDbError> {
        ensure_native_vector_indexes(database, &batch)?;
        let vector_updates = batch
            .mutations
            .iter()
            .filter_map(|mutation| match mutation {
                GraphMutation::UpsertEntity(entity) => Some(entity),
                _ => None,
            })
            .flat_map(|entity| {
                entity
                    .properties
                    .iter()
                    .filter_map(move |(name, property)| {
                        let GraphProperty::Vector(vector) = property else {
                            return None;
                        };
                        Some((
                            entity.identity.clone(),
                            vector_property_key(name, vector.dimension, vector.metric),
                            Value::Vector(vector.values.clone().into()),
                        ))
                    })
            })
            .collect::<Vec<_>>();
        let namespace = batch.namespace.clone();
        let commit = mutation::apply(
            database,
            state,
            batch,
            digest,
            publication_record,
            &self.inner.poisoned,
        )?;
        for (identity, property, value) in vector_updates {
            let stored = crate::state::load_entity(database, &namespace, &identity)?.ok_or_else(
                || GraphDbError::Corrupt {
                    message: format!(
                        "committed vector entity `{identity}` is missing from native identity index"
                    ),
                },
            )?;
            database.set_node_property(stored.node, &property, value);
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

    pub(crate) fn read_database(
        &self,
        cancellation: &dyn GraphCancellation,
    ) -> Result<RwLockReadGuard<'_, Option<GrafeoDB>>, GraphDbError> {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let guard = self.read_guard()?;
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        Ok(guard)
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

    fn state_write_guard(&self) -> Result<RwLockWriteGuard<'_, StateCache>, GraphDbError> {
        self.inner
            .state
            .write()
            .map_err(|_| GraphDbError::unavailable("graph state lock is poisoned"))
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

fn ensure_native_vector_indexes(
    database: &GrafeoDB,
    batch: &GraphWriteBatch,
) -> Result<(), GraphDbError> {
    let store = database.graph_store();
    let label = vector::native_vector_label(&batch.namespace, &batch.projection);
    for entity in batch
        .mutations
        .iter()
        .filter_map(|mutation| match mutation {
            GraphMutation::UpsertEntity(entity) => Some(entity),
            _ => None,
        })
    {
        for (name, property) in &entity.properties {
            let GraphProperty::Vector(vector) = property else {
                continue;
            };
            let property = vector_property_key(name, vector.dimension, vector.metric);
            if !store.has_vector_index(&label, &property) {
                database
                    .create_vector_index(
                        &label,
                        &property,
                        Some(vector.dimension),
                        Some(vector.metric.engine_name()),
                        None,
                        None,
                        None,
                    )
                    .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
            }
        }
    }
    Ok(())
}

impl GraphSnapshot {
    pub fn traverse(&self, request: TraversalRequest) -> Result<TraversalResult, GraphDbError> {
        self.database.traverse(request)
    }

    pub fn vector_search(
        &self,
        request: VectorSearchRequest,
    ) -> Result<VectorSearchResult, GraphDbError> {
        self.database.vector_search(request)
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
                (SCHEMA_PROPERTY, Value::from(FINAL_SCHEMA)),
                (SEQUENCE_PROPERTY, Value::from(0_i64)),
            ],
        ) {
            let _ = session.rollback();
            return Err(GraphDbError::unavailable(error.to_string()));
        }
        session
            .commit()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        for property in INDEXED_PROPERTIES {
            database.create_property_index(property);
        }
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
    if marker.get_property(SCHEMA_PROPERTY).and_then(Value::as_str) != Some(FINAL_SCHEMA) {
        return Err(GraphDbError::ResetRequired {
            message: "TraceDecay graph schema is not the final native scalar schema".to_owned(),
        });
    }
    Ok(())
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

pub(crate) use crate::schema::vector_property_key;

#[cfg(test)]
mod tests;
