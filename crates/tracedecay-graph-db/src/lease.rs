use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use parking_lot::RawRwLock;
use parking_lot::lock_api::ArcRwLockReadGuard;
#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
use sha2::{Digest, Sha256};
#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;
use tracedecay_store::runtime::GraphVerifiedHeadV1;
#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
use tracedecay_store::runtime::{
    BrainId, GraphGenerationIdV1, GraphNamespaceV1, GraphProjectionIdV1, GraphProjectionIdentityV1,
    GraphPublicationIdempotencyKeyV1, GraphPublicationInputDigestV1, GraphPublicationKeyV1,
    GraphPublicationSequenceV1, ProjectId, StoreShardIdV1, UserProfileId,
};

use crate::generation::physical_namespace;
use crate::{
    GraphCancellation, GraphDbError, GraphEntity, GraphEntityId, GraphEntityRef,
    GraphGenerationDependency, GraphGenerationId, GraphGenerationManifestIdentity,
    GraphGenerationRelation, GraphNamespace, GraphProjectionId, GraphProjectionIdentity,
    GraphProjectionPage, GraphProjectionReadRequest, GraphProjectionTelemetry,
    GraphProjectionTelemetryRequest, GraphRelation, GraphRelationId, GraphRelationRef,
    GraphVectorIndexRequest, GraphVectorIndexStatus, TraversalRequest, VectorSearchRequest,
    VectorSearchResult,
};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct GenerationLocator {
    pub(crate) projection: GraphProjectionIdentity,
    pub(crate) generation: GraphGenerationId,
}

impl GenerationLocator {
    pub(crate) fn new(projection: GraphProjectionIdentity, generation: GraphGenerationId) -> Self {
        Self {
            projection,
            generation,
        }
    }

    pub(crate) fn physical_namespace(&self) -> Result<GraphNamespace, GraphDbError> {
        physical_namespace(
            &self.projection.namespace,
            &self.projection.projection,
            &self.generation,
        )
    }
}

pub(crate) struct VerifiedGenerationLease {
    pub(crate) locator: GenerationLocator,
    pub(crate) head: GraphVerifiedHeadV1,
    pub(crate) dependency_identities: Vec<GraphGenerationDependency>,
    pub(crate) dependencies: BTreeMap<GraphProjectionIdentity, Arc<VerifiedGenerationLease>>,
}

impl fmt::Debug for VerifiedGenerationLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedGenerationLease")
            .field("locator", &self.locator)
            .field("dependencies", &self.dependency_identities)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
pub(crate) struct VerifiedGenerationState {
    pub(crate) heads: BTreeMap<GraphProjectionIdentity, Arc<VerifiedGenerationLease>>,
    pub(crate) known: BTreeMap<GenerationLocator, std::sync::Weak<VerifiedGenerationLease>>,
    pub(crate) quarantined: BTreeSet<GenerationLocator>,
    pub(crate) stored: BTreeMap<GenerationLocator, Vec<GenerationLocator>>,
    pub(crate) retiring: BTreeSet<GenerationLocator>,
    pub(crate) collected: BTreeSet<GenerationLocator>,
}

impl VerifiedGenerationState {
    pub(crate) fn retains(&self, locator: &GenerationLocator) -> bool {
        fn lease_retains(
            lease: &VerifiedGenerationLease,
            locator: &GenerationLocator,
            visited: &mut BTreeSet<GenerationLocator>,
        ) -> bool {
            if !visited.insert(lease.locator.clone()) {
                return false;
            }
            lease.locator == *locator
                || lease
                    .dependencies
                    .values()
                    .any(|dependency| lease_retains(dependency, locator, visited))
        }

        self.heads
            .values()
            .any(|lease| lease_retains(lease, locator, &mut BTreeSet::new()))
            || self
                .known
                .values()
                .filter_map(std::sync::Weak::upgrade)
                .any(|lease| lease_retains(&lease, locator, &mut BTreeSet::new()))
            // `stored` is a durable-row ledger, not a live reader. A
            // generation that wrote pages must still be reservable for
            // staged retirement once every lease has dropped. Dependencies
            // of generations that still have rows stay retained so a
            // still-present child cannot lose its base out from under it.
            || self.stored.iter().any(|(owner, dependencies)| {
                owner != locator && dependencies.contains(locator)
            })
    }

