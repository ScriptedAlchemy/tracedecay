use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, ChangedCodeChunkSetV1, CodeGenerationId, CodeSearchChunkId,
    ManifestDigest, VectorGenerationIdV1,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphEntityId, GraphNamespace, GraphProjectionId,
    GraphPropertyName, GraphVectorIndexRequest, GraphVectorIndexStatus, GraphWatermark,
    MAX_VECTOR_SEARCH_LIMIT, VectorMetric, VectorSearchRequest,
};
use tracedecay_store::{
    GraphNamespaceV1, GraphProjectionIdV1, GraphProjectionIdentityV1, SemanticVectorChunkDigest,
    SemanticVectorChunkId, SemanticVectorChunkManifestAccumulator,
    SemanticVectorChunkManifestDigest, SemanticVectorChunkManifestMember,
    SemanticVectorPublishedGenerationKey, SemanticVectorPublishedGenerationLookup,
    SemanticVectorStageChunkOperation, SemanticVectorStageRecord,
};

use crate::semantic_runtime::{
    RetainedSemanticVectorGraphV1, SemanticGraphExecutionAuthorityV1,
    VerifiedSemanticVectorGraphRuntimeV1,
};

use super::{
    BaseGenerationIncompatibilityV1, PreparedVectorGenerationV1, VectorGenerationBuildIdV1,
    VectorGenerationPlanV1, VectorGenerationPublicationV1, VectorGenerationStateMachineV1,
    VectorGenerationStoreErrorV1, VectorProjectionCheckpointV1,
};

mod evaluation_runtime;
mod native_records;
mod persistence;
mod retention;
mod snapshot;
mod stage_identity;
pub(super) mod transitions;

use native_records::{
    PublishedBaseRecover, ScopedGenerationRecordsV1, peek_generation_base, read_build_records,
    read_cataloged_generation_records, read_generation_catalog, read_generation_catalog_entry,
    read_generation_metadata, read_generation_records_with_recover, read_state_metadata,
};

#[cfg(test)]
pub(crate) use native_records::encode_generation_batch_delta;
use persistence::{
    check_cancelled, map_graph_error, resident_size_overflow, search_vector_property,
    storage_error, vector_metric,
};
use snapshot::SemanticVectorVerifiedReadV1;

pub use evaluation_runtime::{
    IsolatedSemanticEvaluationGraphV1, isolated_semantic_evaluation_graph,
};

pub const SEMANTIC_VECTOR_GRAPH_PROJECTION: &str = "tracedecay.semantic-vector.graph";
const GRAPH_OPERATION_DEADLINE: Duration = Duration::from_secs(30);
/// Finite authority for daemon-owned, corpus-scaled graph work. This matches
/// the isolated 10x-corpus evaluation ceiling while preserving lifecycle
/// cancellation as the earlier reclamation path.
pub const GRAPH_BACKGROUND_OPERATION_BUDGET: Duration = Duration::from_secs(15 * 60);

