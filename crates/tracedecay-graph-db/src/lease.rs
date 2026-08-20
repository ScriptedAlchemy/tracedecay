use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use parking_lot::RawRwLock;
use parking_lot::lock_api::ArcRwLockReadGuard;
#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
use sha2::{Digest, Sha256};
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
    GraphGenerationDependency, GraphGenerationId, GraphGenerationManifest, GraphGenerationRelation,
    GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProjectionPage,
    GraphProjectionReadRequest, GraphProjectionTelemetry, GraphProjectionTelemetryRequest,
    GraphRelationId, GraphRelationRef, TraversalRequest, VectorSearchRequest, VectorSearchResult,
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
            || self.stored.contains_key(locator)
            || self
                .stored
                .values()
                .any(|dependencies| dependencies.contains(locator))
    }

    pub(crate) fn remember(
        &mut self,
        lease: &Arc<VerifiedGenerationLease>,
    ) -> Result<(), GraphDbError> {
        if self.retiring.contains(&lease.locator) || self.collected.contains(&lease.locator) {
            return Err(GraphDbError::Conflict);
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
        manifest: GraphGenerationManifest,
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
        database.apply_generation_unverified(&manifest, &check)?;
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
            input_digest: GraphPublicationInputDigestV1::new(format!(
                "sha256:{}",
                hex::encode(Sha256::digest(recovered_digest.as_str().as_bytes()))
            ))
            .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            dependency_generation_closure_digest: manifest.dependency_closure_digest(&check)?,
            recovered_digest,
        };
        let lease = generation_lease(&manifest, head, BTreeMap::new());
        database.install_verified_generation(Arc::clone(&lease))?;
        Ok(Self::new(
            database,
            Arc::clone(&lease),
            BTreeMap::from([(manifest.projection, lease)]),
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

    pub fn entity(
        &self,
        reference: &GraphEntityRef,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphEntity>, GraphDbError> {
        self.with_operation(|| {
            let lease = self.lease_for_projection(&reference.projection)?;
            self.database.entity(
                &lease.locator.physical_namespace()?,
                &reference.identity,
                cancellation,
            )
        })
    }

    pub fn relation(
        &self,
        reference: &GraphRelationRef,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphGenerationRelation>, GraphDbError> {
        self.with_operation(|| {
            self.database
                .generation_relation(self, reference, cancellation)
        })
    }

    pub fn read_projection(
        &self,
        mut request: GraphProjectionReadRequest,
    ) -> Result<GraphProjectionPage, GraphDbError> {
        self.require_head_projection(&request.namespace, &request.projection)?;
        request.namespace = self.head.locator.physical_namespace()?;
        self.with_operation(|| self.database.read_projection(request))
    }

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
            return Err(GraphDbError::Conflict);
        }
        self.with_operation(|| self.database.traverse_generation(self, request))
    }

    pub fn vector_search(
        &self,
        mut request: VectorSearchRequest,
    ) -> Result<VectorSearchResult, GraphDbError> {
        self.require_head_projection(&request.namespace, &request.projection)?;
        request.namespace = self.head.locator.physical_namespace()?;
        self.with_operation(|| self.database.vector_search(request))
    }

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
            return Err(GraphDbError::Conflict);
        }
        Ok(())
    }

    fn with_operation<T>(
        &self,
        operation: impl FnOnce() -> Result<T, GraphDbError>,
    ) -> Result<T, GraphDbError> {
        let _lease: ArcRwLockReadGuard<RawRwLock, ()> =
            self.database.inner.snapshot_gate.read_arc();
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
    manifest: &GraphGenerationManifest,
    head: GraphVerifiedHeadV1,
    dependencies: BTreeMap<GraphProjectionIdentity, Arc<VerifiedGenerationLease>>,
) -> Arc<VerifiedGenerationLease> {
    Arc::new(VerifiedGenerationLease {
        locator: GenerationLocator::new(manifest.projection.clone(), manifest.generation.clone()),
        head,
        dependency_identities: manifest.dependencies.clone(),
        dependencies,
    })
}