    fn sweep_dead_known(&mut self) {
        self.known.retain(|_, weak| weak.strong_count() > 0);
    }

    pub(crate) fn remember(
        &mut self,
        lease: &Arc<VerifiedGenerationLease>,
    ) -> Result<(), GraphDbError> {
        if self.retiring.contains(&lease.locator) || self.collected.contains(&lease.locator) {
            return Err(GraphDbError::conflict("lease.remember"));
        }
        self.quarantined.remove(&lease.locator);
        self.known
            .insert(lease.locator.clone(), Arc::downgrade(lease));
        self.stored.insert(
            lease.locator.clone(),
            lease
                .dependency_identities
                .iter()
                .map(|dependency| {
                    GenerationLocator::new(
                        dependency.projection.clone(),
                        dependency.generation.clone(),
                    )
                })
                .collect(),
        );
        self.sweep_dead_known();
        Ok(())
    }

    pub(crate) fn install(
        &mut self,
        head: Arc<VerifiedGenerationLease>,
    ) -> Result<Option<Arc<VerifiedGenerationLease>>, GraphDbError> {
        self.remember(&head)?;
        if self
            .heads
            .get(&head.locator.projection)
            .is_some_and(|current| current.head.sequence > head.head.sequence)
        {
            return Ok(self.heads.get(&head.locator.projection).cloned());
        }
        Ok(self.heads.insert(head.locator.projection.clone(), head))
    }

    pub(crate) fn quarantine(&mut self, locator: GenerationLocator) {
        self.quarantined.insert(locator);
    }

    pub(crate) fn head(
        &self,
        projection: &GraphProjectionIdentity,
    ) -> Option<Arc<VerifiedGenerationLease>> {
        self.heads.get(projection).cloned()
    }
}

#[derive(Clone)]
pub struct VerifiedGraphSnapshot {
    database: crate::GraphDbLeaseV1,
    head: Arc<VerifiedGenerationLease>,
    closure: BTreeMap<GraphProjectionIdentity, Arc<VerifiedGenerationLease>>,
    direct_sealed: bool,
}

impl fmt::Debug for VerifiedGraphSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedGraphSnapshot")
            .field("projection", &self.head.locator.projection)
            .field("generation", &self.head.locator.generation)
            .field("dependency_count", &self.head.dependencies.len())
            .finish_non_exhaustive()
    }
}

