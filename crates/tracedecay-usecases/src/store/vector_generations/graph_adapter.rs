use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, ChangedCodeChunkSetV1, CodeGenerationId, CodeSearchChunkId,
    ManifestDigest, ProjectionKeyV1, VectorGenerationIdV1,
};
use tracedecay_graph_db::{GraphCancellation, GraphProperty, GraphWatermark};
use tracedecay_store::{
    GraphNamespaceV1, GraphProjectionIdV1, GraphProjectionIdentityV1, SemanticVectorChunkDigest,
    SemanticVectorChunkId, SemanticVectorChunkManifestMember, SemanticVectorPublishedGenerationKey,
    SemanticVectorPublishedGenerationLookup, SemanticVectorStageChunkOperation,
    SemanticVectorStageRecord,
};

use crate::semantic_runtime::{
    RetainedSemanticVectorGraphV1, SemanticGraphExecutionAuthorityV1,
    VerifiedSemanticVectorGraphRuntimeV1,
};

use super::{
    PreparedVectorGenerationV1, VectorGenerationBuildIdV1, VectorGenerationPlanV1,
    VectorGenerationPublicationV1, VectorGenerationStateMachineV1, VectorGenerationStoreErrorV1,
    VectorProjectionCheckpointV1,
};

mod evaluation_runtime;
mod native_records;
mod persistence;
mod retention;
mod search;
mod snapshot;
mod stage_identity;
pub(super) mod transitions;

use native_records::{
    read_build_records, read_cataloged_generation_records, read_generation_catalog,
    read_generation_metadata, read_state_metadata,
};
use persistence::{
    check_cancelled, generation_label, map_graph_error, measured_resident_bytes, required_string,
    resident_size_overflow, search_vector_property, storage_error, vector_metric,
};
use snapshot::SemanticVectorVerifiedReadV1;

pub use evaluation_runtime::{
    IsolatedSemanticEvaluationGraphV1, isolated_semantic_evaluation_graph,
};

pub const SEMANTIC_VECTOR_GRAPH_PROJECTION: &str = "tracedecay.semantic-vector.graph";
pub const MAX_SEMANTIC_VECTOR_SEARCH_RESULTS: usize = 1_024;
pub const MAX_SEMANTIC_HYBRID_LEXICAL_CANDIDATES: usize = 4_096;
const CHUNK_ID_PROPERTY: &str = "chunk_id";
const GENERATION_ID_PROPERTY: &str = "generation_id";
const GRAPH_OPERATION_DEADLINE: Duration = Duration::from_secs(30);
pub(super) const MAX_RESIDENT_VECTOR_ROWS: usize = 100_000;

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

#[derive(Clone)]
pub struct SemanticVectorStageDescriptorV1 {
    projection: AdmittedEmbeddingProjectionKeyV1,
    members: Vec<SemanticVectorChunkManifestMember>,
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
        let mut members = changes
            .added_or_changed
            .iter()
            .chain(&changes.reused)
            .map(|change| {
                let digest = change.current_digest.as_ref().ok_or_else(|| {
                    VectorGenerationStoreErrorV1::InvalidPlan(
                        "semantic vector live member has no current digest".to_owned(),
                    )
                })?;
                Ok(SemanticVectorChunkManifestMember {
                    chunk_id: SemanticVectorChunkId::new(change.chunk_id.to_string())
                        .map_err(storage_error)?,
                    chunk_digest: SemanticVectorChunkDigest::new(digest.as_str())
                        .map_err(storage_error)?,
                    operation: SemanticVectorStageChunkOperation::Embed,
                })
            })
            .chain(changes.deleted.iter().map(|change| {
                let digest = change.prior_digest.as_ref().ok_or_else(|| {
                    VectorGenerationStoreErrorV1::InvalidPlan(
                        "semantic vector tombstone has no prior digest".to_owned(),
                    )
                })?;
                Ok(SemanticVectorChunkManifestMember {
                    chunk_id: SemanticVectorChunkId::new(change.chunk_id.to_string())
                        .map_err(storage_error)?,
                    chunk_digest: SemanticVectorChunkDigest::new(digest.as_str())
                        .map_err(storage_error)?,
                    operation: SemanticVectorStageChunkOperation::Tombstone,
                })
            }))
            .collect::<Result<Vec<_>, VectorGenerationStoreErrorV1>>()?;
        members.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
        tracedecay_store::semantic_vector_chunk_manifest_digest(&members).map_err(storage_error)?;
        Ok(Self {
            projection,
            members,
        })
    }
}