pub struct GraphVectorGenerationStoreV1 {
    runtime: Arc<dyn VerifiedSemanticVectorGraphRuntimeV1>,
    snapshot: Mutex<Option<SemanticVectorVerifiedReadV1>>,
    descriptor: Mutex<Option<SemanticVectorStageDescriptorV1>>,
    pending: Mutex<BTreeMap<VectorGenerationBuildIdV1, PendingSemanticVectorBuildV1>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum VectorGenerationBeginOutcomeV1 {
    ReplayFromStart {
        build_id: VectorGenerationBuildIdV1,
    },
    AlreadyPublished {
        build_id: VectorGenerationBuildIdV1,
        publication: VectorGenerationPublicationV1,
    },
}

impl VectorGenerationBeginOutcomeV1 {
    pub fn build_id(&self) -> &VectorGenerationBuildIdV1 {
        match self {
            Self::ReplayFromStart { build_id } | Self::AlreadyPublished { build_id, .. } => {
                build_id
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticVectorStageDescriptorV1 {
    projection: AdmittedEmbeddingProjectionKeyV1,
    expected_chunk_count: u64,
    expected_chunk_manifest_digest: SemanticVectorChunkManifestDigest,
}

struct PendingSemanticVectorBuildV1 {
    state: VectorGenerationStateMachineV1,
    stage: SemanticVectorStageRecord,
    revision: u64,
    publication: Option<VectorGenerationPublicationV1>,
}

impl SemanticVectorStageDescriptorV1 {
    pub fn from_changes(
        projection: AdmittedEmbeddingProjectionKeyV1,
        changes: &ChangedCodeChunkSetV1,
    ) -> Result<Self, VectorGenerationStoreErrorV1> {
        let (expected_chunk_count, expected_chunk_manifest_digest) =
            semantic_vector_stage_manifest(changes)?;
        Ok(Self {
            projection,
            expected_chunk_count,
            expected_chunk_manifest_digest,
        })
    }
}

/// Folds the three already-canonical change partitions into one canonical
/// manifest without retaining another corpus-sized collection. The durable
/// writer remains bounded by its batch/page limits; project size is not an
/// admission limit.
fn semantic_vector_stage_manifest(
    changes: &ChangedCodeChunkSetV1,
) -> Result<(u64, SemanticVectorChunkManifestDigest), VectorGenerationStoreErrorV1> {
    let mut added = changes.added_or_changed.iter().peekable();
    let mut reused = changes.reused.iter().peekable();
    let mut deleted = changes.deleted.iter().peekable();
    let mut accumulator = SemanticVectorChunkManifestAccumulator::new();
    let mut expected_chunk_count = 0_u64;

    loop {
        let source = [
            added.peek().map(|change| (&change.chunk_id, 0_u8)),
            reused.peek().map(|change| (&change.chunk_id, 1_u8)),
            deleted.peek().map(|change| (&change.chunk_id, 2_u8)),
        ]
        .into_iter()
        .flatten()
        .min_by(|left, right| left.0.cmp(right.0))
        .map(|(_, source)| source);
        let Some(source) = source else {
            break;
        };
        let (change, operation) = match source {
            0 => (
                added.next().ok_or_else(|| {
                    VectorGenerationStoreErrorV1::InvalidPlan(
                        "semantic vector added partition changed during manifest fold".to_owned(),
                    )
                })?,
                SemanticVectorStageChunkOperation::Embed,
            ),
            1 => (
                reused.next().ok_or_else(|| {
                    VectorGenerationStoreErrorV1::InvalidPlan(
                        "semantic vector reused partition changed during manifest fold".to_owned(),
                    )
                })?,
                SemanticVectorStageChunkOperation::Reuse,
            ),
            _ => (
                deleted.next().ok_or_else(|| {
                    VectorGenerationStoreErrorV1::InvalidPlan(
                        "semantic vector deleted partition changed during manifest fold".to_owned(),
                    )
                })?,
                SemanticVectorStageChunkOperation::Tombstone,
            ),
        };
        let digest = match operation {
            SemanticVectorStageChunkOperation::Embed | SemanticVectorStageChunkOperation::Reuse => {
                change.current_digest.as_ref().ok_or_else(|| {
                    VectorGenerationStoreErrorV1::InvalidPlan(
                        "semantic vector live member has no current digest".to_owned(),
                    )
                })?
            }
            SemanticVectorStageChunkOperation::Tombstone => {
                change.prior_digest.as_ref().ok_or_else(|| {
                    VectorGenerationStoreErrorV1::InvalidPlan(
                        "semantic vector tombstone has no prior digest".to_owned(),
                    )
                })?
            }
        };
        accumulator
            .push(&SemanticVectorChunkManifestMember {
                chunk_id: SemanticVectorChunkId::new(change.chunk_id.to_string())
                    .map_err(storage_error)?,
                chunk_digest: SemanticVectorChunkDigest::new(digest.as_str())
                    .map_err(storage_error)?,
                operation,
            })
            .map_err(storage_error)?;
        expected_chunk_count = expected_chunk_count.checked_add(1).ok_or_else(|| {
            VectorGenerationStoreErrorV1::InvalidPlan(
                "semantic vector chunk count exceeds u64".to_owned(),
            )
        })?;
    }

    Ok((
        expected_chunk_count,
        accumulator.finish().map_err(storage_error)?,
    ))
}

#[cfg(test)]
mod stage_descriptor_tests {
    use super::*;
    use tracedecay_domain::{ChangedCodeChunkV1, ContentDigest};

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("canonical test identity")
    }

    fn digest(byte: char) -> ContentDigest {
        id(&format!("sha256:{}", byte.to_string().repeat(64)))
    }

    fn change(
        chunk_id: &str,
        prior_digest: Option<char>,
        current_digest: Option<char>,
    ) -> ChangedCodeChunkV1 {
        ChangedCodeChunkV1 {
            chunk_id: id(chunk_id),
            prior_digest: prior_digest.map(digest),
            current_digest: current_digest.map(digest),
        }
    }

    #[test]
    fn stage_manifest_stream_merge_preserves_canonical_cross_partition_order() {
        let changes = ChangedCodeChunkSetV1 {
            from_generation: Some(id("generation.base")),
            to_generation: id("generation.target"),
            manifest_digest: id(&format!("sha256:{}", "d".repeat(64))),
            added_or_changed: vec![change("chunk.b", None, Some('b'))],
            deleted: vec![change("chunk.a", Some('a'), None)],
            reused: vec![change("chunk.c", Some('c'), Some('c'))],
        };
        let (count, actual) = semantic_vector_stage_manifest(&changes).expect("stream manifest");
        let expected = tracedecay_store::semantic_vector_chunk_manifest_digest(&[
            SemanticVectorChunkManifestMember {
                chunk_id: SemanticVectorChunkId::new("chunk.a").expect("chunk id"),
                chunk_digest: SemanticVectorChunkDigest::new(digest('a').as_str())
                    .expect("chunk digest"),
                operation: SemanticVectorStageChunkOperation::Tombstone,
            },
            SemanticVectorChunkManifestMember {
                chunk_id: SemanticVectorChunkId::new("chunk.b").expect("chunk id"),
                chunk_digest: SemanticVectorChunkDigest::new(digest('b').as_str())
                    .expect("chunk digest"),
                operation: SemanticVectorStageChunkOperation::Embed,
            },
            SemanticVectorChunkManifestMember {
                chunk_id: SemanticVectorChunkId::new("chunk.c").expect("chunk id"),
                chunk_digest: SemanticVectorChunkDigest::new(digest('c').as_str())
                    .expect("chunk digest"),
                operation: SemanticVectorStageChunkOperation::Reuse,
            },
        ])
        .expect("reference manifest digest");

        assert_eq!(count, 3);
        assert_eq!(actual, expected);
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedGraphVectorGenerationSnapshotV1 {
    revision: u64,
    generation: super::PublishedVectorGenerationV1,
}

impl VerifiedGraphVectorGenerationSnapshotV1 {
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn generation(&self) -> &super::PublishedVectorGenerationV1 {
        &self.generation
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedVectorResidentPlanV1 {
    pub watermark: GraphWatermark,
    pub generation_id: VectorGenerationIdV1,
    pub retained_bytes: u64,
    pub hydration_peak_bytes: u64,
}

pub struct ResidentVectorRowV1 {
    pub chunk_id: CodeSearchChunkId,
    pub values: Box<[f32]>,
}

/// One generation-bound persisted ANN index, retained with the verified
/// snapshot lease it serves from.
///
/// `indexed` is the persisted index's census: the index covers exactly the
/// vectors written as native entities of this generation's namespace, so a
/// caller serving a row set that also hydrates reused base-generation rows
/// compares `indexed` against its resident row count before trusting
/// searches for candidate generation.
pub struct SemanticAnnServingIndexV1 {
    snapshot: snapshot::SemanticVectorVerifiedReadV1,
    namespace: GraphNamespace,
    projection: GraphProjectionId,
    property: GraphPropertyName,
    dimension: usize,
    metric: VectorMetric,
    chunks_by_entity: BTreeMap<GraphEntityId, CodeSearchChunkId>,
    indexed: u64,
    cancellation: Arc<dyn GraphCancellation>,
}

impl SemanticAnnServingIndexV1 {
    pub const fn indexed(&self) -> u64 {
        self.indexed
    }

    /// Index-nearest serving chunks for one transient query vector, in
    /// ascending index-distance order. The query is searched once and not
    /// retained. An index hit that does not map to a serving chunk is a
    /// corrupt index/row divergence and fails closed.
    pub fn search(
        &self,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<CodeSearchChunkId>, VectorGenerationStoreErrorV1> {
        check_cancelled(self.cancellation.as_ref())?;
        let result = self
            .snapshot
            .vector_search(VectorSearchRequest {
                namespace: self.namespace.clone(),
                projection: self.projection.clone(),
                property: self.property.clone(),
                query: query.to_vec(),
                dimension: self.dimension,
                metric: self.metric,
                limit: limit.min(MAX_VECTOR_SEARCH_LIMIT),
                cancellation: Arc::clone(&self.cancellation),
            })
            .map_err(map_graph_error)?;
        result
            .matches
            .into_iter()
            .map(|found| {
                self.chunks_by_entity
                    .get(&found.entity)
                    .cloned()
                    .ok_or_else(|| {
                        VectorGenerationStoreErrorV1::Corrupt(
                            "semantic vector index returned an entity that is not a serving row"
                                .to_owned(),
                        )
                    })
            })
            .collect()
    }
}

impl GraphVectorGenerationStoreV1 {
    pub fn open(
        retained: &RetainedSemanticVectorGraphV1,
    ) -> Result<Self, VectorGenerationStoreErrorV1> {
        let cancellation = Arc::clone(retained.cancellation());
        let store = Self::read_only(retained)?;
        check_cancelled(cancellation.as_ref())?;
        if store.optional_snapshot()?.is_some() {
            store.verify_existing_state(cancellation)?;
        }
        Ok(store)
    }

    /// Read-only handle over an already-resolved graph runtime. Unlike
    /// [`Self::open`] this never installs or verifies the projection: a graph
    /// that has never published a semantic-vector generation reads as "no
    /// vectors" on the identity-filtered read surface.
    pub fn read_only(
        retained: &RetainedSemanticVectorGraphV1,
    ) -> Result<Self, VectorGenerationStoreErrorV1> {
        let runtime = Arc::clone(retained.runtime());
        let authority = SemanticGraphExecutionAuthorityV1::new(
            Arc::clone(retained.cancellation()),
            Instant::now() + GRAPH_OPERATION_DEADLINE,
        );
        let snapshot = runtime
            .recover_verified_snapshot(&authority)
            .map_err(map_graph_error)?
            .map(SemanticVectorVerifiedReadV1::new);
        Ok(Self {
            runtime,
            snapshot: Mutex::new(snapshot),
            descriptor: Mutex::new(None),
            pending: Mutex::new(BTreeMap::new()),
        })
    }

    /// Recover the one verified physical graph generation bound to a stable
    /// semantic generation identity. Serving callers use the configured
    /// semantic pin here; graph head order is never an activation authority.
    pub fn read_only_generation(
        retained: &RetainedSemanticVectorGraphV1,
        generation_id: &VectorGenerationIdV1,
    ) -> Result<Option<Self>, VectorGenerationStoreErrorV1> {
        let runtime = Arc::clone(retained.runtime());
        let authority = SemanticGraphExecutionAuthorityV1::new(
            Arc::clone(retained.cancellation()),
            Instant::now() + GRAPH_OPERATION_DEADLINE,
        );
        let (_, binding) = runtime.staging_binding();
        let scope = runtime.scope();
        let key = SemanticVectorPublishedGenerationKey {
            projection: GraphProjectionIdentityV1 {
                shard_id: binding.shard_id.clone(),
                namespace: GraphNamespaceV1::new(scope.projection().namespace.as_str())
                    .map_err(storage_error)?,
                projection: GraphProjectionIdV1::new(scope.projection().projection.as_str())
                    .map_err(storage_error)?,
            },
            semantic_generation_id: generation_id.clone(),
        };
        let (record, verified_head) = match runtime
            .published_semantic_generation(&key, &authority)
            .map_err(map_graph_error)?
        {
            SemanticVectorPublishedGenerationLookup::Missing => return Ok(None),
            SemanticVectorPublishedGenerationLookup::Published {
                record,
                verified_head,
            } => (record, verified_head),
        };
        if record.plan.semantic_generation_id != *generation_id
            || record.plan.publication_key != verified_head.key
        {
            return Err(VectorGenerationStoreErrorV1::Corrupt(
                "published semantic mapping returned foreign generation evidence".to_owned(),
            ));
        }
        let snapshot = runtime
            .recover_verified_generation(&verified_head.key, &authority)
            .map_err(map_graph_error)?;
        if snapshot.verified_head() != verified_head.as_ref() {
            return Err(map_graph_error(GraphDbError::conflict_observed(
                "usecases.store.read_only_generation.verified_head",
                format!("verified_head={verified_head:?}"),
                format!("verified_head={:?}", snapshot.verified_head()),
            )));
        }
        Ok(Some(Self {
            runtime,
            snapshot: Mutex::new(Some(SemanticVectorVerifiedReadV1::new(snapshot))),
            descriptor: Mutex::new(None),
            pending: Mutex::new(BTreeMap::new()),
        }))
    }

    pub fn configure_stage(
        &self,
        descriptor: SemanticVectorStageDescriptorV1,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        let mut current = self.descriptor.lock().map_err(|_| {
            VectorGenerationStoreErrorV1::Unavailable(
                "semantic vector stage descriptor lock is poisoned".to_owned(),
            )
        })?;
        match current.as_ref() {
            Some(existing) if existing != &descriptor => {
                Err(map_graph_error(GraphDbError::conflict_observed(
                    "usecases.store.configure_stage",
                    format!("descriptor={existing:?}"),
                    format!("descriptor={descriptor:?}"),
                )))
            }
            Some(_) => Ok(()),
            None => {
                *current = Some(descriptor);
                Ok(())
            }
        }
    }

    fn optional_snapshot(
        &self,
    ) -> Result<Option<SemanticVectorVerifiedReadV1>, VectorGenerationStoreErrorV1> {
        self.snapshot
            .lock()
            .map_err(|_| {
                VectorGenerationStoreErrorV1::Unavailable(
                    "semantic vector verified snapshot lock is poisoned".to_owned(),
                )
            })
            .map(|snapshot| snapshot.clone())
    }

    fn snapshot(&self) -> Result<SemanticVectorVerifiedReadV1, VectorGenerationStoreErrorV1> {
        self.optional_snapshot()?.ok_or_else(|| {
            VectorGenerationStoreErrorV1::Unavailable(
                "semantic vector projection has no verified generation".to_owned(),
            )
        })
    }

    fn install_snapshot(
        &self,
        snapshot: tracedecay_graph_db::VerifiedGraphSnapshot,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        let mut current = self.snapshot.lock().map_err(|_| {
            VectorGenerationStoreErrorV1::Unavailable(
                "semantic vector verified snapshot lock is poisoned".to_owned(),
            )
        })?;
        *current = Some(SemanticVectorVerifiedReadV1::new(snapshot));
        Ok(())
    }

    fn refresh_snapshot(
        &self,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<Option<SemanticVectorVerifiedReadV1>, VectorGenerationStoreErrorV1> {
        let recovered = self
            .runtime
            .recover_verified_snapshot(authority)
            .map_err(map_graph_error)?
            .map(SemanticVectorVerifiedReadV1::new);
        let mut current = self.snapshot.lock().map_err(|_| {
            VectorGenerationStoreErrorV1::Unavailable(
                "semantic vector verified snapshot lock is poisoned".to_owned(),
            )
        })?;
        *current = recovered.clone();
        Ok(recovered)
    }

    #[hotpath::measure(label = "usecases.store.verify_state")]
    fn verify_existing_state(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        check_cancelled(cancellation.as_ref())?;
        let snapshot = self.snapshot()?;
        let catalog = read_generation_catalog(&snapshot, Arc::clone(&cancellation))?;
        if catalog.len() != 1 {
            return Err(VectorGenerationStoreErrorV1::Corrupt(
                "verified semantic vector graph must contain exactly one generation".to_owned(),
            ));
        }
        let generation_id = catalog[0].generation_id.clone();
        drop(snapshot);
        self.read_cataloged_hydrating_published_bases(&generation_id, Arc::clone(&cancellation))?
            .ok_or_else(|| {
                VectorGenerationStoreErrorV1::Corrupt(
                    "verified semantic vector generation records are missing".to_owned(),
                )
            })?;
        check_cancelled(cancellation.as_ref())?;
        Ok(())
    }

    /// Receipt-only incremental generations keep reused vectors on the live
    /// published base identity. Recover that published snapshot only after
    /// dropping the current verified read: isolated evaluation's SQLite writer
    /// cannot open another generation while a reader snapshot is still live.
    fn read_cataloged_hydrating_published_bases(
        &self,
        generation_id: &VectorGenerationIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<ScopedGenerationRecordsV1>, VectorGenerationStoreErrorV1> {
        let Some(snapshot) = self.optional_snapshot()? else {
            let cache =
                self.preload_published_lineage(Some(generation_id), Arc::clone(&cancellation))?;
            return Ok(cache.get(generation_id).cloned());
        };
        let catalog =
            read_generation_catalog_entry(&snapshot, generation_id, Arc::clone(&cancellation))?;
        let Some(catalog) = catalog else {
            drop(snapshot);
            let cache =
                self.preload_published_lineage(Some(generation_id), Arc::clone(&cancellation))?;
            return Ok(cache.get(generation_id).cloned());
        };
        let base = catalog.base_generation.clone();
        drop(snapshot);
        let cache = self.preload_published_lineage(base.as_ref(), Arc::clone(&cancellation))?;
        let snapshot = self.snapshot()?;
        let recover: &PublishedBaseRecover<'_> =
            &|generation, _, _| Ok(cache.get(generation).cloned());
        read_cataloged_generation_records(&snapshot, generation_id, cancellation, Some(recover))
    }

    #[hotpath::measure(label = "usecases.store.recover_lineage")]
    fn preload_published_lineage(
        &self,
        start: Option<&VectorGenerationIdV1>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<
        BTreeMap<VectorGenerationIdV1, ScopedGenerationRecordsV1>,
        VectorGenerationStoreErrorV1,
    > {
        let mut chain = Vec::new();
        let mut current = start.cloned();
        let mut seen = BTreeSet::new();
        while let Some(generation_id) = current {
            if !seen.insert(generation_id.clone()) {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "semantic vector generation base lineage is cyclic".to_owned(),
                ));
            }
            chain.push(generation_id.clone());
            let snapshot = self
                .load_published_generation_snapshot(&generation_id, Arc::clone(&cancellation))?
                .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration(
                    BaseGenerationIncompatibilityV1::MissingPublished,
                ))?;
            current = peek_generation_base(&snapshot, &generation_id, Arc::clone(&cancellation))?;
        }
        crate::hotpath_observe::vector_lineage_depth(chain.len());
        let mut cache = BTreeMap::new();
        for generation_id in chain.into_iter().rev() {
            let snapshot = self
                .load_published_generation_snapshot(&generation_id, Arc::clone(&cancellation))?
                .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration(
                    BaseGenerationIncompatibilityV1::MissingPublished,
                ))?;
            let recover: &PublishedBaseRecover<'_> =
                &|generation, _, _| Ok(cache.get(generation).cloned());
            let records = read_generation_records_with_recover(
                &snapshot,
                &generation_id,
                Arc::clone(&cancellation),
                Some(recover),
            )?
            .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration(
                BaseGenerationIncompatibilityV1::MissingSnapshot,
            ))?;
            cache.insert(generation_id, records);
        }
        Ok(cache)
    }

    #[hotpath::measure(label = "usecases.store.recover_published")]
    fn load_published_generation_snapshot(
        &self,
        generation_id: &VectorGenerationIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<SemanticVectorVerifiedReadV1>, VectorGenerationStoreErrorV1> {
        let authority = SemanticGraphExecutionAuthorityV1::new(
            cancellation,
            Instant::now() + GRAPH_OPERATION_DEADLINE,
        );
        let (_, binding) = self.runtime.staging_binding();
        let scope = self.runtime.scope();
        let key = SemanticVectorPublishedGenerationKey {
            projection: GraphProjectionIdentityV1 {
                shard_id: binding.shard_id.clone(),
                namespace: GraphNamespaceV1::new(scope.projection().namespace.as_str())
                    .map_err(storage_error)?,
                projection: GraphProjectionIdV1::new(scope.projection().projection.as_str())
                    .map_err(storage_error)?,
            },
            semantic_generation_id: generation_id.clone(),
        };
        let (record, verified_head) = match self
            .runtime
            .published_semantic_generation(&key, &authority)
            .map_err(map_graph_error)?
        {
            SemanticVectorPublishedGenerationLookup::Missing => return Ok(None),
            SemanticVectorPublishedGenerationLookup::Published {
                record,
                verified_head,
            } => (record, verified_head),
        };
        if record.plan.semantic_generation_id != *generation_id
            || record.plan.publication_key != verified_head.key
        {
            return Err(VectorGenerationStoreErrorV1::Corrupt(
                "published semantic mapping returned foreign generation evidence".to_owned(),
            ));
        }
        let snapshot = self
            .runtime
            .recover_verified_generation(&verified_head.key, &authority)
            .map_err(map_graph_error)?;
        if snapshot.verified_head() != verified_head.as_ref() {
            return Err(map_graph_error(GraphDbError::conflict_observed(
                "usecases.store.load_published_generation.verified_head",
                format!("verified_head={verified_head:?}"),
                format!("verified_head={:?}", snapshot.verified_head()),
            )));
        }
        Ok(Some(SemanticVectorVerifiedReadV1::new(snapshot)))
    }

    #[hotpath::measure(label = "usecases.store.begin_generation", future = true)]
    pub async fn begin_generation(
        &self,
        plan: VectorGenerationPlanV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationBeginOutcomeV1, VectorGenerationStoreErrorV1> {
        self.begin_generation_records(plan, false, cancellation)
    }

    #[hotpath::measure(label = "usecases.store.rebuild_generation", future = true)]
    pub async fn rebuild_generation(
        &self,
        plan: VectorGenerationPlanV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationBeginOutcomeV1, VectorGenerationStoreErrorV1> {
        self.begin_generation_records(plan, true, cancellation)
    }

    #[hotpath::measure(label = "usecases.store.cancel_generation", future = true)]
    pub async fn cancel_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        self.cancel_generation_records(build_id, cancellation)
    }

    #[hotpath::measure(label = "usecases.store.commit_batch", future = true)]
    pub async fn commit_batch(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        self.commit_batch_records(build_id, expected_checkpoint, prepared, cancellation)
    }

    #[hotpath::measure(label = "usecases.store.publish_generation", future = true)]
    pub async fn publish_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        self.publish_generation_records(build_id, cancellation)
    }

    /// Read one exact semantic generation from an already identity-selected
    /// verified physical snapshot.
    #[hotpath::measure(label = "usecases.store.generation_snapshot", future = true)]
    pub async fn generation_snapshot_for(
        &self,
        generation_id: &VectorGenerationIdV1,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<VerifiedGraphVectorGenerationSnapshotV1>, VectorGenerationStoreErrorV1> {
        let snapshot = self.snapshot()?;
        let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
        drop(snapshot);
        let Some(records) =
            self.read_cataloged_hydrating_published_bases(generation_id, cancellation)?
        else {
            return Ok(None);
        };
        let generation = records.generation;
        if generation.embedding_key() != embedding_key
            || generation.source_generation() != source_generation
            || generation.source_manifest_digest() != source_manifest_digest
        {
            return Ok(None);
        }
        Ok(Some(VerifiedGraphVectorGenerationSnapshotV1 {
            revision: metadata.revision,
            generation,
        }))
    }

    pub async fn staged_checkpoint(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<VectorProjectionCheckpointV1>, VectorGenerationStoreErrorV1> {
        let Some(snapshot) = self.optional_snapshot()? else {
            return Ok(None);
        };
        read_build_records(&snapshot, build_id, cancellation)
            .map(|records| records.map(|records| records.staged.checkpoint))
    }

    pub async fn generation(
        &self,
        generation_id: &VectorGenerationIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<super::PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        if self.optional_snapshot()?.is_none() {
            return Ok(None);
        }
        self.read_cataloged_hydrating_published_bases(generation_id, cancellation)
            .map(|records| records.map(|records| records.generation))
    }

    /// Catalog/owner visibility only — does not hydrate resident vectors.
    pub async fn published_generation_is_visible(
        &self,
        generation_id: &VectorGenerationIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        let Some(snapshot) = self.optional_snapshot()? else {
            return Ok(false);
        };
        Ok(read_generation_catalog_entry(&snapshot, generation_id, cancellation)?.is_some())
    }

    pub fn verified_revision(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<u64, VectorGenerationStoreErrorV1> {
        read_state_metadata(&self.snapshot()?, cancellation).map(|metadata| metadata.revision)
    }

    #[hotpath::measure(label = "usecases.store.resident_plan", future = true)]
    pub async fn verified_resident_plan(
        &self,
        expected_generation: &VectorGenerationIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<VerifiedVectorResidentPlanV1>, VectorGenerationStoreErrorV1> {
        check_cancelled(cancellation.as_ref())?;
        let snapshot = self.snapshot()?;
        let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
        let generation =
            read_generation_metadata(&snapshot, expected_generation, Arc::clone(&cancellation))?
                .ok_or_else(|| {
                    VectorGenerationStoreErrorV1::Corrupt(
                        "active semantic vector generation metadata is missing".to_owned(),
                    )
                })?;
        let catalog = read_generation_catalog_entry(
            &snapshot,
            expected_generation,
            Arc::clone(&cancellation),
        )?
        .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration(
            BaseGenerationIncompatibilityV1::MissingSnapshot,
        ))?;
        if &catalog.generation_id != expected_generation {
            return Err(VectorGenerationStoreErrorV1::Corrupt(
                "active semantic vector generation catalog identity is inconsistent".to_owned(),
            ));
        }
        let row_count = catalog.rows;
        let dimensions = u64::from(generation.embedding_key.embedding_key().dimensions);
        let vector_bytes = dimensions
            .checked_mul(u64::try_from(size_of::<f32>()).map_err(storage_error)?)
            .ok_or_else(resident_size_overflow)?;
        let per_row = u64::try_from(size_of::<ResidentVectorRowV1>())
            .map_err(storage_error)?
            .checked_add(1_024)
            .and_then(|bytes| bytes.checked_add(vector_bytes))
            .ok_or_else(resident_size_overflow)?;
        let retained_bytes = row_count
            .checked_mul(per_row)
            .ok_or_else(resident_size_overflow)?;
        let hydration_peak_bytes = retained_bytes
            .checked_mul(2)
            .and_then(|bytes| {
                row_count
                    .checked_mul(4_096)
                    .and_then(|overhead| bytes.checked_add(overhead))
            })
            .ok_or_else(resident_size_overflow)?;
        drop(snapshot);
        check_cancelled(cancellation.as_ref())?;
        crate::hotpath_observe::vector_resident_reservation(retained_bytes, hydration_peak_bytes);
        Ok(Some(VerifiedVectorResidentPlanV1 {
            watermark: metadata.watermark,
            generation_id: expected_generation.clone(),
            retained_bytes,
            hydration_peak_bytes,
        }))
    }

    /// The persisted ANN index bound to one published generation, if the
    /// store holds a populated one.
    ///
    /// `serving_chunks` is the caller's complete serving row set; it maps
    /// index hits back to chunk identities. `Ok(None)` is the typed absence:
    /// no index was ever built for this generation's vector property, or it
    /// reopened empty. Coverage against the serving row count is the
    /// caller's check via [`SemanticAnnServingIndexV1::indexed`], because the
    /// index covers only this generation's own staged vectors — never rows
    /// reused from base generations.
    pub fn ann_serving_index<'a>(
        &self,
        generation_id: &VectorGenerationIdV1,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        serving_chunks: impl IntoIterator<Item = &'a CodeSearchChunkId>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<SemanticAnnServingIndexV1>, VectorGenerationStoreErrorV1> {
        check_cancelled(cancellation.as_ref())?;
        let Some(snapshot) = self.optional_snapshot()? else {
            return Ok(None);
        };
        let identity = snapshot.projection().clone();
        let property = search_vector_property(generation_id)?;
        let dimension =
            usize::try_from(embedding_key.embedding_key().dimensions).map_err(storage_error)?;
        let metric = vector_metric(embedding_key.embedding_key().metric);
        let status = snapshot
            .vector_index_status(GraphVectorIndexRequest {
                namespace: identity.namespace.clone(),
                projection: identity.projection.clone(),
                property: property.clone(),
                dimension,
                metric,
                cancellation: Arc::clone(&cancellation),
            })
            .map_err(map_graph_error)?;
        let GraphVectorIndexStatus::Available { vectors } = status else {
            return Ok(None);
        };
        let mut chunks_by_entity = BTreeMap::new();
        for chunk_id in serving_chunks {
            check_cancelled(cancellation.as_ref())?;
            let entity = tracedecay_graph_db::semantic_vector_native::generation_vector_entity_id(
                generation_id.as_digest().as_str(),
                &chunk_id.to_string(),
            )
            .map_err(map_graph_error)?;
            chunks_by_entity.insert(entity, chunk_id.clone());
        }
        Ok(Some(SemanticAnnServingIndexV1 {
            snapshot,
            namespace: identity.namespace,
            projection: identity.projection,
            property,
            dimension,
            metric,
            chunks_by_entity,
            indexed: u64::try_from(vectors).map_err(storage_error)?,
            cancellation,
        }))
    }
}