impl VerifiedGraphSnapshot {
    /// Builds a verified in-memory generation for hermetic adapters and
    /// evaluation fixtures. It verifies the applied state directly and makes
    /// no close/reopen durability claim. Persistent production publication
    /// must use [`crate::GraphDbRegistry::publish_verified`].
    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub fn memory(
        manifest: crate::GraphGenerationManifest,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Self, GraphDbError> {
        let owner = crate::GraphDbOwner::memory(Arc::clone(&cancellation))?;
        let database = owner.issue_lease()?;
        let check = || {
            if cancellation.is_cancelled() {
                Err(GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        let manifest = Arc::new(manifest);
        database.apply_generation_unverified(Arc::clone(&manifest), &check)?;
        let recovered_digest = database.verify_generation_in_place(&manifest, &check)?;
        let projection = GraphProjectionIdentityV1 {
            shard_id: StoreShardIdV1::project(
                BrainId::new("brain.graph-memory")
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?,
                UserProfileId::new("profile.graph-memory")
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?,
                ProjectId::new("project.graph-memory")
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            ),
            namespace: GraphNamespaceV1::new(manifest.projection.namespace.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            projection: GraphProjectionIdV1::new(manifest.projection.projection.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        };
        let head = GraphVerifiedHeadV1 {
            sequence: GraphPublicationSequenceV1::new(1)
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            key: GraphPublicationKeyV1::new(
                projection,
                GraphGenerationIdV1::new(manifest.generation.as_str())
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?,
                GraphPublicationIdempotencyKeyV1::new("graph-memory-publication")
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            ),
            input_digest: GraphPublicationInputDigestV1::new(encode_tagged_lowercase_hex(
                "sha256:",
                &Sha256::digest(recovered_digest.as_str().as_bytes()),
            ))
            .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            dependency_generation_closure_digest: manifest.dependency_closure_digest(&check)?,
            recovered_digest,
        };
        let lease = generation_lease(&manifest.identity(), head, BTreeMap::new());
        database.install_verified_generation(Arc::clone(&lease))?;
        let projection = manifest.projection.clone();
        drop(manifest);
        Ok(Self::new(
            database,
            Arc::clone(&lease),
            BTreeMap::from([(projection, lease)]),
        ))
    }

    pub(crate) fn new(
        database: crate::GraphDbLeaseV1,
        head: Arc<VerifiedGenerationLease>,
        closure: BTreeMap<GraphProjectionIdentity, Arc<VerifiedGenerationLease>>,
    ) -> Self {
        Self {
            database,
            head,
            closure,
            direct_sealed: false,
        }
    }

    pub(crate) fn new_direct_sealed(
        database: crate::GraphDbLeaseV1,
        head: Arc<VerifiedGenerationLease>,
    ) -> Self {
        let projection = head.locator.projection.clone();
        Self {
            database,
            head: Arc::clone(&head),
            closure: BTreeMap::from([(projection, head)]),
            direct_sealed: true,
        }
    }

    #[must_use]
    pub fn projection(&self) -> &GraphProjectionIdentity {
        &self.head.locator.projection
    }

    #[must_use]
    pub fn generation(&self) -> &GraphGenerationId {
        &self.head.locator.generation
    }

    /// Exact relational head whose lease backs this verified snapshot.
    #[must_use]
    pub fn verified_head(&self) -> &GraphVerifiedHeadV1 {
        &self.head.head
    }

    #[hotpath::measure(label = "graph_db.lease.entity", impl_type = "VerifiedGraphSnapshot")]
    pub fn entity(
        &self,
        reference: &GraphEntityRef,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphEntity>, GraphDbError> {
        self.with_operation(|| {
            let lease = self.lease_for_projection(&reference.projection)?;
            let namespace = lease.locator.physical_namespace()?;
            if self.direct_sealed {
                return self
                    .database
                    .entity(&namespace, &reference.identity, cancellation);
            }
            // A sealed generation's point reads serve from its compacted
            // per-generation store; the digest proved the exact row set, so
            // a miss there is authoritative and never re-read from staging.
            if let Some(sealed) = self.database.sealed_generation_reader(&lease.locator) {
                return sealed
                    .database()
                    .entity(&namespace, &reference.identity, cancellation);
            }
            self.database
                .entity(&namespace, &reference.identity, cancellation)
        })
    }

    pub fn relation(
        &self,
        reference: &GraphRelationRef,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphGenerationRelation>, GraphDbError> {
        self.with_operation(|| {
            let lease = self.lease_for_projection(&reference.projection)?;
            // The owning generation's sealed store holds the relation, its
            // edge, and full copies of any dependency-generation endpoints,
            // so the endpoint decode below stays inside one store.
            if let Some(sealed) = self.database.sealed_generation_reader(&lease.locator) {
                return sealed
                    .database()
                    .generation_relation(self, reference, cancellation);
            }
            self.database
                .generation_relation(self, reference, cancellation)
        })
    }

    #[hotpath::measure(
        label = "graph_db.lease.read_projection",
        impl_type = "VerifiedGraphSnapshot"
    )]
    pub fn read_projection(
        &self,
        mut request: GraphProjectionReadRequest,
    ) -> Result<GraphProjectionPage, GraphDbError> {
        self.require_head_projection(&request.namespace, &request.projection)?;
        request.namespace = self.head.locator.physical_namespace()?;
        self.with_operation(|| self.database.read_projection(request))
    }

    #[hotpath::measure(
        label = "graph_db.lease.projection_telemetry",
        impl_type = "VerifiedGraphSnapshot"
    )]
    pub fn projection_telemetry(
        &self,
        mut request: GraphProjectionTelemetryRequest,
    ) -> Result<Option<GraphProjectionTelemetry>, GraphDbError> {
        self.require_head_projection(&request.namespace, &request.projection)?;
        request.namespace = self.head.locator.physical_namespace()?;
        self.with_operation(|| self.database.projection_telemetry(request))
    }

    pub fn traverse(
        &self,
        request: TraversalRequest,
    ) -> Result<VerifiedTraversalResult, GraphDbError> {
        if request.namespace != self.head.locator.projection.namespace {
            return Err(GraphDbError::conflict("lease.traverse"));
        }
        self.with_operation(|| {
            // A dependency-free snapshot's whole closure is one generation,
            // so its sealed store holds every node and edge a traversal can
            // reach and the walk runs on the compacted CSR adjacency. A
            // closure that spans generations keeps the staging database,
            // whose native edges cross physical namespaces.
            if self.head.dependency_identities.is_empty()
                && let Some(sealed) = self.database.sealed_generation_reader(&self.head.locator)
            {
                return sealed.database().traverse_generation(self, request);
            }
            self.database.traverse_generation(self, request)
        })
    }

    #[hotpath::measure(
        label = "graph_db.lease.vector_search",
        impl_type = "VerifiedGraphSnapshot"
    )]
    pub fn vector_search(
        &self,
        mut request: VectorSearchRequest,
    ) -> Result<VectorSearchResult, GraphDbError> {
        self.require_head_projection(&request.namespace, &request.projection)?;
        request.namespace = self.head.locator.physical_namespace()?;
        self.with_operation(|| self.database.vector_search(request))
    }

    /// Typed coverage of the vector index serving this snapshot's head
    /// generation. Callers that require the index to cover a complete row
    /// set compare the reported vector count before trusting searches.
    #[hotpath::measure(
        label = "graph_db.lease.vector_index_status",
        impl_type = "VerifiedGraphSnapshot"
    )]
    pub fn vector_index_status(
        &self,
        mut request: GraphVectorIndexRequest,
    ) -> Result<GraphVectorIndexStatus, GraphDbError> {
        self.require_head_projection(&request.namespace, &request.projection)?;
        request.namespace = self.head.locator.physical_namespace()?;
        self.with_operation(|| self.database.vector_index_status(request))
    }

