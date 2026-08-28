use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use grafeo_common::types::Value;
use grafeo_engine::GrafeoDB;
use parking_lot::lock_api::ArcRwLockReadGuard;
use parking_lot::{
    RawRwLock, RwLock as ParkingRwLock, RwLockUpgradableReadGuard,
    RwLockWriteGuard as ParkingRwLockWriteGuard,
};

use crate::lease::VerifiedGenerationState;
use crate::location::{PersistentGraphStoreState, ValidatedOpen};
use crate::recovery::{
    load_quarantined_projections, map_open_error, projection_mismatch,
    validate_or_initialize_format,
};
use crate::state::{
    FormatState, latest_projection, load_entity_locator, outgoing_relation_projections,
    projection_entities, projection_relations, publication,
};
use crate::{
    GraphCancellation, GraphCommit, GraphDbError, GraphDbOpenOptions, GraphDurability,
    GraphEntityId, GraphIdempotencyKey, GraphMutation, GraphNamespace, GraphProjectionId,
    GraphProperty, GraphPublication, GraphPublicationDigest, GraphPublicationInputDigest,
    GraphPublicationReceipt, GraphRelation, GraphRelationId, GraphRelationKind,
    GraphVectorIndexRequest, GraphVectorIndexStatus, GraphWatermark, GraphWriteBatch,
    ProjectionReplacement, TraversalRequest, TraversalResult, VectorSearchRequest,
    VectorSearchResult, mutation, traversal, vector,
};

enum ReplacementPrecondition<'a> {
    Unchecked,
    Expected(Option<&'a GraphWatermark>),
}

/// A batch derived behind the upgradable snapshot-gate claim, ready to apply
/// under the exclusive claim.
pub(crate) struct PreparedGraphBatch {
    pub(crate) batch: GraphWriteBatch,
    pub(crate) metadata: mutation::CommitMetadata,
    pub(crate) endpoint_namespaces: mutation::RelationEndpointNamespaces,
    pub(crate) ensure_page_vector_indexes: bool,
}

/// Outcome of deriving a gated batch: apply it, or adopt an already-stored
/// commit without taking the exclusive claim.
pub(crate) enum GraphBatchPlan<P> {
    Apply(PreparedGraphBatch, P),
    Settled(GraphCommit, P),
}

pub struct GraphDb {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) database: RwLock<Option<GrafeoDB>>,
    state: RwLock<FormatState>,
    pub(crate) durability: GraphDurability,
    pub(crate) reopen: Option<ValidatedOpen>,
    pub(crate) snapshot_gate: Arc<ParkingRwLock<()>>,
    pub(crate) verified_generations: RwLock<VerifiedGenerationState>,
    pub(crate) quarantined_projections: RwLock<BTreeSet<(GraphNamespace, GraphProjectionId)>>,
    /// Ordered identity access path for projection pagination. Rebuilt lazily
    /// after every database write claim; see
    /// [`crate::projection_identity_index`].
    pub(crate) identity_indexes: crate::projection_identity_index::IdentityIndexCache,
    pub(crate) closed: AtomicBool,
    pub(crate) poisoned: AtomicBool,
}