pub struct SemanticVectorGraphSearchRequestV1 {
    pub generation_id: VectorGenerationIdV1,
    pub embedding_key: AdmittedEmbeddingProjectionKeyV1,
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub query: Vec<f32>,
    pub limit: usize,
    pub cancellation: Arc<dyn GraphCancellation>,
    pub deadline: Instant,
}

struct DeadlineGraphCancellationV1 {
    request: Arc<dyn GraphCancellation>,
    deadline: Instant,
}

impl GraphCancellation for DeadlineGraphCancellationV1 {
    fn is_cancelled(&self) -> bool {
        self.request.is_cancelled() || Instant::now() >= self.deadline
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticVectorGraphMatchV1 {
    pub chunk_id: CodeSearchChunkId,
    pub distance: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticVectorGraphSearchResultV1 {
    pub generation_id: VectorGenerationIdV1,
    pub matches: Vec<SemanticVectorGraphMatchV1>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticHybridLexicalCandidateV1 {
    pub chunk_id: CodeSearchChunkId,
    pub score: f64,
}

pub struct SemanticHybridGraphSearchRequestV1 {
    pub vector: SemanticVectorGraphSearchRequestV1,
    pub lexical: Vec<SemanticHybridLexicalCandidateV1>,
    pub vector_weight: f64,
    pub lexical_weight: f64,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticHybridGraphMatchV1 {
    pub chunk_id: CodeSearchChunkId,
    pub vector_distance: Option<f64>,
    pub lexical_score: Option<f64>,
    pub combined_score: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticHybridGraphSearchResultV1 {
    pub generation_id: VectorGenerationIdV1,
    pub matches: Vec<SemanticHybridGraphMatchV1>,
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

pub struct ResidentVectorGenerationV1 {
    pub generation_id: VectorGenerationIdV1,
    pub projection_key: ProjectionKeyV1,
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub rows: Vec<ResidentVectorRowV1>,
    pub retained_bytes: u64,
}

pub struct ResidentVectorRowV1 {
    pub chunk_id: CodeSearchChunkId,
    pub values: Box<[f32]>,
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
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
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
            Some(existing)
                if existing.projection != descriptor.projection
                    || existing.members != descriptor.members =>
            {
                Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
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
        read_cataloged_generation_records(
            &snapshot,
            &catalog[0].generation_id,
            Arc::clone(&cancellation),
        )?
        .ok_or_else(|| {
            VectorGenerationStoreErrorV1::Corrupt(
                "verified semantic vector generation records are missing".to_owned(),
            )
        })?;
        drop(snapshot);
        check_cancelled(cancellation.as_ref())?;
        Ok(())
    }

    pub async fn begin_generation(
        &self,
        plan: VectorGenerationPlanV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationBeginOutcomeV1, VectorGenerationStoreErrorV1> {
        self.begin_generation_records(plan, false, cancellation)
    }

    pub async fn rebuild_generation(
        &self,
        plan: VectorGenerationPlanV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationBeginOutcomeV1, VectorGenerationStoreErrorV1> {
        self.begin_generation_records(plan, true, cancellation)
    }

    pub async fn cancel_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        self.cancel_generation_records(build_id, cancellation)
    }

    pub async fn commit_batch(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        self.commit_batch_records(build_id, expected_checkpoint, prepared, cancellation)
    }

    pub async fn publish_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        self.publish_generation_records(build_id, cancellation)
    }

    /// Read one exact semantic generation from an already identity-selected
    /// verified physical snapshot.
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
        let Some(records) =
            read_cataloged_generation_records(&snapshot, generation_id, cancellation)?
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
        let Some(snapshot) = self.optional_snapshot()? else {
            return Ok(None);
        };
        read_cataloged_generation_records(&snapshot, generation_id, cancellation)
            .map(|records| records.map(|records| records.generation))
    }

    pub fn verified_revision(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<u64, VectorGenerationStoreErrorV1> {
        read_state_metadata(&self.snapshot()?, cancellation).map(|metadata| metadata.revision)
    }

    fn generation_entities(
        &self,
        snapshot: &SemanticVectorVerifiedReadV1,
        generation_id: &VectorGenerationIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<tracedecay_graph_db::GraphEntity>, VectorGenerationStoreErrorV1> {
        let label = generation_label(generation_id)?;
        let records = read_cataloged_generation_records(snapshot, generation_id, cancellation)?
            .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
        Ok(records
            .entities
            .into_values()
            .filter(|entity| entity.labels.contains(&label))
            .collect())
    }

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
        let rows =
            self.generation_entities(&snapshot, expected_generation, Arc::clone(&cancellation))?;
        let row_count = u64::try_from(rows.len()).map_err(storage_error)?;
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
        Ok(Some(VerifiedVectorResidentPlanV1 {
            watermark: metadata.watermark,
            generation_id: expected_generation.clone(),
            retained_bytes,
            hydration_peak_bytes,
        }))
    }

    pub async fn read_resident_generation_for(
        &self,
        plan: &VerifiedVectorResidentPlanV1,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<ResidentVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        check_cancelled(cancellation.as_ref())?;
        let snapshot = self.snapshot()?;
        let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
        if metadata.watermark != plan.watermark {
            return Ok(None);
        }
        let generation_id = &plan.generation_id;
        let Some(generation) =
            read_generation_metadata(&snapshot, generation_id, Arc::clone(&cancellation))?
        else {
            return Ok(None);
        };
        if &generation.embedding_key != embedding_key
            || &generation.source_generation != source_generation
            || &generation.source_manifest_digest != source_manifest_digest
        {
            return Ok(None);
        }
        let entities =
            self.generation_entities(&snapshot, generation_id, Arc::clone(&cancellation))?;
        let expected_dimension =
            usize::try_from(embedding_key.embedding_key().dimensions).map_err(storage_error)?;
        let expected_metric = vector_metric(embedding_key.embedding_key().metric);
        let vector_property = search_vector_property(generation_id)?;
        let mut rows = Vec::with_capacity(entities.len());
        for entity in entities {
            check_cancelled(cancellation.as_ref())?;
            if required_string(&entity, GENERATION_ID_PROPERTY)?
                != generation_id.as_digest().as_str()
            {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "resident vector row names a foreign generation".to_owned(),
                ));
            }
            let vector = match entity.properties.get(&vector_property) {
                Some(GraphProperty::Vector(vector)) => vector,
                Some(_) => {
                    return Err(VectorGenerationStoreErrorV1::Corrupt(
                        "resident vector property has the wrong type".to_owned(),
                    ));
                }
                None => {
                    return Err(VectorGenerationStoreErrorV1::Corrupt(
                        "resident vector property is missing".to_owned(),
                    ));
                }
            };
            if vector.dimension != expected_dimension || vector.metric != expected_metric {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "resident vector shape does not match its projection".to_owned(),
                ));
            }
            rows.push(ResidentVectorRowV1 {
                chunk_id: CodeSearchChunkId::try_from(
                    required_string(&entity, CHUNK_ID_PROPERTY)?.to_owned(),
                )
                .map_err(storage_error)?,
                values: vector.values.clone().into_boxed_slice(),
            });
        }
        rows.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
        if rows
            .windows(2)
            .any(|pair| pair[0].chunk_id == pair[1].chunk_id)
        {
            return Err(VectorGenerationStoreErrorV1::Corrupt(
                "resident vector generation contains duplicate chunks".to_owned(),
            ));
        }
        let retained_bytes = measured_resident_bytes(&rows)?;
        drop(snapshot);
        check_cancelled(cancellation.as_ref())?;
        Ok(Some(ResidentVectorGenerationV1 {
            generation_id: generation_id.clone(),
            projection_key: generation.projection_key,
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            rows,
            retained_bytes,
        }))
    }
}