    #[hotpath::measure(
        label = "graph_db.lease.outgoing_relation_ids",
        impl_type = "VerifiedGraphSnapshot"
    )]
    pub fn outgoing_relation_ids(
        &self,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<crate::GraphRelationKind>,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelationId>>, GraphDbError> {
        self.with_operation(|| {
            self.database.outgoing_relation_ids(
                &self.head.locator.physical_namespace()?,
                starts,
                relation_kinds,
                max_relations,
                cancellation,
            )
        })
    }

    /// Bulk kind-filtered incoming fan-out over this verified snapshot.
    ///
    /// Plan 39 G7b: `VerifiedGraphSnapshot` is the sole production read
    /// boundary, and reverse adjacency (callers, impact, reverse
    /// reachability) previously had no bulk form here — only
    /// [`Self::outgoing_relation_ids`]. Exposing it lets those reads leave
    /// SQL `edges` joins without dropping their budgets.
    #[hotpath::measure(
        label = "graph_db.lease.incoming_relation_ids",
        impl_type = "VerifiedGraphSnapshot"
    )]
    pub fn incoming_relation_ids(
        &self,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<crate::GraphRelationKind>,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelationId>>, GraphDbError> {
        self.with_operation(|| {
            self.database.incoming_relation_ids(
                &self.head.locator.physical_namespace()?,
                starts,
                relation_kinds,
                max_relations,
                cancellation,
            )
        })
    }

    #[hotpath::measure(
        label = "graph_db.lease.outgoing_relation_ids_page",
        impl_type = "VerifiedGraphSnapshot"
    )]
    pub fn outgoing_relation_ids_page(
        &self,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<crate::GraphRelationKind>,
        after: Option<&GraphRelationId>,
        limit: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelationId>>, GraphDbError> {
        self.with_operation(|| {
            self.database.outgoing_relation_ids_page(
                &self.head.locator.physical_namespace()?,
                starts,
                relation_kinds,
                after,
                limit,
                cancellation,
            )
        })
    }

    #[hotpath::measure(
        label = "graph_db.lease.incoming_relation_ids_page",
        impl_type = "VerifiedGraphSnapshot"
    )]
    pub fn incoming_relation_ids_page(
        &self,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<crate::GraphRelationKind>,
        after: Option<&GraphRelationId>,
        limit: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelationId>>, GraphDbError> {
        self.with_operation(|| {
            self.database.incoming_relation_ids_page(
                &self.head.locator.physical_namespace()?,
                starts,
                relation_kinds,
                after,
                limit,
                cancellation,
            )
        })
    }

    /// Bulk outgoing relation rows over this verified generation. This keeps
    /// the traversal's already-decoded rows in hand for callers that need the
    /// relation payload, instead of reducing them to identities and issuing a
    /// point read for every edge.
    #[hotpath::measure(
        label = "graph_db.lease.outgoing_relations",
        impl_type = "VerifiedGraphSnapshot"
    )]
    pub fn outgoing_relations(
        &self,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<crate::GraphRelationKind>,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelation>>, GraphDbError> {
        self.with_operation(|| {
            self.database.outgoing_relations(
                &self.head.locator.physical_namespace()?,
                starts,
                relation_kinds,
                max_relations,
                cancellation,
            )
        })
    }

    /// Page-shaped outgoing fan-out: stops at `max_relations` instead of
    /// refusing the batch.
    #[hotpath::measure(
        label = "graph_db.lease.outgoing_relations_truncated",
        impl_type = "VerifiedGraphSnapshot"
    )]
    pub fn outgoing_relations_truncated(
        &self,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<crate::GraphRelationKind>,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelation>>, GraphDbError> {
        self.with_operation(|| {
            self.database.outgoing_relations_truncated(
                &self.head.locator.physical_namespace()?,
                starts,
                relation_kinds,
                max_relations,
                cancellation,
            )
        })
    }

    #[hotpath::measure(
        label = "graph_db.lease.outgoing_relation_targets",
        impl_type = "VerifiedGraphSnapshot"
    )]
    pub fn outgoing_relation_targets(
        &self,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<crate::GraphRelationKind>,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<crate::GraphRelationTarget>>, GraphDbError> {
        self.with_operation(|| {
            self.database.outgoing_relation_targets(
                &self.head.locator.physical_namespace()?,
                starts,
                relation_kinds,
                max_relations,
                cancellation,
            )
        })
    }

    pub fn visit_outgoing_relation_targets(
        &self,
        start: &GraphEntityId,
        relation_kinds: &BTreeSet<crate::GraphRelationKind>,
        cancellation: Arc<dyn GraphCancellation>,
        visitor: &mut dyn FnMut(crate::GraphRelationTarget),
    ) -> Result<usize, GraphDbError> {
        self.with_operation(|| {
            self.database.visit_outgoing_relation_targets(
                &self.head.locator.physical_namespace()?,
                start,
                relation_kinds,
                cancellation,
                visitor,
            )
        })
    }

    /// Bulk incoming relation rows over this verified generation.
    #[hotpath::measure(
        label = "graph_db.lease.incoming_relations",
        impl_type = "VerifiedGraphSnapshot"
    )]
    pub fn incoming_relations(
        &self,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<crate::GraphRelationKind>,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelation>>, GraphDbError> {
        self.with_operation(|| {
            self.database.incoming_relations(
                &self.head.locator.physical_namespace()?,
                starts,
                relation_kinds,
                max_relations,
                cancellation,
            )
        })
    }

    /// Page-shaped incoming fan-out: stops at `max_relations` instead of
    /// refusing the batch.
    #[hotpath::measure(
        label = "graph_db.lease.incoming_relations_truncated",
        impl_type = "VerifiedGraphSnapshot"
    )]
    pub fn incoming_relations_truncated(
        &self,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<crate::GraphRelationKind>,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelation>>, GraphDbError> {
        self.with_operation(|| {
            self.database.incoming_relations_truncated(
                &self.head.locator.physical_namespace()?,
                starts,
                relation_kinds,
                max_relations,
                cancellation,
            )
        })
    }

    /// Whether this snapshot's head generation currently serves from its
    /// sealed per-generation compact store (test observability).
    #[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
    #[must_use]
    pub fn serves_from_sealed_store(&self) -> bool {
        self.direct_sealed
            || self
                .database
                .sealed_generation_reader(&self.head.locator)
                .is_some()
    }

    pub(crate) fn lease_for_projection(
        &self,
        projection: &GraphProjectionIdentity,
    ) -> Result<&Arc<VerifiedGenerationLease>, GraphDbError> {
        self.closure
            .get(projection)
            .ok_or_else(|| GraphDbError::invalid("entity reference escapes snapshot closure"))
    }

    fn require_head_projection(
        &self,
        namespace: &GraphNamespace,
        projection: &GraphProjectionId,
    ) -> Result<(), GraphDbError> {
        if namespace != &self.head.locator.projection.namespace
            || projection != &self.head.locator.projection.projection
        {
            return Err(GraphDbError::conflict("lease.require_head_projection"));
        }
        Ok(())
    }

    fn with_operation<T>(
        &self,
        operation: impl FnOnce() -> Result<T, GraphDbError>,
    ) -> Result<T, GraphDbError> {
        let _lease: ArcRwLockReadGuard<RawRwLock, ()> = crate::hotpath_observe::wait_lock(
            crate::hotpath_observe::LOCK_WAIT_SNAPSHOT_GATE_READ,
            || self.database.inner.snapshot_gate.read_arc(),
        );
        operation()
    }

    pub(crate) fn namespace_projection_map(
        &self,
    ) -> Result<BTreeMap<GraphNamespace, GraphProjectionIdentity>, GraphDbError> {
        self.closure
            .values()
            .map(|lease| {
                Ok((
                    lease.locator.physical_namespace()?,
                    lease.locator.projection.clone(),
                ))
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedTraversalVisit {
    pub entity: GraphEntityRef,
    pub depth: usize,
    pub via_relation: Option<GraphRelationRef>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedTraversalResult {
    pub visits: Vec<VerifiedTraversalVisit>,
}

pub(crate) fn generation_lease(
    identity: &GraphGenerationManifestIdentity,
    head: GraphVerifiedHeadV1,
    dependencies: BTreeMap<GraphProjectionIdentity, Arc<VerifiedGenerationLease>>,
) -> Arc<VerifiedGenerationLease> {
    Arc::new(VerifiedGenerationLease {
        locator: GenerationLocator::new(identity.projection.clone(), identity.generation.clone()),
        head,
        dependency_identities: identity.dependencies.clone(),
        dependencies,
    })
}

#[cfg(test)]
mod tests {
    use super::{GenerationLocator, VerifiedGenerationLease, VerifiedGenerationState};
    use crate::{GraphGenerationId, GraphNamespace, GraphProjectionId, GraphProjectionIdentity};
    use std::sync::Arc;
    use tracedecay_store::runtime::{
        BrainId, GraphDependencyGenerationClosureDigestV1, GraphGenerationIdV1, GraphNamespaceV1,
        GraphProjectionIdV1, GraphProjectionIdentityV1, GraphPublicationIdempotencyKeyV1,
        GraphPublicationInputDigestV1, GraphPublicationKeyV1, GraphPublicationSequenceV1,
        GraphRecoveredGenerationDigestV1, GraphVerifiedHeadV1, ProjectId, StoreShardIdV1,
        UserProfileId,
    };

    const ZERO_DIGEST: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn test_lease(generation: &str) -> (GenerationLocator, Arc<VerifiedGenerationLease>) {
        let projection = GraphProjectionIdentity::new(
            GraphNamespace::new("namespace.lease-release").expect("namespace"),
            GraphProjectionId::new("projection.lease-release").expect("projection"),
        );
        let generation = GraphGenerationId::new(generation).expect("generation");
        let locator = GenerationLocator::new(projection.clone(), generation.clone());
        let shard = StoreShardIdV1::project(
            BrainId::new("brain.lease-release").expect("brain"),
            UserProfileId::new("profile.lease-release").expect("profile"),
            ProjectId::new("project.lease-release").expect("project"),
        );
        let head = GraphVerifiedHeadV1 {
            sequence: GraphPublicationSequenceV1::new(1).expect("sequence"),
            key: GraphPublicationKeyV1::new(
                GraphProjectionIdentityV1 {
                    shard_id: shard,
                    namespace: GraphNamespaceV1::new("namespace.lease-release").expect("namespace"),
                    projection: GraphProjectionIdV1::new("projection.lease-release")
                        .expect("projection"),
                },
                GraphGenerationIdV1::new(generation.as_str()).expect("generation id"),
                GraphPublicationIdempotencyKeyV1::new("publication.lease-release")
                    .expect("idempotency"),
            ),
            input_digest: GraphPublicationInputDigestV1::new(ZERO_DIGEST).expect("input digest"),
            dependency_generation_closure_digest: GraphDependencyGenerationClosureDigestV1::new(
                ZERO_DIGEST,
            )
            .expect("closure digest"),
            recovered_digest: GraphRecoveredGenerationDigestV1::new(ZERO_DIGEST)
                .expect("recovered digest"),
        };
        let lease = Arc::new(VerifiedGenerationLease {
            locator: locator.clone(),
            head,
            dependency_identities: Vec::new(),
            dependencies: Default::default(),
        });
        (locator, lease)
    }

    #[test]
    fn dropped_verified_lease_stops_retaining_and_is_not_kept_alive_by_stored_rows() {
        let (locator, lease) = test_lease("generation.lease-release");
        let probe = Arc::downgrade(&lease);
        let mut state = VerifiedGenerationState::default();
        state.remember(&lease).expect("remember live lease");
        assert!(
            state.retains(&locator),
            "a live verified lease must retain its generation"
        );
        assert!(
            state.stored.contains_key(&locator),
            "remembering a lease records its durable-row ledger entry"
        );

        drop(lease);
        assert!(
            probe.upgrade().is_none(),
            "dropping the last lease handle must release the generation"
        );
        assert!(
            !state.retains(&locator),
            "durable rows alone must not keep a generation unreclaimable after every lease drops"
        );

        state
            .remember(&test_lease("generation.lease-release-successor").1)
            .expect("remember successor");
        assert!(
            !state.known.contains_key(&locator),
            "remembering another generation must sweep the dead predecessor Weak"
        );
    }
}