pub struct GraphSnapshot {
    pub(crate) database: Arc<GraphDb>,
    _lease: ArcRwLockReadGuard<RawRwLock, ()>,
    _client: Option<crate::GraphDbLeaseV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphDbRuntimeState {
    Ready,
    Closed,
    DurabilityUncertain,
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

impl GraphSnapshot {
    pub(crate) fn retain_client(&mut self, client: crate::GraphDbLeaseV1) {
        self._client = Some(client);
    }
}

impl GraphDb {
    #[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
    pub(crate) fn open(options: GraphDbOpenOptions) -> Result<Arc<Self>, GraphDbError> {
        Self::open_with_store_state(options, None)
    }

    #[hotpath::measure(label = "graph_db.generation.open", impl_type = "GraphDb")]
    pub(crate) fn open_with_store_state(
        options: GraphDbOpenOptions,
        persistent_store_state: Option<PersistentGraphStoreState>,
    ) -> Result<Arc<Self>, GraphDbError> {
        let validated = options.validate(persistent_store_state)?;
        // The engine call is where a persistent open pays for corpus size:
        // grafeo replays the whole serialized LPG block log through the live
        // mutation path, rebuilds every catalog-listed property index with a
        // full node scan each, and replays any sidecar WAL an unclean
        // shutdown left behind. The phases after it are O(labels), not
        // O(rows), so this span is what a slow open decomposes into first.
        let database = hotpath::measure_block!(
            "graph_db.generation.open.engine",
            GrafeoDB::with_config(validated.config.clone())
                .map_err(|error| map_open_error(error, validated.preexisting_store))
        )?;
        crate::recovery::record_open_corpus_gauges(&database);
        validate_or_initialize_format(&database, &validated)?;
        let state = hotpath::measure_block!(
            "graph_db.generation.open.state",
            FormatState::load(&database)
        )?;
        let quarantined_projections = load_quarantined_projections(&database)?;
        let graph = Arc::new(Self {
            inner: Arc::new(Inner {
                database: RwLock::new(Some(database)),
                state: RwLock::new(state),
                durability: validated.durability,
                reopen: validated.config.path.as_ref().map(|_| {
                    let mut reopen = validated.clone();
                    reopen.preexisting_store = true;
                    reopen
                }),
                snapshot_gate: Arc::new(ParkingRwLock::new(())),
                verified_generations: RwLock::new(VerifiedGenerationState::default()),
                quarantined_projections: RwLock::new(quarantined_projections),
                identity_indexes: crate::projection_identity_index::IdentityIndexCache::default(),
                closed: AtomicBool::new(false),
                poisoned: AtomicBool::new(false),
            }),
        });
        graph.record_memory_checkpoint(crate::hotpath_observe::GrafeoMemoryPhase::Open);
        Ok(graph)
    }

    #[must_use]
    pub fn runtime_state(&self) -> GraphDbRuntimeState {
        if self.inner.poisoned.load(Ordering::Acquire) {
            GraphDbRuntimeState::DurabilityUncertain
        } else if self.inner.closed.load(Ordering::Acquire) {
            GraphDbRuntimeState::Closed
        } else {
            GraphDbRuntimeState::Ready
        }
    }

    #[hotpath::measure(label = "graph_db.snapshot.acquire", impl_type = "GraphDb")]
    pub fn snapshot(self: &Arc<Self>) -> Result<GraphSnapshot, GraphDbError> {
        let lease = crate::hotpath_observe::wait_lock(
            crate::hotpath_observe::LOCK_WAIT_SNAPSHOT_GATE_READ,
            || self.inner.snapshot_gate.read_arc(),
        );
        let guard = self.read_guard()?;
        guard.as_ref().ok_or(GraphDbError::Closed)?;
        drop(guard);
        crate::hotpath_observe::record_hydration_source(
            crate::hotpath_observe::HydrationSource::Snapshot,
        );
        Ok(GraphSnapshot {
            database: Arc::clone(self),
            _lease: lease,
            _client: None,
        })
    }

    /// Applies a mutation batch to the disposable derived graph index.
    ///
    /// The result identifies this handle's native state only; callers keep
    /// canonical projection inputs independently and can rebuild the index.
    #[hotpath::measure(label = "graph_db.write.apply", impl_type = "GraphDb")]
    pub fn apply_unverified(
        &self,
        mut batch: GraphWriteBatch,
    ) -> Result<GraphCommit, GraphDbError> {
        let digest = batch.validate_and_digest()?;
        let _snapshot_gate = self.wait_snapshot_gate_write();
        let guard = self.write_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        if batch.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let mut state = self.state_write_guard()?;
        self.apply_locked(
            database,
            &mut state,
            batch,
            mutation::CommitMetadata::for_digest(digest),
            &mutation::RelationEndpointNamespaces::new(),
            &|| Ok(()),
        )
    }

    /// Rebuilds one disposable derived projection from its complete input.
    ///
    /// This replaces index materialization, never a canonical source record.
    pub fn replace_projection_unverified(
        &self,
        replacement: ProjectionReplacement,
    ) -> Result<GraphCommit, GraphDbError> {
        self.replace_projection_unverified_inner(replacement, ReplacementPrecondition::Unchecked)
    }

    /// Rebuilds a disposable projection only when its current watermark still
    /// matches the caller's observation. Producers that derive a complete
    /// replacement outside the graph write lock use this to prevent an older
    /// source snapshot from overwriting a newer publication.
    pub fn replace_projection_unverified_if_current(
        &self,
        replacement: ProjectionReplacement,
        expected_watermark: Option<&GraphWatermark>,
    ) -> Result<GraphCommit, GraphDbError> {
        self.replace_projection_unverified_inner(
            replacement,
            ReplacementPrecondition::Expected(expected_watermark),
        )
    }

    #[hotpath::measure(label = "graph_db.write.replace", impl_type = "GraphDb")]
    fn replace_projection_unverified_inner(
        &self,
        replacement: ProjectionReplacement,
        precondition: ReplacementPrecondition<'_>,
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
        self.run_gated_batch(
            &|| Ok(()),
            |database| {
                if let ReplacementPrecondition::Expected(expected) = precondition {
                    let current = latest_projection(
                        database,
                        &replacement.namespace,
                        &replacement.projection,
                    )?
                    .map(|state| state.commit.watermark);
                    if current.as_ref() != expected {
                        return Err(GraphDbError::Conflict);
                    }
                }
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
                Ok(GraphBatchPlan::Apply(
                    PreparedGraphBatch {
                        batch,
                        metadata: mutation::CommitMetadata::for_digest(digest),
                        endpoint_namespaces: mutation::RelationEndpointNamespaces::new(),
                        ensure_page_vector_indexes: false,
                    },
                    (),
                ))
            },
            |_database, commit, ()| Ok(commit),
        )
    }

    /// Records an idempotent derived-index publication.
    ///
    /// This is local replay metadata for a rebuildable graph projection, not
    /// publication to a durable source-of-truth authority.
    #[hotpath::measure(label = "graph_db.write.publish", impl_type = "GraphDb")]
    pub fn publish_unverified(
        &self,
        mut publication_request: GraphPublication,
    ) -> Result<GraphCommit, GraphDbError> {
        let digests = publication_request.validate_and_digest()?;
        let _snapshot_gate = self.wait_snapshot_gate_write();
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
            return if existing.digest == digests.publication {
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
        let publication_record = (
            publication_request.idempotency_key,
            digests.publication,
            publication_request.input_digest.as_str().to_owned(),
        );
        let mut state = self.state_write_guard()?;
        self.apply_locked(
            database,
            &mut state,
            publication_request.batch,
            mutation::CommitMetadata {
                digest: digests.batch,
                generation_dependency_digest: None,
                publication_record: Some(publication_record),
            },
            &mutation::RelationEndpointNamespaces::new(),
            &|| Ok(()),
        )
    }

    pub fn publication_receipt(
        &self,
        namespace: &GraphNamespace,
        idempotency_key: &GraphIdempotencyKey,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphPublicationReceipt>, GraphDbError> {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        publication(database, namespace, idempotency_key)?
            .map(|stored| {
                Ok(GraphPublicationReceipt {
                    digest: GraphPublicationDigest::from_persisted(stored.digest)?,
                    input_digest: GraphPublicationInputDigest::from_persisted(stored.input_digest)?,
                    commit: stored.commit,
                })
            })
            .transpose()
    }

    #[hotpath::measure(label = "graph_db.traversal", impl_type = "GraphDb")]
    pub fn traverse(&self, request: TraversalRequest) -> Result<TraversalResult, GraphDbError> {
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        self.ensure_start_projections_readable(
            database,
            &request.namespace,
            std::slice::from_ref(&request.start),
        )?;
        let result = traversal::traverse(database, request, &|namespace, projection| {
            self.ensure_projection_readable(namespace, projection)
        })?;
        #[cfg(feature = "hotpath")]
        {
            let edges = result
                .visits
                .iter()
                .filter(|visit| visit.via_relation.is_some())
                .count();
            crate::hotpath_observe::record_counts(result.visits.len(), edges, 0, 0);
            crate::hotpath_observe::record_hydration_source(
                crate::hotpath_observe::HydrationSource::Live,
            );
        }
        Ok(result)
    }

    #[hotpath::measure(label = "graph_db.traversal.outgoing_ids", impl_type = "GraphDb")]
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
        self.ensure_start_projections_readable(database, namespace, starts)?;
        let batches = traversal::outgoing_relation_ids(
            database,
            namespace,
            starts,
            relation_kinds,
            max_relations,
            cancellation.as_ref(),
            &|namespace, projection| self.ensure_projection_readable(namespace, projection),
        )?;
        #[cfg(feature = "hotpath")]
        {
            let edges = batches.iter().map(Vec::len).sum();
            crate::hotpath_observe::record_counts(starts.len(), edges, 0, 0);
            crate::hotpath_observe::record_hydration_source(
                crate::hotpath_observe::HydrationSource::Live,
            );
        }
        Ok(batches)
    }

    /// Bulk kind-filtered incoming fan-out: the counterpart of
    /// [`Self::outgoing_relation_ids`], with identical budget and
    /// cancellation semantics.
    #[hotpath::measure(label = "graph_db.traversal.incoming_ids", impl_type = "GraphDb")]
    pub fn incoming_relation_ids(
        &self,
        namespace: &GraphNamespace,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<GraphRelationKind>,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelationId>>, GraphDbError> {
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        self.ensure_start_projections_readable(database, namespace, starts)?;
        let batches = traversal::incoming_relation_ids(
            database,
            namespace,
            starts,
            relation_kinds,
            max_relations,
            cancellation.as_ref(),
            &|namespace, projection| self.ensure_projection_readable(namespace, projection),
        )?;
        #[cfg(feature = "hotpath")]
        {
            let edges = batches.iter().map(Vec::len).sum();
            crate::hotpath_observe::record_counts(starts.len(), edges, 0, 0);
            crate::hotpath_observe::record_hydration_source(
                crate::hotpath_observe::HydrationSource::Live,
            );
        }
        Ok(batches)
    }

    #[hotpath::measure(label = "graph_db.traversal.outgoing", impl_type = "GraphDb")]
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
        self.ensure_start_projections_readable(database, namespace, starts)?;
        let batches = traversal::outgoing_relations(
            database,
            namespace,
            starts,
            relation_kinds,
            max_relations,
            cancellation.as_ref(),
            &|namespace, projection| self.ensure_projection_readable(namespace, projection),
        )?;
        #[cfg(feature = "hotpath")]
        {
            let edges = batches.iter().map(Vec::len).sum();
            crate::hotpath_observe::record_counts(starts.len(), edges, 0, 0);
            crate::hotpath_observe::record_hydration_source(
                crate::hotpath_observe::HydrationSource::Live,
            );
        }
        Ok(batches)
    }

    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(label = "graph_db.traversal.reachable", impl_type = "GraphDb")]
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
        self.ensure_projection_readable(namespace, projection)?;
        self.ensure_outgoing_start_relations_readable(database, namespace, starts)?;
        let results = traversal::reachable_entities(
            database,
            namespace,
            projection,
            starts,
            relation_kinds,
            outgoing_overrides,
            max_visits,
            cancellation.as_ref(),
            &|namespace, projection| self.ensure_projection_readable(namespace, projection),
        )?;
        #[cfg(feature = "hotpath")]
        {
            let entities: usize = results.iter().map(BTreeSet::len).sum();
            crate::hotpath_observe::record_counts(entities, 0, 0, 0);
            crate::hotpath_observe::record_hydration_source(
                crate::hotpath_observe::HydrationSource::Live,
            );
        }
        Ok(results)
    }

    #[hotpath::measure(label = "graph_db.search.vector", impl_type = "GraphDb")]
    pub fn vector_search(
        &self,
        request: VectorSearchRequest,
    ) -> Result<VectorSearchResult, GraphDbError> {
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        self.ensure_projection_readable(&request.namespace, &request.projection)?;
        vector::vector_search(database, request)
    }

    pub fn vector_index_status(
        &self,
        request: GraphVectorIndexRequest,
    ) -> Result<GraphVectorIndexStatus, GraphDbError> {
        request.validate()?;
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        self.ensure_projection_readable(&request.namespace, &request.projection)?;
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
    #[hotpath::measure(label = "graph_db.vector.index.ensure", impl_type = "GraphDb")]
    pub fn ensure_vector_index(
        &self,
        request: GraphVectorIndexRequest,
    ) -> Result<GraphVectorIndexStatus, GraphDbError> {
        request.validate()?;
        let _snapshot_gate = self.wait_snapshot_gate_write();
        let guard = self.write_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        self.ensure_projection_readable(&request.namespace, &request.projection)?;
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

    /// Physically close the underlying graph database.
    ///
    /// TODO(grafeo-close-checkpoint): `GrafeoDB::close` flushes with
    /// `FlushReason::Explicit`, which re-serializes *every* section even when
    /// none is dirty — so this call rebuilds an already-durable store and is
    /// the dominant cost of the daemon's `daemon.shutdown.store_close` phase.
    /// The one-line fork fix (close is a checkpoint, not an explicit
    /// CHECKPOINT) is prepared but not yet on the pinned branch; see
    /// `docs/vendor-patches/grafeo-close-checkpoint-dirty-only.patch`. When it
    /// lands, bump the `grafeo-*` revs in the root `Cargo.toml`
    /// `[patch.crates-io]` block together and delete this note.
    ///
    /// There is no sound tracedecay-side workaround: skipping the close would
    /// forfeit the durability confirmation this function reports, and the
    /// dirty-section state that makes the skip safe is private to grafeo.
    #[hotpath::measure(label = "graph_db.runtime.close", impl_type = "GraphDb")]
    pub(crate) fn close(&self) -> Result<(), GraphDbError> {
        let _snapshot_gate = self.wait_snapshot_gate_write();
        let mut guard = match crate::hotpath_observe::wait_lock(
            crate::hotpath_observe::LOCK_WAIT_DATABASE_WRITE,
            || self.inner.database.write(),
        ) {
            Ok(guard) => guard,
            Err(_) => {
                self.inner.closed.store(true, Ordering::Release);
                self.inner.poisoned.store(true, Ordering::Release);
                return Err(GraphDbError::DurabilityUncertain {
                    message:
                        "graph database write lock is poisoned; physical close cannot be confirmed"
                            .to_owned(),
                });
            }
        };
        self.inner.identity_indexes.invalidate();
        let was_uncertain = self.inner.poisoned.load(Ordering::Acquire);
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return if was_uncertain {
                Err(durability_uncertain())
            } else {
                Err(GraphDbError::Closed)
            };
        }
        let Some(database) = guard.take() else {
            self.inner.poisoned.store(true, Ordering::Release);
            return Err(GraphDbError::DurabilityUncertain {
                message: "graph database was absent before physical close could be confirmed"
                    .to_owned(),
            });
        };
        if let Err(error) =
            hotpath::measure_block!("graph_db.runtime.close.engine", database.close())
        {
            self.inner.poisoned.store(true, Ordering::Release);
            return Err(GraphDbError::DurabilityUncertain {
                message: error.to_string(),
            });
        }
        if was_uncertain {
            Err(durability_uncertain())
        } else {
            Ok(())
        }
    }

    /// One shared snapshot-gate choreography for every batch that is derived
    /// from (or validated against) currently stored rows:
    ///
    /// 1. `derive` runs behind an upgradable claim — snapshot readers proceed
    ///    while writers queue, so the rows it reads and the bytes it hashes
    ///    stay exact without stalling reads.
    /// 2. An `Apply` plan upgrades atomically to the exclusive claim for the
    ///    write itself, then downgrades back; a `Settled` plan never takes
    ///    the exclusive claim at all.
    /// 3. `settle` runs behind the upgradable claim again, so post-apply
    ///    reads (bookkeeping, digest streams over the applied rows) also
    ///    admit readers.
    pub(crate) fn run_gated_batch<P, T>(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
        derive: impl FnOnce(&GrafeoDB) -> Result<GraphBatchPlan<P>, GraphDbError>,
        settle: impl FnOnce(&GrafeoDB, GraphCommit, P) -> Result<T, GraphDbError>,
    ) -> Result<T, GraphDbError> {
        let snapshot_gate = self.wait_snapshot_gate_upgradable();
        let plan = {
            let guard = self.read_guard()?;
            let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
            derive(database)?
        };
        let (commit, payload, _snapshot_gate) = match plan {
            GraphBatchPlan::Settled(commit, payload) => (commit, payload, snapshot_gate),
            GraphBatchPlan::Apply(prepared, payload) => {
                let write_gate = crate::hotpath_observe::wait_lock(
                    crate::hotpath_observe::LOCK_WAIT_SNAPSHOT_GATE_UPGRADE,
                    || RwLockUpgradableReadGuard::upgrade(snapshot_gate),
                );
                let commit = {
                    let guard = self.write_guard()?;
                    let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
                    let mut state = self.state_write_guard()?;
                    if prepared.ensure_page_vector_indexes {
                        ensure_vector_indexes_for_batch(database, &prepared.batch)?;
                    }
                    self.apply_locked(
                        database,
                        &mut state,
                        prepared.batch,
                        prepared.metadata,
                        &prepared.endpoint_namespaces,
                        check,
                    )?
                };
                (
                    commit,
                    payload,
                    ParkingRwLockWriteGuard::downgrade_to_upgradable(write_gate),
                )
            }
        };
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        settle(database, commit, payload)
    }

    pub(crate) fn apply_locked(
        &self,
        database: &GrafeoDB,
        state: &mut FormatState,
        batch: GraphWriteBatch,
        metadata: mutation::CommitMetadata,
        endpoint_namespaces: &mutation::RelationEndpointNamespaces,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphCommit, GraphDbError> {
        check()?;
        ensure_initial_vector_indexes(database, &batch)?;
        let mut vector_updates = Vec::new();
        for mutation in &batch.mutations {
            check()?;
            let GraphMutation::UpsertEntity(entity) = mutation else {
                continue;
            };
            for (name, property) in &entity.properties {
                check()?;
                let GraphProperty::Vector(vector) = property else {
                    continue;
                };
                vector_updates.push((
                    entity.identity.clone(),
                    vector_property_key(name, vector.dimension, vector.metric),
                    Value::Vector(vector.values.clone().into()),
                ));
            }
        }
        let namespace = batch.namespace.clone();
        let commit = mutation::apply(
            database,
            state,
            batch,
            metadata,
            endpoint_namespaces,
            &self.inner.poisoned,
            check,
        )?;
        // The Grafeo transaction (including any publication record) is durably
        // committed from this point. Cancellation is deliberately not observed
        // in the commit->refresh window: reporting Cancelled here would mistype
        // a committed write, skip HNSW refresh and WAL settlement on a Ready
        // handle, and retry would short-circuit as exact publication replay
        // without ever repairing them. Settlement failures instead poison the
        // handle and surface as typed DurabilityUncertain.
        for (identity, property, value) in vector_updates {
            let stored = match crate::state::load_entity(database, &namespace, &identity) {
                Ok(Some(stored)) => stored,
                Ok(None) => {
                    self.inner.poisoned.store(true, Ordering::Release);
                    return Err(GraphDbError::DurabilityUncertain {
                        message: format!(
                            "committed vector entity `{identity}` is missing from native identity index; commit settlement is incomplete"
                        ),
                    });
                }
                Err(error) => {
                    self.inner.poisoned.store(true, Ordering::Release);
                    return Err(GraphDbError::DurabilityUncertain {
                        message: format!(
                            "committed vector entity `{identity}` could not be read for native index refresh; commit settlement is incomplete: {error}"
                        ),
                    });
                }
            };
            require_committed_vector_scalar(database, stored.node, &property, &value).inspect_err(
                |_| {
                    self.inner.poisoned.store(true, Ordering::Release);
                },
            )?;
            // `mutation::apply` has already committed this exact scalar. Grafeo
            // Session mutations do not maintain HNSW, so this identical direct
            // write is index refresh only. The outer database write guard keeps
            // readers excluded; after reopen the non-durable index is Missing
            // until an explicit retained owner calls `ensure_vector_index`.
            if database.graph_store().has_vector_index(
                &vector::native_vector_label(&namespace, &stored.projection),
                &property,
            ) {
                database.set_node_property(stored.node, &property, value);
            }
        }
        if self.inner.durability == GraphDurability::WalSync
            && let Err(error) = sync_wal(database)
        {
            self.inner.poisoned.store(true, Ordering::Release);
            return Err(error);
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

    pub(crate) fn ensure_projection_readable(
        &self,
        namespace: &GraphNamespace,
        projection: &GraphProjectionId,
    ) -> Result<(), GraphDbError> {
        let quarantined = self
            .inner
            .quarantined_projections
            .read()
            .map_err(|_| GraphDbError::unavailable("graph quarantine lock is poisoned"))?;
        if quarantined.is_empty() {
            return Ok(());
        }
        let is_quarantined =
            quarantined
                .iter()
                .any(|(quarantined_namespace, quarantined_projection)| {
                    quarantined_namespace == namespace && quarantined_projection == projection
                });
        if is_quarantined {
            Err(projection_mismatch(
                namespace,
                projection,
                "recovered projection remains quarantined until replay verification succeeds",
            ))
        } else {
            Ok(())
        }
    }

    fn ensure_start_projections_readable(
        &self,
        database: &GrafeoDB,
        namespace: &GraphNamespace,
        starts: &[GraphEntityId],
    ) -> Result<(), GraphDbError> {
        for start in starts {
            if let Some(stored) = load_entity_locator(database, namespace, start)? {
                self.ensure_projection_readable(&stored.namespace, &stored.projection)?;
            }
        }
        Ok(())
    }

    fn ensure_outgoing_start_relations_readable(
        &self,
        database: &GrafeoDB,
        namespace: &GraphNamespace,
        starts: &[GraphEntityId],
    ) -> Result<(), GraphDbError> {
        for start in starts {
            let Some(stored) = load_entity_locator(database, namespace, start)? else {
                continue;
            };
            for projection in outgoing_relation_projections(database, stored.node)? {
                self.ensure_projection_readable(namespace, &projection)?;
            }
        }
        Ok(())
    }

    pub(crate) fn read_guard(&self) -> Result<RwLockReadGuard<'_, Option<GrafeoDB>>, GraphDbError> {
        self.ensure_available()?;
        let guard = crate::hotpath_observe::wait_lock(
            crate::hotpath_observe::LOCK_WAIT_DATABASE_READ,
            || self.inner.database.read(),
        )
        .map_err(|_| GraphDbError::unavailable("graph database read lock is poisoned"))?;
        self.ensure_available()?;
        Ok(guard)
    }

    /// Best-effort profiler observation that cannot change graph authority or
    /// operation outcomes. A closed or poisoned database simply has no
    /// trustworthy memory census to report.
    #[inline(always)]
    pub(crate) fn record_memory_checkpoint(
        &self,
        phase: crate::hotpath_observe::GrafeoMemoryPhase,
    ) {
        #[cfg(feature = "hotpath")]
        {
            let Ok(guard) = self.read_guard() else {
                return;
            };
            let Some(database) = guard.as_ref() else {
                return;
            };
            crate::hotpath_observe::record_grafeo_memory(database, phase);
        }
        #[cfg(not(feature = "hotpath"))]
        let _ = phase;
    }

    pub(crate) fn write_guard(
        &self,
    ) -> Result<RwLockWriteGuard<'_, Option<GrafeoDB>>, GraphDbError> {
        self.ensure_available()?;
        let guard = crate::hotpath_observe::wait_lock(
            crate::hotpath_observe::LOCK_WAIT_DATABASE_WRITE,
            || self.inner.database.write(),
        )
        .map_err(|_| GraphDbError::unavailable("graph database write lock is poisoned"))?;
        // Anything holding this guard may rewrite the rows a cached ordered
        // identity index was built from, so the index is stale from here on.
        self.inner.identity_indexes.invalidate();
        self.ensure_available()?;
        Ok(guard)
    }

    /// Bench-lane only: freeze the live LPG store into a columnar
    /// `CompactStore` base plus a fresh mutable overlay.
    ///
    /// This exists to *measure* an at-rest form for a sealed generation and is
    /// deliberately unreachable from any production path: it is gated on
    /// `test-helpers` so it cannot compile into a shipped binary, and nothing
    /// in this crate calls it. See `tests/at_rest_snapshot.rs` and
    /// `docs/graph-at-rest/README.md`.
    ///
    /// After this returns, `GrafeoDB::build_sections` emits a
    /// `SectionType::CompactStore` section on close instead of serializing the
    /// whole LPG block log, and the next open reconstructs the layered store
    /// from that section rather than replaying every mutation.
    ///
    /// # Errors
    ///
    /// Returns [`GraphDbError::Closed`] when the database is already closed and
    /// [`GraphDbError::Unavailable`] when grafeo refuses the conversion (for
    /// example more than 32,767 distinct labels or edge types).
    #[cfg(all(feature = "test-helpers", feature = "graph-disk-tier"))]
    pub fn compact_snapshot_for_bench(&self) -> Result<(), GraphDbError> {
        let mut guard = self.write_guard()?;
        let database = guard.as_mut().ok_or(GraphDbError::Closed)?;
        database
            .compact()
            .map_err(|error| GraphDbError::unavailable(format!("grafeo compact failed: {error}")))
    }

    pub(crate) fn state_write_guard(
        &self,
    ) -> Result<RwLockWriteGuard<'_, FormatState>, GraphDbError> {
        crate::hotpath_observe::wait_lock(crate::hotpath_observe::LOCK_WAIT_STATE_WRITE, || {
            self.inner.state.write()
        })
        .map_err(|_| GraphDbError::unavailable("graph state lock is poisoned"))
    }

    pub(crate) fn wait_snapshot_gate_write(&self) -> ParkingRwLockWriteGuard<'_, ()> {
        crate::hotpath_observe::wait_lock(
            crate::hotpath_observe::LOCK_WAIT_SNAPSHOT_GATE_WRITE,
            || self.inner.snapshot_gate.write(),
        )
    }

    pub(crate) fn wait_snapshot_gate_upgradable(&self) -> RwLockUpgradableReadGuard<'_, ()> {
        crate::hotpath_observe::wait_lock(
            crate::hotpath_observe::LOCK_WAIT_SNAPSHOT_GATE_UPGRADABLE,
            || self.inner.snapshot_gate.upgradable_read(),
        )
    }

