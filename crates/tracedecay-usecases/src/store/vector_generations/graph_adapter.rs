use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeGenerationId,
    CodeSearchChunkId, ManifestDigest, VectorGenerationIdV1,
};
use tracedecay_graph_db::{GraphCancellation, GraphWatermark};
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
use persistence::{check_cancelled, map_graph_error, resident_size_overflow, storage_error};
use snapshot::SemanticVectorVerifiedReadV1;

pub use evaluation_runtime::{
    IsolatedSemanticEvaluationGraphV1, isolated_semantic_evaluation_graph,
};

pub const SEMANTIC_VECTOR_GRAPH_PROJECTION: &str = "tracedecay.semantic-vector.graph";
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
        let live_member = |change: &ChangedCodeChunkV1,
                           operation: SemanticVectorStageChunkOperation| {
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
                operation,
            })
        };
        let mut members = changes
            .added_or_changed
            .iter()
            .map(|change| live_member(change, SemanticVectorStageChunkOperation::Embed))
            .chain(
                changes
                    .reused
                    .iter()
                    .map(|change| live_member(change, SemanticVectorStageChunkOperation::Reuse)),
            )
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
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        Ok(Some(SemanticVectorVerifiedReadV1::new(snapshot)))
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
        Ok(Some(VerifiedVectorResidentPlanV1 {
            watermark: metadata.watermark,
            generation_id: expected_generation.clone(),
            retained_bytes,
            hydration_peak_bytes,
        }))
    }
}