    pub(crate) fn wait_verified_generations_write(
        &self,
    ) -> Result<RwLockWriteGuard<'_, crate::lease::VerifiedGenerationState>, GraphDbError> {
        crate::hotpath_observe::wait_lock(
            crate::hotpath_observe::LOCK_WAIT_VERIFIED_GENERATIONS,
            || self.inner.verified_generations.write(),
        )
        .map_err(|_| GraphDbError::unavailable("verified graph generation state lock is poisoned"))
    }

    pub(crate) fn ensure_available(&self) -> Result<(), GraphDbError> {
        if self.inner.poisoned.load(Ordering::Acquire) {
            return Err(durability_uncertain());
        }
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(GraphDbError::Closed);
        }
        Ok(())
    }
}

fn ensure_initial_vector_indexes(
    database: &GrafeoDB,
    batch: &GraphWriteBatch,
) -> Result<(), GraphDbError> {
    if latest_projection(database, &batch.namespace, &batch.projection)?.is_some() {
        return Ok(());
    }
    ensure_vector_indexes_for_batch(database, batch)
}

fn ensure_vector_indexes_for_batch(
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

fn require_committed_vector_scalar(
    database: &GrafeoDB,
    node: grafeo_common::types::NodeId,
    property: &str,
    expected: &Value,
) -> Result<(), GraphDbError> {
    let committed = database
        .graph_store()
        .get_node(node)
        .and_then(|node| node.get_property(property).cloned());
    if committed.as_ref() == Some(expected) {
        Ok(())
    } else {
        Err(GraphDbError::DurabilityUncertain {
            message: format!(
                "committed vector scalar `{property}` differs before native index refresh"
            ),
        })
    }
}

fn durability_uncertain() -> GraphDbError {
    GraphDbError::DurabilityUncertain {
        message: "the handle was poisoned after an observed post-commit persistence failure"
            .to_owned(),
    }
}

/// Flushes the public Grafeo WAL handle without touching internal checkpoint
/// metadata. A successful sync observes only the current WAL state: Grafeo's
/// session commit API can suppress an earlier WAL append error, so it cannot
/// establish a stronger durable-commit guarantee.
pub(crate) fn sync_wal(database: &GrafeoDB) -> Result<(), GraphDbError> {
    database
        .wal()
        .ok_or_else(|| GraphDbError::DurabilityUncertain {
            message: "persistent Grafeo database has no WAL after commit; durable outcome cannot be established"
                .to_owned(),
        })?
        .sync()
        .map_err(|error| GraphDbError::DurabilityUncertain {
            message: format!(
                "Grafeo WAL synchronization failed after commit; durable outcome cannot be established: {error}"
            ),
        })
}

pub(crate) use crate::schema::vector_property_key;

#[cfg(test)]
mod tests;
