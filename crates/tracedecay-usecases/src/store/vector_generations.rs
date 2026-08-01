//! Immutable semantic vector-generation storage.
//!
//! The deterministic state machine is retained as a test oracle. Production
//! persistence stores that same state in the already-open project database,
//! using a revisioned compare-and-swap so generation publication and the
//! active pointer become visible together. No separate vector database or
//! approximate index is introduced.
#![allow(dead_code)] // Plan 25/31 semantic vector storage — test oracle + staged persistence
#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeGenerationId, CodeSearchChunkId, ContentDigest,
    ManifestDigest, ProjectionBatchReceiptV1, ProjectionKeyV1, ProjectionKindV1,
    ProjectionOperationV1, ProjectionOutcomeV1, canonical_sha256,
};

pub use tracedecay_domain::VectorGenerationIdV1;

use tracedecay_code_index::projection::{expected_publication_digest, verify_batch_receipt};
use tracedecay_runtime_core::db::{Database, engine::params};
use tracedecay_semantic::legacy_migration::{
    LegacyVectorInventoryEntryV1, LegacyVectorInventoryPortV1, LegacyVectorInventoryV1,
    LegacyVectorMigrationOutcomeKindV1, LegacyVectorMigrationOwnerTransactionV1,
    LegacyVectorMigrationReceiptV1, canonical_chunk_set_digest,
};
use tracedecay_semantic::projector::{
    PreparedVectorGenerationV1, ProjectedChunkVectorV1, SemanticProjectionErrorV1,
};

const VECTOR_GENERATION_BUILD_DIGEST_DOMAIN: &str = "tracedecay.vector-generation-build.v1";
const VECTOR_GENERATION_MANIFEST_DIGEST_DOMAIN: &str = "tracedecay.vector-generation-manifest.v1";
const PHYSICAL_VECTOR_REUSE_DIGEST_DOMAIN: &str = "tracedecay.physical-vector-reuse.v1";
const VECTOR_GENERATION_STATE_OPERATION: &str = "persist semantic vector generations";
const VECTOR_GENERATION_STATE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_vector_generation_state_v1 (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    state_json TEXT NOT NULL
) STRICT;
";
const VECTOR_EVALUATION_STATE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_vector_evaluation_state_v1 (
    evaluation_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    state_json TEXT NOT NULL
) STRICT;
";
const LEGACY_VECTOR_QUARANTINE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_legacy_vector_quarantine_v1 (
    receipt_digest TEXT NOT NULL,
    legacy_generation TEXT NOT NULL,
    reason_digest TEXT NOT NULL,
    generation_json TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    PRIMARY KEY (receipt_digest, legacy_generation)
) STRICT;
";
const LEGACY_VECTOR_UNREADABLE_REASON_DOMAIN_V1: &str =
    "tracedecay.semantic-code.legacy-vector-unreadable-reason.v1";
const MAX_STATE_CAS_RETRIES: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct VectorGenerationBuildIdV1(ManifestDigest);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VectorGenerationPlanV1 {
    pub target_projection_key: ProjectionKeyV1,
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub expected_chunk_ids: Vec<CodeSearchChunkId>,
    pub base_generation: Option<VectorGenerationIdV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VectorProjectionCheckpointV1 {
    pub target_projection_key: ProjectionKeyV1,
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub completed_batches: u64,
    pub last_request_digest: Option<ManifestDigest>,
    pub last_publication_digest: Option<ManifestDigest>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct PhysicalVectorReuseKeyV1 {
    canonical_chunk_digest: ContentDigest,
    projection_key: ProjectionKeyV1,
    admitted_embedding_key: AdmittedEmbeddingProjectionKeyV1,
    privacy_domain: tracedecay_domain::PrivacyDomainId,
    privacy_key_epoch: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct SharedVectorBytesV1(Arc<[f32]>);

impl Serialize for SharedVectorBytesV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.as_ref().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SharedVectorBytesV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Vec::<f32>::deserialize(deserializer).map(|values| Self(Arc::from(values)))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct PhysicalVectorPayloadV1 {
    reuse_key: PhysicalVectorReuseKeyV1,
    values: SharedVectorBytesV1,
}

type PhysicalVectorPoolMapV1 = BTreeMap<PhysicalVectorReuseKeyV1, Weak<[f32]>>;

/// Process-wide physical byte interner. Complete projection and privacy
/// authority is part of the key, so sharing cannot cross either boundary.
#[derive(Clone)]
pub struct PhysicalVectorBytePoolV1 {
    entries: Arc<Mutex<PhysicalVectorPoolMapV1>>,
}

impl Default for PhysicalVectorBytePoolV1 {
    fn default() -> Self {
        static ENTRIES: std::sync::OnceLock<Arc<Mutex<PhysicalVectorPoolMapV1>>> =
            std::sync::OnceLock::new();
        Self {
            entries: Arc::clone(ENTRIES.get_or_init(|| Arc::new(Mutex::new(BTreeMap::new())))),
        }
    }
}

impl PhysicalVectorBytePoolV1 {
    fn intern(
        &self,
        reuse_key: &PhysicalVectorReuseKeyV1,
        values: &[f32],
    ) -> Result<Arc<[f32]>, VectorGenerationStoreErrorV1> {
        let mut entries = self.entries.lock().map_err(|_| {
            VectorGenerationStoreErrorV1::Storage(
                "physical vector byte pool lock is poisoned".to_string(),
            )
        })?;
        if let Some(shared) = entries.get(reuse_key).and_then(Weak::upgrade) {
            if shared.as_ref() != values {
                return Err(VectorGenerationStoreErrorV1::PhysicalVectorConflict);
            }
            return Ok(shared);
        }
        let shared: Arc<[f32]> = Arc::from(values.to_vec());
        entries.insert(reuse_key.clone(), Arc::downgrade(&shared));
        Ok(shared)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PublishedVectorGenerationV1 {
    generation_id: VectorGenerationIdV1,
    projection_key: ProjectionKeyV1,
    source_generation: CodeGenerationId,
    source_manifest_digest: ManifestDigest,
    base_generation: Option<VectorGenerationIdV1>,
    embedding_key: AdmittedEmbeddingProjectionKeyV1,
    vectors: BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>,
    tombstones: Vec<CodeSearchChunkId>,
    tombstone_digests: BTreeMap<CodeSearchChunkId, ContentDigest>,
    receipts: Vec<ProjectionBatchReceiptV1>,
    checkpoint: VectorProjectionCheckpointV1,
    manifest_digest: ManifestDigest,
}

impl PublishedVectorGenerationV1 {
    pub fn generation_id(&self) -> &VectorGenerationIdV1 {
        &self.generation_id
    }

    pub fn projection_key(&self) -> &ProjectionKeyV1 {
        &self.projection_key
    }

    pub fn source_generation(&self) -> &CodeGenerationId {
        &self.source_generation
    }

    pub fn source_manifest_digest(&self) -> &ManifestDigest {
        &self.source_manifest_digest
    }

    pub fn base_generation(&self) -> Option<&VectorGenerationIdV1> {
        self.base_generation.as_ref()
    }

    pub fn embedding_key(&self) -> &AdmittedEmbeddingProjectionKeyV1 {
        &self.embedding_key
    }

    pub fn vectors(&self) -> &BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1> {
        &self.vectors
    }

    pub fn tombstones(&self) -> &[CodeSearchChunkId] {
        &self.tombstones
    }

    pub fn tombstone_digests(&self) -> &BTreeMap<CodeSearchChunkId, ContentDigest> {
        &self.tombstone_digests
    }

    pub fn receipts(&self) -> &[ProjectionBatchReceiptV1] {
        &self.receipts
    }

    pub fn checkpoint(&self) -> &VectorProjectionCheckpointV1 {
        &self.checkpoint
    }

    pub fn manifest_digest(&self) -> &ManifestDigest {
        &self.manifest_digest
    }

    fn same_vector_content(&self, other: &Self) -> bool {
        self.projection_key == other.projection_key
            && self.source_generation == other.source_generation
            && self.source_manifest_digest == other.source_manifest_digest
            && self.embedding_key == other.embedding_key
            && self.vectors == other.vectors
            && self.tombstones == other.tombstones
            && self.tombstone_digests == other.tombstone_digests
            && self.manifest_digest == other.manifest_digest
    }

    fn canonicalize_tombstones(&mut self) {
        self.tombstones = self.tombstone_digests.keys().cloned().collect();
    }

    fn validate_persisted(&self) -> Result<(), VectorGenerationStoreErrorV1> {
        if self.generation_id.as_digest() != &self.manifest_digest
            || generation_identity_digest(
                &VectorGenerationPlanV1 {
                    target_projection_key: self.projection_key.clone(),
                    source_generation: self.source_generation.clone(),
                    source_manifest_digest: self.source_manifest_digest.clone(),
                    expected_chunk_ids: self.vectors.keys().cloned().collect(),
                    base_generation: self.base_generation.clone(),
                },
                &self.vectors,
                &self.tombstone_digests,
            )
            .map_err(|error| VectorGenerationStoreErrorV1::Storage(error.to_string()))?
                != self.manifest_digest
        {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "published generation id does not match manifest digest".to_string(),
            ));
        }
        if self.embedding_key.projection_key() != &self.projection_key {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "published embedding key does not match projection key".to_string(),
            ));
        }
        let canonical_tombstones = self.tombstone_digests.keys().cloned().collect::<Vec<_>>();
        if self.tombstones != canonical_tombstones {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "published tombstone list is not the canonical digest-map order".to_string(),
            ));
        }
        for vector in self.vectors.values() {
            validate_vector_row_for_published(self, vector)?;
        }
        for chunk_id in self.tombstone_digests.keys() {
            if self.vectors.contains_key(chunk_id) {
                return Err(VectorGenerationStoreErrorV1::Storage(format!(
                    "published generation retains both vector and tombstone for {chunk_id}"
                )));
            }
        }
        validate_published_receipts(self)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VectorGenerationPublicationV1 {
    pub generation_id: VectorGenerationIdV1,
    pub manifest_digest: ManifestDigest,
    pub checkpoint: VectorProjectionCheckpointV1,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum VectorGenerationStoreErrorV1 {
    #[error("vector generation plan is invalid: {0}")]
    InvalidPlan(String),
    #[error("unknown vector generation build")]
    UnknownBuild,
    #[error("the supplied checkpoint is stale")]
    StaleCheckpoint,
    #[error("projection batch does not match its vector generation plan")]
    BatchIdentityMismatch,
    #[error("projection batch was replayed with conflicting content")]
    ConflictingBatchReplay,
    #[error("chunk {0} appears in more than one committed batch")]
    DuplicateChunkEffect(CodeSearchChunkId),
    #[error("base vector generation is missing or incompatible")]
    IncompatibleBaseGeneration,
    #[error("reused chunk {0} has no matching immutable base vector")]
    MissingBaseVector(CodeSearchChunkId),
    #[error("applied chunk {0} has no matching vector output")]
    MissingAppliedVector(CodeSearchChunkId),
    #[error("vector generation membership is incomplete")]
    IncompleteGeneration,
    #[error("active vector generation changed before publication")]
    StaleActiveGeneration,
    #[error("immutable vector generation identity already has different content")]
    ImmutableGenerationConflict,
    #[error("physical vector reuse identity already has different bytes")]
    PhysicalVectorConflict,
    #[error("injected failure before atomic publication swap")]
    InjectedPublicationFailure,
    #[error("legacy vector migration failed: {0}")]
    LegacyMigration(String),
    #[error("project vector generation storage failed: {0}")]
    Storage(String),
    #[error("project vector generation state changed repeatedly during compare-and-swap")]
    ConcurrentMutation,
    #[error("semantic projector handoff rejected: {0}")]
    Projection(#[from] SemanticProjectionErrorV1),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StagedVectorGenerationV1 {
    plan: VectorGenerationPlanV1,
    embedding_key: Option<AdmittedEmbeddingProjectionKeyV1>,
    vectors: BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>,
    tombstones: BTreeMap<CodeSearchChunkId, ContentDigest>,
    batches: Vec<PreparedVectorGenerationV1>,
    committed_chunk_effects: BTreeSet<CodeSearchChunkId>,
    checkpoint: VectorProjectionCheckpointV1,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishedStateV1 {
    generations: BTreeMap<VectorGenerationIdV1, PublishedVectorGenerationV1>,
    active_generation: Option<VectorGenerationIdV1>,
    #[serde(default)]
    legacy_migration_receipts: BTreeMap<ManifestDigest, LegacyVectorMigrationReceiptV1>,
    #[serde(skip, default)]
    physical_vectors: BTreeMap<ManifestDigest, PhysicalVectorPayloadV1>,
    #[serde(default)]
    physical_vector_bindings:
        BTreeMap<VectorGenerationIdV1, BTreeMap<CodeSearchChunkId, ManifestDigest>>,
}

/// Deterministic state machine used directly by focused tests and persisted by
/// [`DatabaseVectorGenerationStoreV1`].
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FakeVectorGenerationStoreV1 {
    staged: BTreeMap<VectorGenerationBuildIdV1, StagedVectorGenerationV1>,
    published: PublishedStateV1,
    #[serde(skip, default)]
    physical_vector_pool: PhysicalVectorBytePoolV1,
    #[serde(default, skip)]
    fail_before_publication_swap: bool,
}

impl FakeVectorGenerationStoreV1 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin_generation(
        &mut self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        validate_plan(&plan)?;
        if let Some(base_id) = &plan.base_generation {
            self.published
                .generations
                .get(base_id)
                .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
        }
        let digest = canonical_sha256(&(VECTOR_GENERATION_BUILD_DIGEST_DOMAIN, &plan))
            .map_err(|error| VectorGenerationStoreErrorV1::InvalidPlan(error.to_string()))?;
        let build_id = VectorGenerationBuildIdV1(digest);
        if let Some(existing) = self.staged.get(&build_id) {
            if existing.plan == plan {
                return Ok(build_id);
            }
            return Err(VectorGenerationStoreErrorV1::InvalidPlan(
                "build identity collision".to_string(),
            ));
        }
        let checkpoint = VectorProjectionCheckpointV1 {
            target_projection_key: plan.target_projection_key.clone(),
            source_generation: plan.source_generation.clone(),
            source_manifest_digest: plan.source_manifest_digest.clone(),
            completed_batches: 0,
            last_request_digest: None,
            last_publication_digest: None,
        };
        self.staged.insert(
            build_id.clone(),
            StagedVectorGenerationV1 {
                plan,
                embedding_key: None,
                vectors: BTreeMap::new(),
                tombstones: BTreeMap::new(),
                batches: Vec::new(),
                committed_chunk_effects: BTreeSet::new(),
                checkpoint,
            },
        );
        Ok(build_id)
    }

    /// Discard any checkpointed execution for the same deterministic build
    /// identity and restart projection from its authoritative query inputs.
    /// Already-published generations and the active pointer are untouched.
    pub fn rebuild_generation(
        &mut self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        let build_id = self.begin_generation(plan.clone())?;
        let checkpoint = VectorProjectionCheckpointV1 {
            target_projection_key: plan.target_projection_key.clone(),
            source_generation: plan.source_generation.clone(),
            source_manifest_digest: plan.source_manifest_digest.clone(),
            completed_batches: 0,
            last_request_digest: None,
            last_publication_digest: None,
        };
        self.staged.insert(
            build_id.clone(),
            StagedVectorGenerationV1 {
                plan,
                embedding_key: None,
                vectors: BTreeMap::new(),
                tombstones: BTreeMap::new(),
                batches: Vec::new(),
                committed_chunk_effects: BTreeSet::new(),
                checkpoint,
            },
        );
        Ok(build_id)
    }

    /// Discard one unpublished build without changing any immutable
    /// generation or the active pointer. This is the cancellation boundary
    /// for asynchronous projection work.
    pub fn cancel_generation(&mut self, build_id: &VectorGenerationBuildIdV1) -> bool {
        self.staged.remove(build_id).is_some()
    }

    /// Atomically commit one batch's vector effects, tombstones, Plan 25
    /// receipt, and next checkpoint. Any validation failure leaves the prior
    /// staged state and checkpoint unchanged.
    pub fn commit_batch(
        &mut self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        let current = self
            .staged
            .get(build_id)
            .cloned()
            .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
        if let Some(existing) = current
            .batches
            .iter()
            .find(|batch| batch.request.request_digest == prepared.request.request_digest)
        {
            if existing == &prepared {
                return Ok(current.checkpoint);
            }
            return Err(VectorGenerationStoreErrorV1::ConflictingBatchReplay);
        }
        if current.checkpoint.completed_batches == 0 {
            if expected_checkpoint.is_some() {
                return Err(VectorGenerationStoreErrorV1::StaleCheckpoint);
            }
        } else if expected_checkpoint != Some(&current.checkpoint) {
            return Err(VectorGenerationStoreErrorV1::StaleCheckpoint);
        }

        validate_batch_identity(&current.plan, &prepared)?;
        validate_base_generation_for_batch(&self.published, &current.plan, &prepared)?;
        verify_batch_receipt(&prepared.request, &prepared.receipt)
            .map_err(SemanticProjectionErrorV1::from)?;
        let mut next = current;
        if let Some(key) = &next.embedding_key {
            if key != &prepared.embedding_key {
                return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
            }
        } else {
            next.embedding_key = Some(prepared.embedding_key.clone());
        }

        let vector_by_chunk = prepared
            .vectors
            .iter()
            .map(|vector| (vector.chunk_id.clone(), vector))
            .collect::<BTreeMap<_, _>>();
        let tombstone_by_chunk = prepared
            .tombstones
            .iter()
            .map(|tombstone| (tombstone.chunk_id.clone(), tombstone))
            .collect::<BTreeMap<_, _>>();
        if vector_by_chunk.len() != prepared.vectors.len()
            || tombstone_by_chunk.len() != prepared.tombstones.len()
        {
            return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
        }

        for receipt in &prepared.receipt.receipts {
            if !next
                .committed_chunk_effects
                .insert(receipt.chunk_id.clone())
            {
                return Err(VectorGenerationStoreErrorV1::DuplicateChunkEffect(
                    receipt.chunk_id.clone(),
                ));
            }
            match receipt.operation {
                ProjectionOperationV1::Added | ProjectionOperationV1::Updated => {
                    let vector = vector_by_chunk.get(&receipt.chunk_id).ok_or_else(|| {
                        VectorGenerationStoreErrorV1::MissingAppliedVector(receipt.chunk_id.clone())
                    })?;
                    validate_prepared_vector_row(&prepared, vector)?;
                    if receipt.outcome != ProjectionOutcomeV1::Applied
                        || receipt.output_digest.as_ref() != Some(&vector.output_digest)
                    {
                        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
                    }
                    next.tombstones.remove(&receipt.chunk_id);
                    let mut rebound = (*vector).clone();
                    rebound.source_manifest_digest = next.plan.source_manifest_digest.clone();
                    next.vectors.insert(receipt.chunk_id.clone(), rebound);
                }
                ProjectionOperationV1::Deleted => {
                    let tombstone = tombstone_by_chunk
                        .get(&receipt.chunk_id)
                        .ok_or(VectorGenerationStoreErrorV1::BatchIdentityMismatch)?;
                    if receipt.prior_chunk_digest.as_ref() != Some(&tombstone.prior_chunk_digest) {
                        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
                    }
                    validate_base_digest(&self.published, &next.plan, receipt)?;
                    next.vectors.remove(&receipt.chunk_id);
                    next.tombstones.insert(
                        receipt.chunk_id.clone(),
                        tombstone.prior_chunk_digest.clone(),
                    );
                }
                ProjectionOperationV1::Reused => {
                    let base = base_vector(&self.published, &next.plan, &receipt.chunk_id)?;
                    if next.plan.target_projection_key != base.projection_key
                        || receipt.prior_chunk_digest.as_ref() != Some(&base.chunk_digest)
                        || receipt.current_chunk_digest.as_ref() != Some(&base.chunk_digest)
                    {
                        return Err(VectorGenerationStoreErrorV1::MissingBaseVector(
                            receipt.chunk_id.clone(),
                        ));
                    }
                    let mut rebound = base.clone();
                    rebound.source_generation = next.plan.source_generation.clone();
                    rebound.source_manifest_digest = next.plan.source_manifest_digest.clone();
                    next.vectors.insert(receipt.chunk_id.clone(), rebound);
                }
            }
        }
        if vector_by_chunk.len()
            != prepared
                .receipt
                .receipts
                .iter()
                .filter(|receipt| {
                    matches!(
                        receipt.operation,
                        ProjectionOperationV1::Added | ProjectionOperationV1::Updated
                    )
                })
                .count()
            || tombstone_by_chunk.len()
                != prepared
                    .receipt
                    .receipts
                    .iter()
                    .filter(|receipt| receipt.operation == ProjectionOperationV1::Deleted)
                    .count()
        {
            return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
        }

        next.checkpoint.completed_batches += 1;
        next.checkpoint.last_request_digest = Some(prepared.request.request_digest.clone());
        next.checkpoint.last_publication_digest = Some(prepared.receipt.publication_digest.clone());
        next.batches.push(prepared);
        let checkpoint = next.checkpoint.clone();
        self.staged.insert(build_id.clone(), next);
        Ok(checkpoint)
    }

    /// Validate a fully staged immutable generation and atomically publish
    /// both its record and active pointer. Partial generations remain in
    /// `staged` and are never returned by active-generation reads.
    pub fn publish_generation(
        &mut self,
        build_id: &VectorGenerationBuildIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        if self.published.active_generation.as_ref() != expected_active_generation {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        let staged = self
            .staged
            .get(build_id)
            .cloned()
            .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
        let expected = staged
            .plan
            .expected_chunk_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let actual = staged.vectors.keys().cloned().collect::<BTreeSet<_>>();
        if expected != actual || staged.batches.is_empty() {
            return Err(VectorGenerationStoreErrorV1::IncompleteGeneration);
        }
        let embedding_key = staged
            .embedding_key
            .clone()
            .ok_or(VectorGenerationStoreErrorV1::IncompleteGeneration)?;
        for vector in staged.vectors.values() {
            validate_vector_row(&staged.plan, &embedding_key, vector)?;
        }

        let manifest_digest =
            generation_identity_digest(&staged.plan, &staged.vectors, &staged.tombstones)?;
        let generation_id = VectorGenerationIdV1::new(manifest_digest.clone());
        let tombstone_digests = staged.tombstones;
        let mut generation = PublishedVectorGenerationV1 {
            generation_id: generation_id.clone(),
            projection_key: staged.plan.target_projection_key,
            source_generation: staged.plan.source_generation,
            source_manifest_digest: staged.plan.source_manifest_digest,
            base_generation: staged.plan.base_generation,
            embedding_key,
            vectors: staged.vectors,
            tombstones: Vec::new(),
            tombstone_digests,
            receipts: staged
                .batches
                .into_iter()
                .map(|batch| batch.receipt)
                .collect(),
            checkpoint: staged.checkpoint.clone(),
            manifest_digest: manifest_digest.clone(),
        };
        generation.canonicalize_tombstones();
        generation.validate_persisted()?;
        let mut next = self.published.clone();
        intern_generation_vectors(&self.physical_vector_pool, &mut next, &generation)?;
        let checkpoint = if let Some(existing) = next.generations.get(&generation_id) {
            if !existing.same_vector_content(&generation) {
                return Err(VectorGenerationStoreErrorV1::ImmutableGenerationConflict);
            }
            existing.checkpoint.clone()
        } else {
            let checkpoint = generation.checkpoint.clone();
            next.generations.insert(generation_id.clone(), generation);
            checkpoint
        };
        next.active_generation = Some(generation_id.clone());
        if self.fail_before_publication_swap {
            self.fail_before_publication_swap = false;
            return Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure);
        }
        self.published = next;
        self.staged.remove(build_id);
        Ok(VectorGenerationPublicationV1 {
            generation_id,
            manifest_digest,
            checkpoint,
        })
    }

    /// Seal a complete generation inside caller-owned scratch state without
    /// making it active. This is the legacy-rebuild staging boundary: the
    /// scratch state is not queryable and can be discarded on any failure.
    pub(crate) fn seal_generation_inactive(
        &mut self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        let prior_active = self.published.active_generation.clone();
        let publication = self.publish_generation(build_id, prior_active.as_ref())?;
        self.published.active_generation = prior_active;
        Ok(publication)
    }

    pub fn active_generation_id(&self) -> Option<&VectorGenerationIdV1> {
        self.published.active_generation.as_ref()
    }

    /// Atomically repoint reads to an already-published immutable generation.
    pub fn activate_generation(
        &mut self,
        generation_id: &VectorGenerationIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        if self.published.active_generation.as_ref() != expected_active_generation {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        let generation = self
            .published
            .generations
            .get(generation_id)
            .cloned()
            .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
        generation.validate_persisted()?;
        let publication = VectorGenerationPublicationV1 {
            generation_id: generation.generation_id().clone(),
            manifest_digest: generation.manifest_digest().clone(),
            checkpoint: generation.checkpoint().clone(),
        };
        let mut next = self.published.clone();
        next.active_generation = Some(generation_id.clone());
        if self.fail_before_publication_swap {
            self.fail_before_publication_swap = false;
            return Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure);
        }
        self.published = next;
        Ok(publication)
    }

    /// Atomically disable semantic reads while retaining immutable generations
    /// for an exact offline rollback.
    pub fn deactivate_generation(
        &mut self,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        if self.published.active_generation.as_ref() != expected_active_generation {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        let mut next = self.published.clone();
        next.active_generation = None;
        if self.fail_before_publication_swap {
            self.fail_before_publication_swap = false;
            return Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure);
        }
        self.published = next;
        Ok(())
    }

    fn commit_legacy_vector_migration(
        &mut self,
        transaction: &LegacyVectorMigrationOwnerTransactionV1,
    ) -> Result<LegacyVectorMigrationReceiptV1, VectorGenerationStoreErrorV1> {
        transaction
            .validate()
            .map_err(|error| VectorGenerationStoreErrorV1::LegacyMigration(error.to_string()))?;
        if self.published.active_generation != transaction.expected_prior_active_generation {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        for rebuilt in transaction
            .receipt
            .items
            .iter()
            .filter_map(|item| item.rebuilt_generation.as_ref())
        {
            if !self.published.generations.contains_key(rebuilt) {
                return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
            }
        }
        if let Some(next_active) = &transaction.next_active_generation
            && !self.published.generations.contains_key(next_active)
        {
            return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
        }

        let mut next = self.published.clone();
        match next
            .legacy_migration_receipts
            .get(&transaction.receipt.receipt_digest)
        {
            Some(existing) if existing != &transaction.receipt => {
                return Err(VectorGenerationStoreErrorV1::ImmutableGenerationConflict);
            }
            Some(_) => {}
            None => {
                next.legacy_migration_receipts.insert(
                    transaction.receipt.receipt_digest.clone(),
                    transaction.receipt.clone(),
                );
            }
        }
        next.active_generation
            .clone_from(&transaction.next_active_generation);
        if self.fail_before_publication_swap {
            self.fail_before_publication_swap = false;
            return Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure);
        }
        self.published = next;
        Ok(transaction.receipt.clone())
    }

    /// Bind scratch-built generations to a validated migration receipt.
    ///
    /// The legacy active pointer belongs to the live state, not this scratch
    /// state, so it is checked by the database replacement transaction.
    fn finish_legacy_replacement(
        &mut self,
        transaction: &LegacyVectorMigrationOwnerTransactionV1,
    ) -> Result<LegacyVectorMigrationReceiptV1, VectorGenerationStoreErrorV1> {
        transaction
            .validate()
            .map_err(|error| VectorGenerationStoreErrorV1::LegacyMigration(error.to_string()))?;
        let mut rebuilt = BTreeMap::new();
        for item in &transaction.receipt.items {
            let Some(generation) = item.rebuilt_generation.as_ref() else {
                continue;
            };
            let identity = (
                item.source_generation.as_ref(),
                item.canonical_chunk_set_digest.as_ref(),
            );
            if rebuilt
                .insert(generation, identity)
                .is_some_and(|existing| existing != identity)
            {
                return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
            }
        }
        if rebuilt.len() != self.published.generations.len() {
            return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
        }
        for (generation_id, (source_generation, expected_chunk_set_digest)) in rebuilt {
            let generation = self
                .published
                .generations
                .get(generation_id)
                .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
            if Some(generation.source_generation()) != source_generation {
                return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
            }
            let chunk_identities = generation
                .vectors
                .iter()
                .map(|(chunk_id, vector)| (chunk_id.clone(), vector.chunk_digest.clone()))
                .collect::<Vec<_>>();
            let actual_chunk_set_digest =
                canonical_chunk_set_digest(generation.source_generation(), &chunk_identities)
                    .map_err(|error| {
                        VectorGenerationStoreErrorV1::LegacyMigration(error.to_string())
                    })?;
            if Some(&actual_chunk_set_digest) != expected_chunk_set_digest {
                return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
            }
        }
        if let Some(next_active) = &transaction.next_active_generation
            && !self.published.generations.contains_key(next_active)
        {
            return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
        }
        self.staged.clear();
        self.published
            .active_generation
            .clone_from(&transaction.next_active_generation);
        self.published.legacy_migration_receipts.insert(
            transaction.receipt.receipt_digest.clone(),
            transaction.receipt.clone(),
        );
        Ok(transaction.receipt.clone())
    }

    pub fn active_checkpoint(&self) -> Option<&VectorProjectionCheckpointV1> {
        self.active_generation()
            .map(PublishedVectorGenerationV1::checkpoint)
    }

    pub fn active_generation(&self) -> Option<&PublishedVectorGenerationV1> {
        self.active_generation_id()
            .and_then(|id| self.published.generations.get(id))
    }

    /// Return the active immutable generation only when every query-facing
    /// projection and source identity matches exactly. A staged replacement
    /// is never considered, so incompatible searches omit semantics rather
    /// than reading stale or partial rows.
    pub fn active_generation_for(
        &self,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Option<&PublishedVectorGenerationV1> {
        self.active_generation().filter(|generation| {
            generation.embedding_key() == embedding_key
                && generation.source_generation() == source_generation
                && generation.source_manifest_digest() == source_manifest_digest
        })
    }

    pub fn generation(
        &self,
        generation_id: &VectorGenerationIdV1,
    ) -> Option<&PublishedVectorGenerationV1> {
        self.published.generations.get(generation_id)
    }

    /// Resolve the shared immutable vector bytes behind one logical generation
    /// occurrence. The returned allocation is reused only inside the exact
    /// projection/privacy authority named by the generation.
    pub fn physical_vector_values(
        &self,
        generation_id: &VectorGenerationIdV1,
        chunk_id: &CodeSearchChunkId,
    ) -> Option<Arc<[f32]>> {
        let physical_id = self
            .published
            .physical_vector_bindings
            .get(generation_id)?
            .get(chunk_id)?;
        self.published
            .physical_vectors
            .get(physical_id)
            .map(|payload| Arc::clone(&payload.values.0))
    }

    pub(crate) fn fail_before_publication_swap_once(&mut self) {
        self.fail_before_publication_swap = true;
    }
}

/// Persistent adapter over the already-open project database.
///
/// The complete generation state is one canonical JSON value guarded by a
/// monotonically increasing revision. Every mutation is a single conditional
/// update, so a reader observes either the complete old state or the complete
/// new state. In particular, an immutable generation record cannot become
/// visible separately from its active-generation pointer.
pub struct DatabaseVectorGenerationStoreV1<'database> {
    database: &'database Database,
}

/// SQLite-backed, non-authoritative state used by the native semantic evaluator.
///
/// It executes the same generation state machine and writer path as
/// production, but uses an isolated row that is removed after the measured
/// run. It can therefore exercise publication/activation without changing the
/// project's active semantic generation.
pub(crate) struct DatabaseVectorEvaluationStoreV1<'database> {
    database: &'database Database,
    evaluation_id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveVectorGenerationSnapshotV1 {
    revision: i64,
    generation: PublishedVectorGenerationV1,
}

impl ActiveVectorGenerationSnapshotV1 {
    pub(crate) const fn revision(&self) -> i64 {
        self.revision
    }

    pub(crate) fn generation(&self) -> &PublishedVectorGenerationV1 {
        &self.generation
    }

    pub(crate) fn into_generation(self) -> PublishedVectorGenerationV1 {
        self.generation
    }
}

/// Identity-only snapshot of the legacy state. The SQL adapter never returns
/// legacy vector payloads to Rust.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DatabaseLegacyVectorInventoryV1 {
    revision: i64,
    inventory: LegacyVectorInventoryV1,
}

impl LegacyVectorInventoryPortV1 for DatabaseLegacyVectorInventoryV1 {
    fn read_only_inventory(
        &self,
    ) -> Result<
        LegacyVectorInventoryV1,
        tracedecay_semantic::legacy_migration::LegacyVectorMigrationErrorV1,
    > {
        Ok(self.inventory.clone())
    }
}

/// Read only the code-generation identities named by structurally readable
/// vector generations, without opening a daemon runtime or deserializing vector
/// payloads. This is the offline equivalent of
/// [`DatabaseVectorGenerationStoreV1::read_legacy_inventory`] followed by
/// [`LegacyVectorInventoryV1::retained_readable_sources`].
pub(crate) fn retained_readable_sources_from_read_only_database(
    database_path: &Path,
) -> Result<BTreeSet<CodeGenerationId>, VectorGenerationStoreErrorV1> {
    retained_readable_sources_from_optional_read_only_database(database_path)?.ok_or_else(|| {
        VectorGenerationStoreErrorV1::Storage(format!(
            "vector generation state table is missing from '{}'",
            database_path.display()
        ))
    })
}

/// Union readable code-generation sources across every graph database in a
/// project store. Code-index files are project-scoped while vector inventories
/// may reside in the root graph database or a branch graph database, so an
/// offline sweep must conservatively mark sources from all inventories.
pub(crate) fn retained_readable_sources_from_read_only_project_store(
    data_root: &Path,
) -> Result<BTreeSet<CodeGenerationId>, VectorGenerationStoreErrorV1> {
    let mut database_paths = vec![data_root.join(tracedecay_runtime_core::config::DB_FILENAME)];
    let branches_root = data_root.join("branches");
    if let Ok(entries) = std::fs::read_dir(&branches_root) {
        for entry in entries {
            let entry = entry.map_err(storage_error)?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("db") {
                database_paths.push(path);
            }
        }
    }
    database_paths.sort();
    let mut readable_sources = BTreeSet::new();
    let mut inventory_count = 0usize;
    for database_path in database_paths {
        if !database_path.is_file() {
            continue;
        }
        if let Some(sources) =
            retained_readable_sources_from_optional_read_only_database(&database_path)?
        {
            inventory_count += 1;
            readable_sources.extend(sources);
        }
    }
    if inventory_count == 0 {
        return Err(VectorGenerationStoreErrorV1::Storage(format!(
            "no vector generation inventory exists under '{}'",
            data_root.display()
        )));
    }
    Ok(readable_sources)
}

fn retained_readable_sources_from_optional_read_only_database(
    database_path: &Path,
) -> Result<Option<BTreeSet<CodeGenerationId>>, VectorGenerationStoreErrorV1> {
    let connection = rusqlite::Connection::open_with_flags(
        database_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(storage_error)?;
    connection
        .busy_timeout(Duration::from_millis(200))
        .map_err(storage_error)?;
    let has_inventory = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM sqlite_schema
                WHERE type = 'table'
                  AND name = 'semantic_vector_generation_state_v1'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(storage_error)?;
    if !has_inventory {
        return Ok(None);
    }
    let (generations_type, active_type, active_raw) = connection
        .query_row(
            "SELECT json_type(state_json, '$.published.generations'),
                    json_type(state_json, '$.published.active_generation'),
                    CAST(json_extract(
                        state_json,
                        '$.published.active_generation'
                    ) AS TEXT)
             FROM semantic_vector_generation_state_v1
             WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(storage_error)?;
    if generations_type.as_deref() != Some("object") {
        return Err(VectorGenerationStoreErrorV1::LegacyMigration(
            "legacy generation inventory is not a JSON object".to_owned(),
        ));
    }
    match (active_type.as_deref(), active_raw.as_deref()) {
        (None | Some("null"), None) => {}
        (Some("text"), Some(raw)) => {
            parse_vector_generation_id(raw)?;
        }
        _ => {
            return Err(VectorGenerationStoreErrorV1::LegacyMigration(
                "legacy active generation identity is unreadable".to_owned(),
            ));
        }
    }
    let mut statement = connection
        .prepare(
            "SELECT entry.key,
                    entry.type,
                    CASE WHEN entry.type = 'object'
                         THEN CAST(json_extract(entry.value, '$.generation_id') AS TEXT)
                    END,
                    CASE WHEN entry.type = 'object'
                         THEN CAST(json_extract(entry.value, '$.source_generation') AS TEXT)
                    END
             FROM semantic_vector_generation_state_v1 AS state
             JOIN json_each(state.state_json, '$.published.generations') AS entry
             WHERE state.singleton = 1
             ORDER BY entry.key",
        )
        .map_err(storage_error)?;
    let mut rows = statement.query([]).map_err(storage_error)?;
    let mut readable_sources = BTreeSet::new();
    while let Some(row) = rows.next().map_err(storage_error)? {
        let map_key = row.get::<_, String>(0).map_err(storage_error)?;
        let value_type = row.get::<_, Option<String>>(1).map_err(storage_error)?;
        let embedded_generation = row.get::<_, Option<String>>(2).map_err(storage_error)?;
        let source_generation = row.get::<_, Option<String>>(3).map_err(storage_error)?;
        let legacy_generation = parse_vector_generation_id(&map_key)?;
        let embedded_matches = embedded_generation
            .as_deref()
            .and_then(|raw| parse_vector_generation_id(raw).ok())
            .as_ref()
            == Some(&legacy_generation);
        let source_generation =
            source_generation.and_then(|raw| CodeGenerationId::try_from(raw).ok());
        if value_type.as_deref() == Some("object")
            && embedded_matches
            && let Some(source_generation) = source_generation
        {
            readable_sources.insert(source_generation);
        }
    }
    Ok(Some(readable_sources))
}

impl<'database> DatabaseVectorGenerationStoreV1<'database> {
    pub async fn open(database: &'database Database) -> Result<Self, VectorGenerationStoreErrorV1> {
        let store = Self::open_legacy_migration(database).await?;
        store.load_state().await?;
        Ok(store)
    }

    /// Open only the identity/atomic-replacement migration boundary.
    ///
    /// Unlike normal runtime open, this does not deserialize legacy state and
    /// therefore remains callable when old vector payloads are unreadable.
    pub(crate) async fn open_legacy_migration(
        database: &'database Database,
    ) -> Result<Self, VectorGenerationStoreErrorV1> {
        database
            .execute_write_batch(
                VECTOR_GENERATION_STATE_OPERATION,
                VECTOR_GENERATION_STATE_SCHEMA_V1,
            )
            .await
            .map_err(storage_error)?;
        let initial_state = serde_json::to_string(&FakeVectorGenerationStoreV1::default())
            .map_err(storage_error)?;
        database
            .execute_write_engine(
                VECTOR_GENERATION_STATE_OPERATION,
                "INSERT OR IGNORE INTO semantic_vector_generation_state_v1 (
                    singleton, revision, state_json
                 ) VALUES (1, 0, ?1)",
                params![initial_state],
            )
            .await
            .map_err(storage_error)?;
        Ok(Self { database })
    }

    /// Read the one active immutable generation needed by a request without
    /// entering the writer lane or deserializing staged/inactive generations.
    pub(crate) async fn read_active_generation_for(
        database: &Database,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        Ok(Self::read_active_generation_snapshot_for(
            database,
            embedding_key,
            source_generation,
            source_manifest_digest,
        )
        .await?
        .map(ActiveVectorGenerationSnapshotV1::into_generation))
    }

    /// Read the atomically active immutable generation without entering the
    /// writer lane. Callers must apply their own source/projection admission.
    pub(crate) async fn read_active_generation(
        database: &Database,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        Ok(Self::read_active_generation_snapshot(database)
            .await?
            .map(ActiveVectorGenerationSnapshotV1::into_generation))
    }

    async fn read_active_generation_snapshot(
        database: &Database,
    ) -> Result<Option<ActiveVectorGenerationSnapshotV1>, VectorGenerationStoreErrorV1> {
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT state.revision, entry.value
                 FROM semantic_vector_generation_state_v1 AS state
                 JOIN json_each(
                     state.state_json,
                     '$.published.generations'
                 ) AS entry
                   ON entry.key = CAST(json_extract(
                       state.state_json,
                       '$.published.active_generation'
                   ) AS TEXT)
                 WHERE state.singleton = 1
                   AND entry.type = 'object'",
                (),
            )
            .await
            .map_err(storage_error)?;
        let Some(row) = rows.next().await.map_err(storage_error)? else {
            return Ok(None);
        };
        let revision = row.get::<i64>(0).map_err(storage_error)?;
        let generation_json = row.get::<String>(1).map_err(storage_error)?;
        drop(rows);
        let generation: PublishedVectorGenerationV1 =
            serde_json::from_str(&generation_json).map_err(storage_error)?;
        generation.validate_persisted()?;
        Ok(Some(ActiveVectorGenerationSnapshotV1 {
            revision,
            generation,
        }))
    }

    pub(crate) async fn read_active_generation_snapshot_for(
        database: &Database,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<Option<ActiveVectorGenerationSnapshotV1>, VectorGenerationStoreErrorV1> {
        let Some(snapshot) = Self::read_active_generation_snapshot(database).await? else {
            return Ok(None);
        };
        if snapshot.generation.embedding_key() != embedding_key
            || snapshot.generation.source_generation() != source_generation
            || snapshot.generation.source_manifest_digest() != source_manifest_digest
        {
            return Ok(None);
        }
        Ok(Some(snapshot))
    }

    pub(crate) async fn active_snapshot_is_current(
        database: &Database,
        revision: i64,
        generation_id: &VectorGenerationIdV1,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT 1
                 FROM semantic_vector_generation_state_v1
                 WHERE singleton = 1
                   AND revision = ?1
                   AND CAST(json_extract(
                       state_json,
                       '$.published.active_generation'
                   ) AS TEXT) = ?2",
                params![revision, generation_id.as_digest().as_str()],
            )
            .await
            .map_err(storage_error)?;
        let is_current = rows.next().await.map_err(storage_error)?.is_some();
        drop(rows);
        Ok(is_current)
    }

    pub(crate) async fn read_generation(
        database: &Database,
        generation_id: &VectorGenerationIdV1,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT entry.value
                 FROM semantic_vector_generation_state_v1 AS state
                 JOIN json_each(
                     state.state_json,
                     '$.published.generations'
                 ) AS entry
                   ON entry.key = ?1
                 WHERE state.singleton = 1
                   AND entry.type = 'object'",
                params![generation_id.as_digest().as_str()],
            )
            .await
            .map_err(storage_error)?;
        let Some(row) = rows.next().await.map_err(storage_error)? else {
            return Ok(None);
        };
        let generation_json = row.get::<String>(0).map_err(storage_error)?;
        drop(rows);
        let generation: PublishedVectorGenerationV1 =
            serde_json::from_str(&generation_json).map_err(storage_error)?;
        generation.validate_persisted()?;
        (generation.generation_id() == generation_id)
            .then_some(generation)
            .ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(
                    "vector generation map key does not match its identity".to_owned(),
                )
            })
            .map(Some)
    }

    pub async fn begin_generation(
        &self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| state.begin_generation(plan.clone()))
            .await
    }

    pub async fn rebuild_generation(
        &self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| state.rebuild_generation(plan.clone()))
            .await
    }

    pub async fn cancel_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| Ok(state.cancel_generation(build_id)))
            .await
    }

    pub async fn commit_batch(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| {
            state.commit_batch(build_id, expected_checkpoint, prepared.clone())
        })
        .await
    }

    pub async fn publish_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| state.publish_generation(build_id, expected_active_generation))
            .await
    }

    pub async fn activate_generation(
        &self,
        generation_id: &VectorGenerationIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| {
            state.activate_generation(generation_id, expected_active_generation)
        })
        .await
    }

    pub async fn deactivate_generation(
        &self,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| state.deactivate_generation(expected_active_generation))
            .await
    }

    /// Snapshot legacy generation identities without deserializing or
    /// returning any legacy vector payload.
    pub(crate) async fn read_legacy_inventory(
        &self,
    ) -> Result<DatabaseLegacyVectorInventoryV1, VectorGenerationStoreErrorV1> {
        let mut rows = self
            .database
            .engine_conn()
            .query(
                "SELECT state.revision,
                        json_type(state.state_json, '$.published.generations'),
                        json_type(state.state_json, '$.published.active_generation'),
                        CAST(json_extract(
                            state.state_json,
                            '$.published.active_generation'
                        ) AS TEXT),
                        entry.key,
                        entry.type,
                        CASE WHEN entry.type = 'object'
                             THEN CAST(json_extract(
                                 entry.value,
                                 '$.generation_id'
                             ) AS TEXT)
                        END,
                        CASE WHEN entry.type = 'object'
                             THEN CAST(json_extract(
                                 entry.value,
                                 '$.source_generation'
                             ) AS TEXT)
                        END
                 FROM semantic_vector_generation_state_v1 AS state
                 LEFT JOIN json_each(
                     state.state_json,
                     '$.published.generations'
                 ) AS entry
                 WHERE state.singleton = 1
                 ORDER BY entry.key",
                (),
            )
            .await
            .map_err(storage_error)?;
        let mut revision = None;
        let mut expected_active_generation = None;
        let mut entries = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage_error)? {
            let row_revision = row.get::<i64>(0).map_err(storage_error)?;
            if revision
                .replace(row_revision)
                .is_some_and(|prior| prior != row_revision)
            {
                return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
            }
            if row
                .get::<Option<String>>(1)
                .map_err(storage_error)?
                .as_deref()
                != Some("object")
            {
                return Err(VectorGenerationStoreErrorV1::LegacyMigration(
                    "legacy generation inventory is not a JSON object".to_owned(),
                ));
            }
            let active_type = row.get::<Option<String>>(2).map_err(storage_error)?;
            let active_raw = row.get::<Option<String>>(3).map_err(storage_error)?;
            expected_active_generation = match (active_type.as_deref(), active_raw.as_deref()) {
                (None | Some("null"), None) => None,
                (Some("text"), Some(raw)) => Some(parse_vector_generation_id(raw)?),
                _ => {
                    return Err(VectorGenerationStoreErrorV1::LegacyMigration(
                        "legacy active generation identity is unreadable".to_owned(),
                    ));
                }
            };
            let Some(map_key) = row.get::<Option<String>>(4).map_err(storage_error)? else {
                continue;
            };
            let legacy_generation = parse_vector_generation_id(&map_key)?;
            let value_type = row.get::<Option<String>>(5).map_err(storage_error)?;
            let embedded_generation = row.get::<Option<String>>(6).map_err(storage_error)?;
            let source_generation = row.get::<Option<String>>(7).map_err(storage_error)?;
            let readable = value_type.as_deref() == Some("object")
                && embedded_generation
                    .as_deref()
                    .and_then(|raw| parse_vector_generation_id(raw).ok())
                    .as_ref()
                    == Some(&legacy_generation)
                && source_generation
                    .as_deref()
                    .and_then(|raw| CodeGenerationId::try_from(raw.to_owned()).ok())
                    .is_some();
            if readable {
                entries.push(LegacyVectorInventoryEntryV1::Readable {
                    legacy_generation,
                    source_generation: CodeGenerationId::try_from(
                        source_generation.unwrap_or_default(),
                    )
                    .map_err(|error| {
                        VectorGenerationStoreErrorV1::LegacyMigration(error.to_string())
                    })?,
                });
            } else {
                let reason_digest = canonical_sha256(&(
                    LEGACY_VECTOR_UNREADABLE_REASON_DOMAIN_V1,
                    &map_key,
                    &value_type,
                    &embedded_generation,
                    &source_generation,
                ))
                .map_err(storage_error)?;
                entries.push(LegacyVectorInventoryEntryV1::Unreadable {
                    legacy_generation,
                    reason_digest,
                });
            }
        }
        drop(rows);
        Ok(DatabaseLegacyVectorInventoryV1 {
            revision: revision.ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(
                    "vector generation state row is missing".to_owned(),
                )
            })?,
            inventory: LegacyVectorInventoryV1 {
                expected_active_generation,
                entries,
            },
        })
    }

    /// Return a durable completed migration receipt, if atomic replacement
    /// already committed. A crash before replacement has no receipt and is
    /// therefore safely retried; a restart after replacement performs no
    /// second rebuild.
    pub(crate) async fn completed_legacy_migration_receipt(
        &self,
    ) -> Result<Option<LegacyVectorMigrationReceiptV1>, VectorGenerationStoreErrorV1> {
        let mut rows = self
            .database
            .engine_conn()
            .query(
                "SELECT entry.key, entry.value
                 FROM semantic_vector_generation_state_v1 AS state
                 JOIN json_each(
                     state.state_json,
                     '$.published.legacy_migration_receipts'
                 ) AS entry
                 WHERE state.singleton = 1
                   AND entry.type = 'object'
                 ORDER BY entry.key",
                (),
            )
            .await
            .map_err(storage_error)?;
        let mut completed = None;
        while let Some(row) = rows.next().await.map_err(storage_error)? {
            let key = row.get::<String>(0).map_err(storage_error)?;
            let receipt_json = row.get::<String>(1).map_err(storage_error)?;
            let receipt: LegacyVectorMigrationReceiptV1 =
                serde_json::from_str(&receipt_json).map_err(storage_error)?;
            receipt.validate().map_err(|error| {
                VectorGenerationStoreErrorV1::LegacyMigration(error.to_string())
            })?;
            if receipt.receipt_digest.as_str() != key {
                return Err(VectorGenerationStoreErrorV1::LegacyMigration(
                    "legacy migration receipt key does not match its digest".to_owned(),
                ));
            }
            completed = Some(receipt);
        }
        Ok(completed)
    }

    /// Replace the complete legacy state with scratch-built canonical
    /// generations in one guarded writer transaction. Unreadable state is
    /// copied into an isolated quarantine table by `SQLite` itself; its bytes
    /// never cross the Rust migration boundary.
    pub(crate) async fn replace_legacy_vectors_atomically(
        &self,
        inventory: &DatabaseLegacyVectorInventoryV1,
        mut replacement: FakeVectorGenerationStoreV1,
        transaction: &LegacyVectorMigrationOwnerTransactionV1,
    ) -> Result<LegacyVectorMigrationReceiptV1, VectorGenerationStoreErrorV1> {
        if inventory.inventory.expected_active_generation
            != transaction.expected_prior_active_generation
        {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        let receipt = replacement.finish_legacy_replacement(transaction)?;
        validate_loaded_state(&replacement)?;
        let state_json = serde_json::to_string(&replacement).map_err(storage_error)?;
        let receipt_json = serde_json::to_string(&receipt).map_err(storage_error)?;
        let quarantined_items = receipt
            .items
            .iter()
            .filter(|item| item.outcome == LegacyVectorMigrationOutcomeKindV1::QuarantineUnreadable)
            .collect::<Vec<_>>();
        let receipt_digest = receipt.receipt_digest.as_str().to_owned();

        let writer = self
            .database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        let mut current_rows = writer
            .query_engine(
                "SELECT revision,
                        CAST(json_extract(
                            state_json,
                            '$.published.active_generation'
                        ) AS TEXT)
                 FROM semantic_vector_generation_state_v1
                 WHERE singleton = 1",
                (),
            )
            .await
            .map_err(storage_error)?;
        let current = current_rows
            .next()
            .await
            .map_err(storage_error)?
            .ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(
                    "vector generation state row is missing".to_owned(),
                )
            })?;
        let current_revision = current.get::<i64>(0).map_err(storage_error)?;
        let current_active = current
            .get::<Option<String>>(1)
            .map_err(storage_error)?
            .as_deref()
            .map(parse_vector_generation_id)
            .transpose()?;
        drop(current_rows);
        if current_revision != inventory.revision
            || current_active != inventory.inventory.expected_active_generation
        {
            writer.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        if !quarantined_items.is_empty() {
            writer
                .execute_batch_engine(LEGACY_VECTOR_QUARANTINE_SCHEMA_V1)
                .await
                .map_err(storage_error)?;
            for item in quarantined_items {
                let reason = item.quarantine_reason_digest.as_ref().ok_or_else(|| {
                    VectorGenerationStoreErrorV1::LegacyMigration(
                        "quarantine receipt has no reason digest".to_owned(),
                    )
                })?;
                let inserted = writer
                    .execute_engine(
                        "INSERT INTO semantic_legacy_vector_quarantine_v1 (
                        receipt_digest,
                        legacy_generation,
                        reason_digest,
                        generation_json,
                        receipt_json
                     )
                     SELECT ?1,
                            ?2,
                            ?3,
                            CASE entry.type
                                WHEN 'text' THEN json_quote(entry.value)
                                WHEN 'null' THEN 'null'
                                ELSE CAST(entry.value AS TEXT)
                            END,
                            ?4
                     FROM semantic_vector_generation_state_v1 AS state,
                          json_each(
                              state.state_json,
                              '$.published.generations'
                          ) AS entry
                     WHERE state.singleton = 1
                       AND state.revision = ?5
                       AND entry.key = ?2",
                        params![
                            receipt_digest.clone(),
                            item.legacy_generation.as_digest().as_str(),
                            reason.as_str(),
                            receipt_json.clone(),
                            inventory.revision
                        ],
                    )
                    .await
                    .map_err(storage_error)?;
                if inserted != 1 {
                    writer.rollback().await.map_err(storage_error)?;
                    return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
                }
            }
        }
        let changed = writer
            .execute_engine(
                "UPDATE semantic_vector_generation_state_v1
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1 AND revision = ?2",
                params![state_json, inventory.revision],
            )
            .await
            .map_err(storage_error)?;
        if changed != 1 {
            writer.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        writer.commit().await.map_err(storage_error)?;
        Ok(receipt)
    }

    pub async fn active_generation_id(
        &self,
    ) -> Result<Option<VectorGenerationIdV1>, VectorGenerationStoreErrorV1> {
        let (_, state) = self.load_state().await?;
        Ok(state.active_generation_id().cloned())
    }

    pub async fn active_checkpoint(
        &self,
    ) -> Result<Option<VectorProjectionCheckpointV1>, VectorGenerationStoreErrorV1> {
        let (_, state) = self.load_state().await?;
        Ok(state.active_checkpoint().cloned())
    }

    pub async fn active_generation(
        &self,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let (_, state) = self.load_state().await?;
        Ok(state.active_generation().cloned())
    }

    pub async fn active_generation_for(
        &self,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let (_, state) = self.load_state().await?;
        Ok(state
            .active_generation_for(embedding_key, source_generation, source_manifest_digest)
            .cloned())
    }

    pub async fn generation(
        &self,
        generation_id: &VectorGenerationIdV1,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let (_, state) = self.load_state().await?;
        Ok(state.generation(generation_id).cloned())
    }

    pub async fn physical_vector_values(
        &self,
        generation_id: &VectorGenerationIdV1,
        chunk_id: &CodeSearchChunkId,
    ) -> Result<Option<Arc<[f32]>>, VectorGenerationStoreErrorV1> {
        let (_, state) = self.load_state().await?;
        Ok(state.physical_vector_values(generation_id, chunk_id))
    }

    async fn mutate_state<ResultValue>(
        &self,
        mut mutation: impl FnMut(
            &mut FakeVectorGenerationStoreV1,
        ) -> Result<ResultValue, VectorGenerationStoreErrorV1>,
    ) -> Result<ResultValue, VectorGenerationStoreErrorV1> {
        for _ in 0..MAX_STATE_CAS_RETRIES {
            let (revision, mut state) = self.load_state().await?;
            let result = mutation(&mut state)?;
            let state_json = serde_json::to_string(&state).map_err(storage_error)?;
            let changed = self
                .database
                .execute_write_engine(
                    VECTOR_GENERATION_STATE_OPERATION,
                    "UPDATE semantic_vector_generation_state_v1
                     SET revision = revision + 1, state_json = ?1
                     WHERE singleton = 1 AND revision = ?2",
                    params![state_json, revision],
                )
                .await
                .map_err(storage_error)?;
            if changed == 1 {
                return Ok(result);
            }
        }
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }

    async fn load_state(
        &self,
    ) -> Result<(i64, FakeVectorGenerationStoreV1), VectorGenerationStoreErrorV1> {
        let mut rows = self
            .database
            .engine_conn()
            .query(
                "SELECT revision, state_json
                 FROM semantic_vector_generation_state_v1
                 WHERE singleton = 1",
                (),
            )
            .await
            .map_err(storage_error)?;
        let row = rows.next().await.map_err(storage_error)?.ok_or_else(|| {
            VectorGenerationStoreErrorV1::Storage(
                "vector generation state row is missing".to_string(),
            )
        })?;
        let revision = row.get::<i64>(0).map_err(storage_error)?;
        let state_json = row.get::<String>(1).map_err(storage_error)?;
        drop(rows);
        let mut state: FakeVectorGenerationStoreV1 =
            serde_json::from_str(&state_json).map_err(storage_error)?;
        state.ensure_physical_reuse_index()?;
        validate_loaded_state(&state)?;
        Ok((revision, state))
    }
}

impl<'database> DatabaseVectorEvaluationStoreV1<'database> {
    pub(crate) async fn open(
        database: &'database Database,
        evaluation_id: impl Into<String>,
    ) -> Result<Self, VectorGenerationStoreErrorV1> {
        let evaluation_id = evaluation_id.into();
        if evaluation_id.is_empty()
            || evaluation_id.len() > 256
            || evaluation_id.trim() != evaluation_id
            || evaluation_id.chars().any(char::is_control)
        {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "semantic evaluation identity is invalid".to_owned(),
            ));
        }
        database
            .execute_write_batch(
                VECTOR_GENERATION_STATE_OPERATION,
                VECTOR_EVALUATION_STATE_SCHEMA_V1,
            )
            .await
            .map_err(storage_error)?;
        let initial_state = serde_json::to_string(&FakeVectorGenerationStoreV1::default())
            .map_err(storage_error)?;
        let inserted = database
            .execute_write_engine(
                VECTOR_GENERATION_STATE_OPERATION,
                "INSERT INTO semantic_vector_evaluation_state_v1 (
                    evaluation_id, revision, state_json
                 ) VALUES (?1, 0, ?2)",
                params![evaluation_id.clone(), initial_state],
            )
            .await
            .map_err(storage_error)?;
        if inserted != 1 {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "semantic evaluation state could not be initialized".to_owned(),
            ));
        }
        Ok(Self {
            database,
            evaluation_id,
        })
    }

    pub(crate) async fn rebuild_generation(
        &self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| state.rebuild_generation(plan.clone()))
            .await
    }

    pub(crate) async fn commit_batch(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| {
            state.commit_batch(build_id, expected_checkpoint, prepared.clone())
        })
        .await
    }

    pub(crate) async fn publish_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| state.publish_generation(build_id, expected_active_generation))
            .await
    }

    pub(crate) async fn cancel_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| Ok(state.cancel_generation(build_id)))
            .await
    }

    pub(crate) async fn active_generation_id(
        &self,
    ) -> Result<Option<VectorGenerationIdV1>, VectorGenerationStoreErrorV1> {
        let (_, state) = self.load_state().await?;
        Ok(state.active_generation_id().cloned())
    }

    pub(crate) async fn active_generation(
        &self,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let (_, state) = self.load_state().await?;
        Ok(state.active_generation().cloned())
    }

    pub(crate) async fn active_generation_for(
        &self,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let (_, state) = self.load_state().await?;
        Ok(state
            .active_generation_for(embedding_key, source_generation, source_manifest_digest)
            .cloned())
    }

    pub(crate) async fn close(self) -> Result<(), VectorGenerationStoreErrorV1> {
        let deleted = self
            .database
            .execute_write_engine(
                VECTOR_GENERATION_STATE_OPERATION,
                "DELETE FROM semantic_vector_evaluation_state_v1
                 WHERE evaluation_id = ?1",
                params![self.evaluation_id],
            )
            .await
            .map_err(storage_error)?;
        if deleted != 1 {
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        Ok(())
    }

    async fn mutate_state<ResultValue>(
        &self,
        mut mutation: impl FnMut(
            &mut FakeVectorGenerationStoreV1,
        ) -> Result<ResultValue, VectorGenerationStoreErrorV1>,
    ) -> Result<ResultValue, VectorGenerationStoreErrorV1> {
        for _ in 0..MAX_STATE_CAS_RETRIES {
            let (revision, mut state) = self.load_state().await?;
            let result = mutation(&mut state)?;
            let state_json = serde_json::to_string(&state).map_err(storage_error)?;
            let changed = self
                .database
                .execute_write_engine(
                    VECTOR_GENERATION_STATE_OPERATION,
                    "UPDATE semantic_vector_evaluation_state_v1
                     SET revision = revision + 1, state_json = ?1
                     WHERE evaluation_id = ?2 AND revision = ?3",
                    params![state_json, self.evaluation_id.clone(), revision],
                )
                .await
                .map_err(storage_error)?;
            if changed == 1 {
                return Ok(result);
            }
        }
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }

    async fn load_state(
        &self,
    ) -> Result<(i64, FakeVectorGenerationStoreV1), VectorGenerationStoreErrorV1> {
        let mut rows = self
            .database
            .engine_conn()
            .query(
                "SELECT revision, state_json
                 FROM semantic_vector_evaluation_state_v1
                 WHERE evaluation_id = ?1",
                params![self.evaluation_id.clone()],
            )
            .await
            .map_err(storage_error)?;
        let row = rows.next().await.map_err(storage_error)?.ok_or_else(|| {
            VectorGenerationStoreErrorV1::Storage(
                "semantic evaluation state row is missing".to_owned(),
            )
        })?;
        let revision = row.get::<i64>(0).map_err(storage_error)?;
        let state_json = row.get::<String>(1).map_err(storage_error)?;
        drop(rows);
        let mut state: FakeVectorGenerationStoreV1 =
            serde_json::from_str(&state_json).map_err(storage_error)?;
        state.ensure_physical_reuse_index()?;
        validate_loaded_state(&state)?;
        Ok((revision, state))
    }
}

impl FakeVectorGenerationStoreV1 {
    fn ensure_physical_reuse_index(&mut self) -> Result<(), VectorGenerationStoreErrorV1> {
        let generations = self
            .published
            .generations
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for generation in &generations {
            intern_generation_vectors(&self.physical_vector_pool, &mut self.published, generation)?;
        }
        Ok(())
    }
}

fn physical_vector_reuse_key(
    embedding_key: &AdmittedEmbeddingProjectionKeyV1,
    vector: &ProjectedChunkVectorV1,
) -> Result<(ManifestDigest, PhysicalVectorReuseKeyV1), VectorGenerationStoreErrorV1> {
    if embedding_key.projection_key() != &vector.projection_key {
        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
    }
    let reuse_key = PhysicalVectorReuseKeyV1 {
        canonical_chunk_digest: vector.chunk_digest.clone(),
        projection_key: vector.projection_key.clone(),
        admitted_embedding_key: embedding_key.clone(),
        privacy_domain: embedding_key.privacy_domain().clone(),
        privacy_key_epoch: embedding_key.privacy_key_epoch(),
    };
    let physical_id = canonical_sha256(&(PHYSICAL_VECTOR_REUSE_DIGEST_DOMAIN, &reuse_key))
        .map_err(|error| VectorGenerationStoreErrorV1::Storage(error.to_string()))?;
    Ok((physical_id, reuse_key))
}

fn intern_generation_vectors(
    physical_vector_pool: &PhysicalVectorBytePoolV1,
    published: &mut PublishedStateV1,
    generation: &PublishedVectorGenerationV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let mut bindings = BTreeMap::new();
    for (chunk_id, vector) in &generation.vectors {
        let (physical_id, reuse_key) =
            physical_vector_reuse_key(&generation.embedding_key, vector)?;
        match published.physical_vectors.get(&physical_id) {
            Some(existing)
                if existing.reuse_key != reuse_key
                    || existing.values.0.as_ref() != vector.values.as_slice() =>
            {
                return Err(VectorGenerationStoreErrorV1::PhysicalVectorConflict);
            }
            Some(_) => {}
            None => {}
        }
        let shared = physical_vector_pool.intern(&reuse_key, &vector.values)?;
        published.physical_vectors.insert(
            physical_id.clone(),
            PhysicalVectorPayloadV1 {
                reuse_key,
                values: SharedVectorBytesV1(shared),
            },
        );
        bindings.insert(chunk_id.clone(), physical_id);
    }
    match published
        .physical_vector_bindings
        .get(generation.generation_id())
    {
        Some(existing) if existing != &bindings => {
            Err(VectorGenerationStoreErrorV1::ImmutableGenerationConflict)
        }
        Some(_) => Ok(()),
        None => {
            published
                .physical_vector_bindings
                .insert(generation.generation_id().clone(), bindings);
            Ok(())
        }
    }
}

fn validate_loaded_state(
    state: &FakeVectorGenerationStoreV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if let Some(active) = &state.published.active_generation
        && !state.published.generations.contains_key(active)
    {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "active vector generation pointer is dangling".to_string(),
        ));
    }
    for (receipt_digest, receipt) in &state.published.legacy_migration_receipts {
        if &receipt.receipt_digest != receipt_digest || receipt.validate().is_err() {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "legacy vector migration receipt is invalid".to_string(),
            ));
        }
    }
    for (generation_id, generation) in &state.published.generations {
        if generation.generation_id() != generation_id {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "published generation map key does not match record id".to_string(),
            ));
        }
        generation.validate_persisted()?;
        let bindings = state
            .published
            .physical_vector_bindings
            .get(generation_id)
            .ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(
                    "published generation has no physical vector bindings".to_string(),
                )
            })?;
        if bindings.len() != generation.vectors.len() {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "published generation physical vector membership is incomplete".to_string(),
            ));
        }
        for (chunk_id, vector) in &generation.vectors {
            let physical_id = bindings.get(chunk_id).ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(format!(
                    "published vector {chunk_id} has no physical byte binding"
                ))
            })?;
            let physical = state
                .published
                .physical_vectors
                .get(physical_id)
                .ok_or_else(|| {
                    VectorGenerationStoreErrorV1::Storage(format!(
                        "published vector {chunk_id} has a dangling physical byte binding"
                    ))
                })?;
            let (expected_id, expected_key) =
                physical_vector_reuse_key(generation.embedding_key(), vector)?;
            if physical_id != &expected_id
                || physical.reuse_key != expected_key
                || physical.values.0.as_ref() != vector.values.as_slice()
            {
                return Err(VectorGenerationStoreErrorV1::Storage(format!(
                    "published vector {chunk_id} physical byte binding drifted"
                )));
            }
        }
    }
    for staged in state.staged.values() {
        if let Some(embedding_key) = &staged.embedding_key {
            for vector in staged.vectors.values() {
                validate_vector_row(&staged.plan, embedding_key, vector)?;
            }
        }
        let canonical = staged.tombstones.keys().cloned().collect::<BTreeSet<_>>();
        if staged.tombstones.len() != canonical.len() {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "staged tombstones contain duplicate chunk ids".to_string(),
            ));
        }
        for chunk_id in staged.tombstones.keys() {
            if staged.vectors.contains_key(chunk_id) {
                return Err(VectorGenerationStoreErrorV1::Storage(format!(
                    "staged generation retains both vector and tombstone for {chunk_id}"
                )));
            }
        }
    }
    Ok(())
}

fn validate_published_receipts(
    generation: &PublishedVectorGenerationV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let checkpoint = generation.checkpoint();
    if checkpoint.target_projection_key != *generation.projection_key()
        || checkpoint.source_generation != *generation.source_generation()
        || checkpoint.source_manifest_digest != *generation.source_manifest_digest()
        || checkpoint.completed_batches == 0
        || checkpoint.completed_batches != generation.receipts().len() as u64
    {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "published generation checkpoint is incomplete or incompatible".to_owned(),
        ));
    }
    let last = generation.receipts().last().ok_or_else(|| {
        VectorGenerationStoreErrorV1::Storage(
            "published generation has no projection receipt".to_owned(),
        )
    })?;
    if checkpoint.last_request_digest.as_ref() != Some(&last.request_digest)
        || checkpoint.last_publication_digest.as_ref() != Some(&last.publication_digest)
    {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "published generation checkpoint does not name its last receipt".to_owned(),
        ));
    }

    let mut effects = BTreeSet::new();
    for batch in generation.receipts() {
        if batch.target_projection_key != *generation.projection_key()
            || batch.source_generation != *generation.source_generation()
            || expected_publication_digest(batch).map_err(storage_error)?
                != batch.publication_digest
            || batch.reused_count
                != batch
                    .receipts
                    .iter()
                    .filter(|receipt| receipt.operation == ProjectionOperationV1::Reused)
                    .count() as u64
        {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "published projection batch receipt is incompatible".to_owned(),
            ));
        }
        for receipt in &batch.receipts {
            if !effects.insert(receipt.chunk_id.clone())
                || receipt.projection_key != *generation.projection_key()
                || receipt.request_digest != batch.request_digest
                || receipt.source_generation != *generation.source_generation()
                || receipt.source_manifest_digest != batch.source_manifest_digest
            {
                return Err(VectorGenerationStoreErrorV1::Storage(
                    "published chunk receipt is duplicated or incompatible".to_owned(),
                ));
            }
            match receipt.operation {
                ProjectionOperationV1::Added | ProjectionOperationV1::Updated => {
                    let vector = generation.vectors().get(&receipt.chunk_id);
                    if receipt.outcome != ProjectionOutcomeV1::Applied
                        || vector.is_none()
                        || receipt.current_chunk_digest.as_ref()
                            != vector.map(|vector| &vector.chunk_digest)
                        || receipt.output_digest.as_ref()
                            != vector.map(|vector| &vector.output_digest)
                        || generation
                            .tombstone_digests()
                            .contains_key(&receipt.chunk_id)
                    {
                        return Err(VectorGenerationStoreErrorV1::Storage(
                            "published applied receipt has no matching vector".to_owned(),
                        ));
                    }
                }
                ProjectionOperationV1::Reused => {
                    let vector = generation.vectors().get(&receipt.chunk_id);
                    if receipt.outcome != ProjectionOutcomeV1::Reused
                        || vector.is_none()
                        || receipt.prior_chunk_digest.as_ref()
                            != vector.map(|vector| &vector.chunk_digest)
                        || receipt.current_chunk_digest.as_ref()
                            != vector.map(|vector| &vector.chunk_digest)
                        || receipt.output_digest.is_some()
                        || generation
                            .tombstone_digests()
                            .contains_key(&receipt.chunk_id)
                    {
                        return Err(VectorGenerationStoreErrorV1::Storage(
                            "published reused receipt has no matching vector".to_owned(),
                        ));
                    }
                }
                ProjectionOperationV1::Deleted => {
                    if receipt.outcome != ProjectionOutcomeV1::Applied
                        || receipt.current_chunk_digest.is_some()
                        || receipt.output_digest.is_some()
                        || receipt.prior_chunk_digest.as_ref()
                            != generation.tombstone_digests().get(&receipt.chunk_id)
                        || generation.vectors().contains_key(&receipt.chunk_id)
                    {
                        return Err(VectorGenerationStoreErrorV1::Storage(
                            "published deletion receipt has no matching tombstone".to_owned(),
                        ));
                    }
                }
            }
        }
    }

    let expected_effects = generation
        .vectors()
        .keys()
        .chain(generation.tombstone_digests().keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    if effects != expected_effects {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "published generation receipt membership is incomplete".to_owned(),
        ));
    }
    Ok(())
}

fn storage_error(error: impl std::fmt::Display) -> VectorGenerationStoreErrorV1 {
    VectorGenerationStoreErrorV1::Storage(error.to_string())
}

fn parse_vector_generation_id(
    raw: &str,
) -> Result<VectorGenerationIdV1, VectorGenerationStoreErrorV1> {
    ManifestDigest::try_from(raw.to_owned())
        .map(VectorGenerationIdV1::new)
        .map_err(|error| VectorGenerationStoreErrorV1::LegacyMigration(error.to_string()))
}

/// Derive the immutable vector-generation identity from projected content,
/// not from resumable execution evidence. Receipt batches and checkpoints
/// remain available for audit but must not change the generation they produced.
fn generation_identity_digest(
    plan: &VectorGenerationPlanV1,
    vectors: &BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>,
    tombstones: &BTreeMap<CodeSearchChunkId, ContentDigest>,
) -> Result<ManifestDigest, VectorGenerationStoreErrorV1> {
    let vector_digests = vectors
        .iter()
        .map(|(chunk_id, vector)| (chunk_id, &vector.output_digest))
        .collect::<Vec<_>>();
    let tombstone_digests = tombstones.iter().collect::<Vec<_>>();
    canonical_sha256(&(
        VECTOR_GENERATION_MANIFEST_DIGEST_DOMAIN,
        &plan.target_projection_key,
        &plan.source_generation,
        &plan.source_manifest_digest,
        &plan.expected_chunk_ids,
        vector_digests,
        tombstone_digests,
    ))
    .map_err(|error| VectorGenerationStoreErrorV1::InvalidPlan(error.to_string()))
}

fn validate_plan(plan: &VectorGenerationPlanV1) -> Result<(), VectorGenerationStoreErrorV1> {
    if plan.target_projection_key.kind != ProjectionKindV1::Embedding {
        return Err(VectorGenerationStoreErrorV1::InvalidPlan(
            "target projection is not embedding".to_string(),
        ));
    }
    plan.source_generation
        .validate()
        .map_err(|error| VectorGenerationStoreErrorV1::InvalidPlan(error.to_string()))?;
    plan.source_manifest_digest
        .validate()
        .map_err(|error| VectorGenerationStoreErrorV1::InvalidPlan(error.to_string()))?;
    if plan
        .expected_chunk_ids
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return Err(VectorGenerationStoreErrorV1::InvalidPlan(
            "expected chunk IDs are not canonical".to_string(),
        ));
    }
    Ok(())
}

fn validate_batch_identity(
    plan: &VectorGenerationPlanV1,
    prepared: &PreparedVectorGenerationV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if prepared.request.target_projection_key != plan.target_projection_key
        || prepared.receipt.target_projection_key != plan.target_projection_key
        || prepared.request.changes.to_generation != plan.source_generation
        || prepared.receipt.source_generation != plan.source_generation
    {
        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
    }
    if prepared.embedding_key.projection_key() != &plan.target_projection_key {
        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
    }
    Ok(())
}

fn validate_base_generation_for_batch(
    published: &PublishedStateV1,
    plan: &VectorGenerationPlanV1,
    prepared: &PreparedVectorGenerationV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let Some(base_id) = plan.base_generation.as_ref() else {
        return Ok(());
    };
    let base = published
        .generations
        .get(base_id)
        .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
    if prepared.request.changes.from_generation.as_ref() != Some(base.source_generation())
        || prepared.request.previous_projection_key.as_ref() != Some(base.projection_key())
        || (prepared.request.target_projection_key == *base.projection_key()
            && prepared.embedding_key != *base.embedding_key())
    {
        return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
    }
    Ok(())
}

fn validate_prepared_vector_row(
    prepared: &PreparedVectorGenerationV1,
    vector: &ProjectedChunkVectorV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if vector.projection_key != prepared.request.target_projection_key
        || vector.source_generation != prepared.request.changes.to_generation
        || vector.source_manifest_digest != prepared.request.changes.manifest_digest
    {
        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
    }
    vector.validate(prepared.embedding_key.embedding_key().dimensions)?;
    Ok(())
}

fn validate_vector_row(
    plan: &VectorGenerationPlanV1,
    embedding_key: &AdmittedEmbeddingProjectionKeyV1,
    vector: &ProjectedChunkVectorV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if vector.projection_key != plan.target_projection_key
        || vector.source_generation != plan.source_generation
        || vector.source_manifest_digest != plan.source_manifest_digest
    {
        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
    }
    vector.validate(embedding_key.embedding_key().dimensions)?;
    Ok(())
}

fn validate_vector_row_for_published(
    generation: &PublishedVectorGenerationV1,
    vector: &ProjectedChunkVectorV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if vector.projection_key != generation.projection_key
        || vector.source_generation != generation.source_generation
        || vector.source_manifest_digest != generation.source_manifest_digest
    {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "published vector row identity drifted from generation metadata".to_string(),
        ));
    }
    vector
        .validate(generation.embedding_key.embedding_key().dimensions)
        .map_err(|error| VectorGenerationStoreErrorV1::Storage(error.to_string()))?;
    Ok(())
}

fn base_vector<'a>(
    published: &'a PublishedStateV1,
    plan: &VectorGenerationPlanV1,
    chunk_id: &CodeSearchChunkId,
) -> Result<&'a ProjectedChunkVectorV1, VectorGenerationStoreErrorV1> {
    let base_id = plan
        .base_generation
        .as_ref()
        .ok_or_else(|| VectorGenerationStoreErrorV1::MissingBaseVector(chunk_id.clone()))?;
    published
        .generations
        .get(base_id)
        .and_then(|generation| generation.vectors.get(chunk_id))
        .ok_or_else(|| VectorGenerationStoreErrorV1::MissingBaseVector(chunk_id.clone()))
}

fn validate_base_digest(
    published: &PublishedStateV1,
    plan: &VectorGenerationPlanV1,
    receipt: &tracedecay_domain::CodeChunkProjectionReceiptV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let base = base_vector(published, plan, &receipt.chunk_id)?;
    if receipt.prior_chunk_digest.as_ref() != Some(&base.chunk_digest) {
        return Err(VectorGenerationStoreErrorV1::MissingBaseVector(
            receipt.chunk_id.clone(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        BoundedSanitizedText, ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision,
        CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, EmbeddingDeviceClassV1, EmbeddingMetricV1,
        EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
        EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1, FileOccurrenceId,
        LanguageDescriptorRevision, PolicyRevisionId, PrivacyDomainId, ProjectionBatchRequestV1,
        ProjectionReplayReasonV1, SanitizerRevision, SensitivityDecision, SensitivityLevelV1,
        SourceSpan,
    };
    use tracedecay_runtime_core::db::{DatabaseAuthority, TestDatabaseRuntimeMode};
    use tracedecay_semantic::legacy_migration::{
        CanonicalEligibleChunkSetV1, NeverCancelLegacyVectorMigrationV1,
        ProductionLegacyVectorCanonicalRebuilderV1, StagedCanonicalVectorRebuildV1,
        prepare_legacy_vector_migration,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("canonical test identity")
    }

    fn manifest_digest(byte: char) -> ManifestDigest {
        id(&format!("sha256:{}", byte.to_string().repeat(64)))
    }

    fn content_digest(byte: char) -> ContentDigest {
        id(&format!("sha256:{}", byte.to_string().repeat(64)))
    }

    fn canonical_chunk(
        chunk_id: &str,
        source_generation: &CodeGenerationId,
        digest: char,
    ) -> tracedecay_domain::CodeSearchChunkV1 {
        tracedecay_domain::CodeSearchChunkV1 {
            id: id(chunk_id),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: source_generation.clone(),
                file_occurrence_id: id::<FileOccurrenceId>("file.rs"),
                symbol_occurrence_id: None,
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: 4,
                },
                grain: CodeSearchChunkGrainV1::FileWindow,
                ordinal: 0,
            },
            content_digest: content_digest(digest),
            language_descriptor_revision: id::<LanguageDescriptorRevision>("rust.v1"),
            chunker_revision: id::<ChunkerRevision>("chunker.v1"),
            sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
            sensitivity: SensitivityDecision {
                level: SensitivityLevelV1::Public,
                policy_revision: id::<PolicyRevisionId>("policy.v1"),
            },
            exact_terms: vec![],
            subtokens: vec![],
            sanitized_text: BoundedSanitizedText::new("code").expect("sanitized text"),
        }
    }

    fn admitted_embedding() -> AdmittedEmbeddingProjectionKeyV1 {
        EmbeddingProjectionKeyV1 {
            model_artifact_digest: manifest_digest('1'),
            tokenizer_digest: manifest_digest('2'),
            config_digest: manifest_digest('3'),
            query_instruction_digest: Some(manifest_digest('4')),
            document_instruction_digest: Some(manifest_digest('5')),
            pooling: EmbeddingPoolingV1::Mean,
            truncation_side: EmbeddingTruncationSideV1::Right,
            truncation_length: 512,
            runtime_backend: "fastembed-ort".to_owned(),
            runtime_build_revision: "ort-test-rev-1".to_owned(),
            device_class: EmbeddingDeviceClassV1::Cpu,
            dimensions: 1,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            precision: EmbeddingPrecisionV1::Fp32,
            chunk_schema_revision: "code-search-chunk.v1".to_owned(),
            chunker_revision: id::<ChunkerRevision>("chunker.v1"),
            privacy_domain: id::<PrivacyDomainId>("privacy.project-a"),
            privacy_key_epoch: 7,
        }
        .admit()
        .expect("admitted embedding fixture")
    }

    fn admitted_embedding_for(
        privacy_domain: &str,
        privacy_key_epoch: u64,
        runtime_build_revision: &str,
    ) -> AdmittedEmbeddingProjectionKeyV1 {
        let mut key = admitted_embedding().embedding_key().clone();
        key.privacy_domain = id(privacy_domain);
        key.privacy_key_epoch = privacy_key_epoch;
        key.runtime_build_revision = runtime_build_revision.to_owned();
        key.admit().expect("admitted embedding fixture variant")
    }

    fn logical_generation(
        generation_digest: char,
        embedding_key: AdmittedEmbeddingProjectionKeyV1,
        source_generation: &str,
        source_manifest_digest: char,
        chunk_id: &str,
        chunk_digest: char,
        values: Vec<f32>,
    ) -> PublishedVectorGenerationV1 {
        let projection_key = embedding_key.projection_key().clone();
        let source_generation: CodeGenerationId = id(source_generation);
        let source_manifest_digest = manifest_digest(source_manifest_digest);
        let chunk_id: CodeSearchChunkId = id(chunk_id);
        let chunk_digest = content_digest(chunk_digest);
        let output_digest = tracedecay_semantic::projector::vector_output_digest(
            &projection_key,
            &chunk_id,
            &chunk_digest,
            &values,
        )
        .expect("canonical vector output digest");
        let vectors = BTreeMap::from([(
            chunk_id.clone(),
            ProjectedChunkVectorV1 {
                projection_key: projection_key.clone(),
                source_generation: source_generation.clone(),
                source_manifest_digest: source_manifest_digest.clone(),
                chunk_id: chunk_id.clone(),
                chunk_digest: chunk_digest.clone(),
                values,
                output_digest: output_digest.clone(),
            },
        )]);
        let plan = VectorGenerationPlanV1 {
            target_projection_key: projection_key.clone(),
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            expected_chunk_ids: vec![chunk_id.clone()],
            base_generation: None,
        };
        let manifest_digest =
            generation_identity_digest(&plan, &vectors, &BTreeMap::new()).expect("manifest digest");
        let generation_id = VectorGenerationIdV1::new(manifest_digest.clone());
        let request_digest = manifest_digest_for_test_request(generation_digest);
        let mut batch = ProjectionBatchReceiptV1 {
            target_projection_key: projection_key.clone(),
            request_digest: request_digest.clone(),
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            receipts: vec![tracedecay_domain::CodeChunkProjectionReceiptV1 {
                projection_key: projection_key.clone(),
                request_digest: request_digest.clone(),
                prior_generation: None,
                source_generation: source_generation.clone(),
                source_manifest_digest: source_manifest_digest.clone(),
                chunk_id,
                prior_chunk_digest: None,
                current_chunk_digest: Some(chunk_digest),
                operation: ProjectionOperationV1::Added,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(output_digest),
            }],
            reused_count: 0,
            publication_digest: manifest_digest_for_test_request('0'),
        };
        batch.publication_digest = expected_publication_digest(&batch).expect("publication digest");
        let publication_digest = batch.publication_digest.clone();
        PublishedVectorGenerationV1 {
            generation_id: generation_id.clone(),
            projection_key: projection_key.clone(),
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            base_generation: None,
            embedding_key,
            vectors,
            tombstones: Vec::new(),
            tombstone_digests: BTreeMap::new(),
            receipts: vec![batch],
            checkpoint: VectorProjectionCheckpointV1 {
                target_projection_key: projection_key,
                source_generation,
                source_manifest_digest,
                completed_batches: 1,
                last_request_digest: Some(request_digest),
                last_publication_digest: Some(publication_digest),
            },
            manifest_digest,
        }
    }

    fn manifest_digest_for_test_request(byte: char) -> ManifestDigest {
        manifest_digest(if byte.is_ascii_hexdigit() { byte } else { 'f' })
    }

    fn reused_prepared(
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        from_generation: &CodeGenerationId,
        to_generation: &CodeGenerationId,
        chunk_id: &CodeSearchChunkId,
        chunk_digest: &ContentDigest,
    ) -> PreparedVectorGenerationV1 {
        let mut changes = ChangedCodeChunkSetV1 {
            from_generation: Some(from_generation.clone()),
            to_generation: to_generation.clone(),
            manifest_digest: manifest_digest('0'),
            added_or_changed: vec![],
            deleted: vec![],
            reused: vec![ChangedCodeChunkV1 {
                chunk_id: chunk_id.clone(),
                prior_digest: Some(chunk_digest.clone()),
                current_digest: Some(chunk_digest.clone()),
            }],
        };
        changes.manifest_digest = changes.compute_digest().expect("changed-set digest");
        let mut request = ProjectionBatchRequestV1 {
            request_digest: manifest_digest('0'),
            changes,
            previous_projection_key: Some(embedding_key.projection_key().clone()),
            target_projection_key: embedding_key.projection_key().clone(),
            replay_reason: ProjectionReplayReasonV1::SourceEdit,
        };
        request.request_digest =
            tracedecay_code_index::projection::expected_request_digest(&request)
                .expect("projection request digest");
        let receipt = tracedecay_code_index::projection::build_batch_receipt(
            &request,
            &[
                tracedecay_code_index::projection::ChunkProjectionDecisionV1 {
                    chunk_id: chunk_id.clone(),
                    prior_chunk_digest: Some(chunk_digest.clone()),
                    current_chunk_digest: Some(chunk_digest.clone()),
                    operation: ProjectionOperationV1::Reused,
                    outcome: ProjectionOutcomeV1::Reused,
                    output_digest: None,
                },
            ],
        )
        .expect("reused projection receipt");
        PreparedVectorGenerationV1 {
            embedding_key: embedding_key.clone(),
            request,
            receipt,
            vectors: vec![],
            tombstones: vec![],
        }
    }

    fn insert_generation(
        store: &mut FakeVectorGenerationStoreV1,
        generation: PublishedVectorGenerationV1,
    ) -> VectorGenerationIdV1 {
        let generation_id = generation.generation_id().clone();
        intern_generation_vectors(
            &store.physical_vector_pool,
            &mut store.published,
            &generation,
        )
        .expect("intern generation vectors");
        store
            .published
            .generations
            .insert(generation_id.clone(), generation);
        generation_id
    }

    #[test]
    fn batch_watermark_and_base_generation_must_match_the_projection_request() {
        let embedding = admitted_embedding();
        let base = logical_generation(
            'a',
            embedding.clone(),
            "code-generation.base",
            'b',
            "chunk.v1.base",
            'c',
            vec![0.25],
        );
        let chunk_id = base.vectors.keys().next().expect("base chunk").clone();
        let chunk_digest = base
            .vectors
            .get(&chunk_id)
            .expect("base vector")
            .chunk_digest
            .clone();
        let base_id = base.generation_id().clone();
        let mut store = FakeVectorGenerationStoreV1::new();
        insert_generation(&mut store, base);
        let foreign_source = id("code-generation.foreign");
        let target_source = id("code-generation.target");
        let prepared = reused_prepared(
            &embedding,
            &foreign_source,
            &target_source,
            &chunk_id,
            &chunk_digest,
        );
        let build = store
            .begin_generation(VectorGenerationPlanV1 {
                target_projection_key: embedding.projection_key().clone(),
                source_generation: target_source.clone(),
                source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
                expected_chunk_ids: vec![chunk_id.clone()],
                base_generation: Some(base_id.clone()),
            })
            .expect("staged build");
        assert_eq!(
            store.commit_batch(&build, None, prepared.clone()),
            Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)
        );

        let mismatched_manifest = manifest_digest('f');
        let mismatched_build = store
            .begin_generation(VectorGenerationPlanV1 {
                target_projection_key: embedding.projection_key().clone(),
                source_generation: target_source,
                source_manifest_digest: mismatched_manifest,
                expected_chunk_ids: vec![chunk_id],
                base_generation: Some(base_id),
            })
            .expect("mismatched-watermark build");
        assert_eq!(
            store.commit_batch(&mismatched_build, None, prepared),
            Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch)
        );
    }

    #[test]
    fn successful_publication_consumes_the_staged_build() {
        let embedding = admitted_embedding();
        let base = logical_generation(
            'a',
            embedding.clone(),
            "code-generation.base",
            'b',
            "chunk.v1.base",
            'c',
            vec![0.25],
        );
        let chunk_id = base.vectors.keys().next().expect("base chunk").clone();
        let chunk_digest = base
            .vectors
            .get(&chunk_id)
            .expect("base vector")
            .chunk_digest
            .clone();
        let base_source = base.source_generation().clone();
        let base_id = base.generation_id().clone();
        let target_source = id("code-generation.target");
        let prepared = reused_prepared(
            &embedding,
            &base_source,
            &target_source,
            &chunk_id,
            &chunk_digest,
        );
        let mut store = FakeVectorGenerationStoreV1::new();
        insert_generation(&mut store, base);
        store.published.active_generation = Some(base_id.clone());
        let build = store
            .begin_generation(VectorGenerationPlanV1 {
                target_projection_key: embedding.projection_key().clone(),
                source_generation: target_source,
                source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
                expected_chunk_ids: vec![chunk_id],
                base_generation: Some(base_id.clone()),
            })
            .expect("staged build");
        store
            .commit_batch(&build, None, prepared)
            .expect("complete reused batch");
        let publication = store
            .publish_generation(&build, Some(&base_id))
            .expect("atomic publication");

        assert!(!store.staged.contains_key(&build));
        assert_eq!(
            store.active_generation_id(),
            Some(&publication.generation_id)
        );
        store
            .active_generation()
            .expect("current generation")
            .validate_persisted()
            .expect("current generation is complete");
    }

    #[tokio::test]
    async fn legacy_inventory_never_deserializes_vectors_and_quarantines_only_unreadable_entries() {
        let temporary = tempfile::tempdir().expect("temporary project database");
        let path = temporary.path().join("project.db");
        let authority =
            DatabaseAuthority::acquire_test(&path, "legacy vector migration").expect("authority");
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("database");
        let store = DatabaseVectorGenerationStoreV1::open_legacy_migration(&database)
            .await
            .expect("migration store");
        let readable = manifest_digest('a');
        let unreadable = manifest_digest('b');
        let source = "code-generation.legacy";
        let secret = "legacy-vector-secret";
        let generations = serde_json::Map::from_iter([
            (
                readable.as_str().to_owned(),
                serde_json::json!({
                    "generation_id": readable.as_str(),
                    "source_generation": source,
                    "vectors": [secret]
                }),
            ),
            (unreadable.as_str().to_owned(), serde_json::json!(secret)),
        ]);
        let state = serde_json::json!({
            "staged": {},
            "published": {
                "generations": generations,
                "active_generation": readable.as_str(),
                "legacy_migration_receipts": {},
                "physical_vector_bindings": {}
            }
        })
        .to_string();
        database
            .execute_write_engine(
                "install unreadable legacy vector fixture",
                "UPDATE semantic_vector_generation_state_v1
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1",
                params![state],
            )
            .await
            .expect("legacy fixture");

        let inventory = store
            .read_legacy_inventory()
            .await
            .expect("identity-only inventory");
        assert_eq!(inventory.inventory.entries.len(), 2);
        assert!(matches!(
            &inventory.inventory.entries[0],
            LegacyVectorInventoryEntryV1::Readable { .. }
        ));
        assert!(matches!(
            &inventory.inventory.entries[1],
            LegacyVectorInventoryEntryV1::Unreadable { .. }
        ));
        let offline_sources = retained_readable_sources_from_read_only_database(&path)
            .expect("read-only source inventory");
        assert_eq!(
            offline_sources,
            BTreeSet::from([id(source)]),
            "offline retention planning must use exactly the readable source set"
        );
        let mut rebuilder = ProductionLegacyVectorCanonicalRebuilderV1::try_new(
            Vec::new(),
            |_| -> Result<
                StagedCanonicalVectorRebuildV1,
                tracedecay_semantic::legacy_migration::LegacyVectorMigrationErrorV1,
            > { unreachable!("no retained generations") },
        )
        .expect("empty production rebuilder");
        let transaction = prepare_legacy_vector_migration(
            &inventory,
            &mut rebuilder,
            &NeverCancelLegacyVectorMigrationV1,
        )
        .expect("migration transaction");
        store
            .replace_legacy_vectors_atomically(
                &inventory,
                FakeVectorGenerationStoreV1::new(),
                &transaction,
            )
            .await
            .expect("atomic replacement");

        assert_eq!(
            database
                .query_scalar_text(
                    "inspect isolated legacy quarantine",
                    "SELECT generation_json
                     FROM semantic_legacy_vector_quarantine_v1",
                )
                .await
                .expect("quarantine row"),
            serde_json::to_string(secret).expect("secret JSON")
        );
        assert_eq!(
            database
                .query_scalar_i64(
                    "prove readable legacy vectors were dropped",
                    "SELECT COUNT(*)
                     FROM semantic_legacy_vector_quarantine_v1",
                )
                .await
                .expect("quarantine count"),
            1
        );
        assert_eq!(
            database
                .query_scalar_i64(
                    "prove legacy bytes left active state",
                    "SELECT instr(state_json, 'legacy-vector-secret')
                     FROM semantic_vector_generation_state_v1
                     WHERE singleton = 1",
                )
                .await
                .expect("active state inspection"),
            0
        );
        let committed_state = database
            .query_scalar_text(
                "capture committed vector state",
                "SELECT state_json
                 FROM semantic_vector_generation_state_v1
                 WHERE singleton = 1",
            )
            .await
            .expect("committed state");
        assert_eq!(
            store
                .replace_legacy_vectors_atomically(
                    &inventory,
                    FakeVectorGenerationStoreV1::new(),
                    &transaction,
                )
                .await,
            Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
        );
        assert_eq!(
            database
                .query_scalar_text(
                    "verify stale migration rollback",
                    "SELECT state_json
                     FROM semantic_vector_generation_state_v1
                     WHERE singleton = 1",
                )
                .await
                .expect("state after stale migration"),
            committed_state
        );
        DatabaseVectorGenerationStoreV1::open(&database)
            .await
            .expect("replacement state is runtime-readable");
    }

    #[tokio::test]
    async fn retained_canonical_rebuild_and_active_pointer_publish_together() {
        let temporary = tempfile::tempdir().expect("temporary project database");
        let path = temporary.path().join("project.db");
        let authority =
            DatabaseAuthority::acquire_test(&path, "canonical vector rebuild").expect("authority");
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("database");
        let store = DatabaseVectorGenerationStoreV1::open_legacy_migration(&database)
            .await
            .expect("migration store");
        let legacy = manifest_digest('a');
        let source: CodeGenerationId = id("code-generation.retained");
        let legacy_generations = serde_json::Map::from_iter([(
            legacy.as_str().to_owned(),
            serde_json::json!({
                "generation_id": legacy.as_str(),
                "source_generation": source.as_str(),
                "vectors": "legacy-bytes-must-not-be-used"
            }),
        )]);
        let legacy_state = serde_json::json!({
            "staged": {},
            "published": {
                "generations": legacy_generations,
                "active_generation": legacy.as_str(),
                "legacy_migration_receipts": {},
                "physical_vector_bindings": {}
            }
        })
        .to_string();
        database
            .execute_write_engine(
                "install readable legacy vector fixture",
                "UPDATE semantic_vector_generation_state_v1
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1",
                params![legacy_state],
            )
            .await
            .expect("legacy fixture");
        let inventory = store
            .read_legacy_inventory()
            .await
            .expect("legacy inventory");
        let retained = CanonicalEligibleChunkSetV1::try_from_chunks(
            source.clone(),
            vec![canonical_chunk("chunk.v1.retained", &source, 'd')],
        )
        .expect("retained canonical code");
        let mut replacement = FakeVectorGenerationStoreV1::new();
        let rebuilt = logical_generation(
            'c',
            admitted_embedding(),
            source.as_str(),
            '3',
            "chunk.v1.retained",
            'd',
            vec![0.5],
        );
        let rebuilt_id = insert_generation(&mut replacement, rebuilt);
        let rebuilt_for_callback = rebuilt_id.clone();
        let mut rebuilder = ProductionLegacyVectorCanonicalRebuilderV1::try_new(
            vec![retained],
            move |chunks: &CanonicalEligibleChunkSetV1| {
                Ok(StagedCanonicalVectorRebuildV1 {
                    source_generation: chunks.source_generation().clone(),
                    rebuilt_generation: rebuilt_for_callback.clone(),
                    canonical_chunk_set_digest: chunks.digest().clone(),
                })
            },
        )
        .expect("production rebuilder");
        let transaction = prepare_legacy_vector_migration(
            &inventory,
            &mut rebuilder,
            &NeverCancelLegacyVectorMigrationV1,
        )
        .expect("canonical rebuild transaction");

        let receipt = store
            .replace_legacy_vectors_atomically(&inventory, replacement, &transaction)
            .await
            .expect("atomic canonical rebuild publication");
        assert_eq!(
            store
                .completed_legacy_migration_receipt()
                .await
                .expect("completed migration receipt"),
            Some(receipt)
        );

        let reopened = DatabaseVectorGenerationStoreV1::open(&database)
            .await
            .expect("runtime store");
        assert_eq!(
            reopened
                .active_generation_id()
                .await
                .expect("active generation"),
            Some(rebuilt_id)
        );
        assert_eq!(
            database
                .query_scalar_i64(
                    "prove rebuild did not quarantine readable legacy bytes",
                    "SELECT COUNT(*)
                     FROM sqlite_schema
                     WHERE type = 'table'
                       AND name = 'semantic_legacy_vector_quarantine_v1'",
                )
                .await
                .expect("quarantine schema count"),
            0
        );
    }

    #[tokio::test]
    async fn request_read_ignores_corrupt_inactive_and_staged_generations() {
        let temporary = tempfile::tempdir().expect("temporary project database");
        let path = temporary.path().join("project.db");
        let authority = DatabaseAuthority::acquire_test(&path, "active vector request read")
            .expect("authority");
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("database");
        let _store = DatabaseVectorGenerationStoreV1::open_legacy_migration(&database)
            .await
            .expect("migration store");
        let embedding = admitted_embedding();
        let source: CodeGenerationId = id("code-generation.request-read");
        let source_manifest = manifest_digest('4');
        let active = logical_generation(
            'c',
            embedding.clone(),
            source.as_str(),
            '4',
            "chunk.v1.request-read",
            'd',
            vec![0.5],
        );
        let active_id = active.generation_id().clone();
        let mut state = FakeVectorGenerationStoreV1::new();
        insert_generation(&mut state, active);
        state.published.active_generation = Some(active_id.clone());
        let mut state_json = serde_json::to_value(&state).expect("vector state JSON");
        state_json["published"]["generations"][manifest_digest('e').as_str()] =
            serde_json::json!("corrupt-inactive-vector-bytes");
        state_json["staged"] = serde_json::json!({
            "corrupt-build": "corrupt-staged-vector-bytes"
        });
        database
            .execute_write_engine(
                "install inactive corruption fixture",
                "UPDATE semantic_vector_generation_state_v1
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1",
                params![state_json.to_string()],
            )
            .await
            .expect("corrupt inactive fixture");

        let observed = DatabaseVectorGenerationStoreV1::read_active_generation_for(
            &database,
            &embedding,
            &source,
            &source_manifest,
        )
        .await
        .expect("bounded active read")
        .expect("compatible active generation");
        assert_eq!(observed.generation_id(), &active_id);
        assert!(
            DatabaseVectorGenerationStoreV1::read_active_generation_snapshot_for(
                &database,
                &embedding,
                &source,
                &manifest_digest('5'),
            )
            .await
            .expect("wrong-manifest active read")
            .is_none(),
            "an active generation with the wrong source manifest must be denied"
        );
        assert!(
            DatabaseVectorGenerationStoreV1::open(&database)
                .await
                .is_err(),
            "full-state decoding would observe unrelated corruption"
        );
    }

    #[tokio::test]
    async fn native_evaluation_state_is_sqlite_backed_and_never_becomes_authoritative() {
        let temporary = tempfile::tempdir().expect("temporary project database");
        let path = temporary.path().join("project.db");
        let authority = DatabaseAuthority::acquire_test(&path, "native semantic evaluation")
            .expect("authority");
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("database");

        let evaluation =
            DatabaseVectorEvaluationStoreV1::open(&database, "semantic-native-evaluation:test")
                .await
                .expect("SQLite-backed evaluation store");
        assert_eq!(
            evaluation
                .active_generation_id()
                .await
                .expect("evaluation active generation"),
            None
        );
        assert_eq!(
            database
                .query_scalar_i64(
                    "inspect native evaluation row",
                    "SELECT COUNT(*) FROM semantic_vector_evaluation_state_v1",
                )
                .await
                .expect("evaluation row count"),
            1
        );
        assert_eq!(
            database
                .query_scalar_i64(
                    "prove native evaluation did not create authoritative state",
                    "SELECT COUNT(*)
                     FROM sqlite_schema
                     WHERE type = 'table'
                       AND name = 'semantic_vector_generation_state_v1'",
                )
                .await
                .expect("authoritative schema count"),
            0
        );

        evaluation.close().await.expect("remove evaluation row");
        assert_eq!(
            database
                .query_scalar_i64(
                    "verify native evaluation cleanup",
                    "SELECT COUNT(*) FROM semantic_vector_evaluation_state_v1",
                )
                .await
                .expect("evaluation row count after cleanup"),
            0
        );
    }

    #[test]
    fn active_pointer_cas_fault_restart_and_semantic_off_are_atomic() {
        let embedding = admitted_embedding();
        let first = logical_generation(
            'a',
            embedding.clone(),
            "code-generation.atomic-a",
            '1',
            "chunk.v1.atomic-a",
            'a',
            vec![0.25],
        );
        let second = logical_generation(
            'b',
            embedding,
            "code-generation.atomic-b",
            '2',
            "chunk.v1.atomic-b",
            'b',
            vec![0.75],
        );
        let mut store = FakeVectorGenerationStoreV1::new();
        let first_id = insert_generation(&mut store, first);
        let second_id = insert_generation(&mut store, second);
        store.published.active_generation = Some(first_id.clone());

        assert_eq!(
            store.activate_generation(&second_id, None),
            Err(VectorGenerationStoreErrorV1::StaleActiveGeneration)
        );
        assert_eq!(store.active_generation_id(), Some(&first_id));

        store.fail_before_publication_swap_once();
        assert_eq!(
            store.activate_generation(&second_id, Some(&first_id)),
            Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure)
        );
        assert_eq!(store.active_generation_id(), Some(&first_id));

        store
            .activate_generation(&second_id, Some(&first_id))
            .expect("activate replacement generation");
        assert_eq!(
            store.deactivate_generation(Some(&first_id)),
            Err(VectorGenerationStoreErrorV1::StaleActiveGeneration)
        );
        assert_eq!(store.active_generation_id(), Some(&second_id));
        let encoded = serde_json::to_string(&store).expect("serialize vector state");
        let mut restarted: FakeVectorGenerationStoreV1 =
            serde_json::from_str(&encoded).expect("deserialize vector state");
        restarted
            .ensure_physical_reuse_index()
            .expect("rebuild physical reuse index");
        validate_loaded_state(&restarted).expect("validate restarted vector state");
        assert_eq!(restarted.active_generation_id(), Some(&second_id));

        restarted.fail_before_publication_swap_once();
        assert_eq!(
            restarted.deactivate_generation(Some(&second_id)),
            Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure)
        );
        assert_eq!(restarted.active_generation_id(), Some(&second_id));

        restarted
            .deactivate_generation(Some(&second_id))
            .expect("disable semantic generation");
        assert_eq!(restarted.active_generation_id(), None);
        assert!(
            restarted.generation(&second_id).is_some(),
            "semantic-off retains the immutable generation for rollback"
        );
        restarted
            .activate_generation(&second_id, None)
            .expect("restore exact retained generation");
        assert_eq!(restarted.active_generation_id(), Some(&second_id));
    }

    #[test]
    fn cross_worktree_reuses_physical_bytes_without_reusing_logical_identity() {
        let embedding = admitted_embedding_for("privacy.reuse-regression-a", 7, "ort-test-rev-1");
        let first = logical_generation(
            'a',
            embedding.clone(),
            "code-generation.worktree-a",
            '1',
            "chunk.v1.worktree-a.alpha",
            'c',
            vec![0.25],
        );
        let second = logical_generation(
            'b',
            embedding.clone(),
            "code-generation.worktree-b",
            '2',
            "chunk.v1.worktree-b.alpha",
            'c',
            vec![0.25],
        );
        let first_chunk = first.vectors.keys().next().unwrap().clone();
        let second_chunk = second.vectors.keys().next().unwrap().clone();
        let first_generation = first.generation_id().clone();
        let second_generation = second.generation_id().clone();
        let mut first_store = FakeVectorGenerationStoreV1::new();
        let mut second_store = FakeVectorGenerationStoreV1::new();

        intern_generation_vectors(
            &first_store.physical_vector_pool,
            &mut first_store.published,
            &first,
        )
        .unwrap();
        first_store
            .published
            .generations
            .insert(first_generation.clone(), first.clone());
        first_store.published.active_generation = Some(first_generation.clone());
        intern_generation_vectors(
            &second_store.physical_vector_pool,
            &mut second_store.published,
            &second,
        )
        .unwrap();
        second_store
            .published
            .generations
            .insert(second_generation.clone(), second.clone());
        second_store.published.active_generation = Some(second_generation.clone());

        let first_values = first_store
            .physical_vector_values(&first_generation, &first_chunk)
            .unwrap();
        let second_values = second_store
            .physical_vector_values(&second_generation, &second_chunk)
            .unwrap();
        assert!(Arc::ptr_eq(&first_values, &second_values));
        assert_eq!(first_store.published.physical_vectors.len(), 1);
        assert_eq!(second_store.published.physical_vectors.len(), 1);
        assert_ne!(first_generation, second_generation);
        assert_ne!(first.source_generation(), second.source_generation());
        assert_ne!(first_chunk, second_chunk);
        assert_ne!(first.receipts(), second.receipts());
        assert_eq!(first_store.active_generation_id(), Some(&first_generation));
        assert_eq!(
            second_store.active_generation_id(),
            Some(&second_generation),
            "each worktree retains its own active pointer"
        );

        for (generation_digest, embedding_key) in [
            (
                'd',
                admitted_embedding_for("privacy.reuse-regression-b", 7, "ort-test-rev-1"),
            ),
            (
                'e',
                admitted_embedding_for("privacy.reuse-regression-a", 8, "ort-test-rev-1"),
            ),
            (
                'f',
                admitted_embedding_for("privacy.reuse-regression-a", 7, "ort-test-rev-2"),
            ),
        ] {
            let isolated = logical_generation(
                generation_digest,
                embedding_key,
                &format!("code-generation.isolated-{generation_digest}"),
                generation_digest,
                &format!("chunk.v1.isolated-{generation_digest}.alpha"),
                'c',
                vec![0.25],
            );
            intern_generation_vectors(
                &second_store.physical_vector_pool,
                &mut second_store.published,
                &isolated,
            )
            .unwrap();
            second_store
                .published
                .generations
                .insert(isolated.generation_id().clone(), isolated);
        }
        assert_eq!(
            second_store.published.physical_vectors.len(),
            4,
            "privacy domain, key epoch, and any projection-key input isolate physical bytes"
        );

        let edited_second = logical_generation(
            '9',
            embedding.clone(),
            "code-generation.worktree-b-edited",
            '9',
            "chunk.v1.worktree-b.alpha-edited",
            '9',
            vec![0.75],
        );
        let edited_generation = edited_second.generation_id().clone();
        let edited_chunk = edited_second.vectors.keys().next().unwrap().clone();
        intern_generation_vectors(
            &second_store.physical_vector_pool,
            &mut second_store.published,
            &edited_second,
        )
        .unwrap();
        second_store
            .published
            .generations
            .insert(edited_generation.clone(), edited_second);
        assert_eq!(second_store.published.physical_vectors.len(), 5);
        assert!(!Arc::ptr_eq(
            &second_values,
            &second_store
                .physical_vector_values(&edited_generation, &edited_chunk)
                .unwrap()
        ));
        assert!(Arc::ptr_eq(
            &first_values,
            &second_store
                .physical_vector_values(&second_generation, &second_chunk)
                .unwrap()
        ));
        assert!(Arc::ptr_eq(
            &first_values,
            &first_store
                .physical_vector_values(&first_generation, &first_chunk)
                .unwrap()
        ));
        assert_eq!(first_store.active_generation_id(), Some(&first_generation));
        assert_eq!(
            second_store.active_generation_id(),
            Some(&second_generation)
        );

        let conflicting = logical_generation(
            '8',
            embedding,
            "code-generation.worktree-c",
            '8',
            "chunk.v1.worktree-c.alpha",
            'c',
            vec![0.5],
        );
        assert_eq!(
            intern_generation_vectors(
                &second_store.physical_vector_pool,
                &mut second_store.published,
                &conflicting,
            ),
            Err(VectorGenerationStoreErrorV1::PhysicalVectorConflict)
        );
    }

    #[test]
    fn generation_identity_ignores_batch_execution_history() {
        let embedding_key = admitted_embedding();
        let projection_key = embedding_key.projection_key().clone();
        let source_generation = id::<CodeGenerationId>("code-generation.1");
        let source_manifest_digest = manifest_digest('b');
        let chunk_id = id::<CodeSearchChunkId>("chunk.v1.alpha");
        let plan = VectorGenerationPlanV1 {
            target_projection_key: projection_key.clone(),
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            expected_chunk_ids: vec![chunk_id.clone()],
            base_generation: None,
        };
        let vectors = BTreeMap::from([(
            chunk_id.clone(),
            ProjectedChunkVectorV1 {
                projection_key: projection_key.clone(),
                source_generation: source_generation.clone(),
                source_manifest_digest: source_manifest_digest.clone(),
                chunk_id,
                chunk_digest: content_digest('c'),
                values: vec![0.25],
                // Identity tests compare digest bytes, not recomputed projector validity.
                output_digest: content_digest('d'),
            },
        )]);
        let tombstones = BTreeMap::new();

        let first = generation_identity_digest(&plan, &vectors, &tombstones)
            .expect("identity from vector content");
        let second = generation_identity_digest(&plan, &vectors, &tombstones)
            .expect("identity remains independent from receipt/checkpoint batching");

        assert_eq!(first, second);

        let checkpoint = VectorProjectionCheckpointV1 {
            target_projection_key: plan.target_projection_key.clone(),
            source_generation: plan.source_generation.clone(),
            source_manifest_digest: plan.source_manifest_digest.clone(),
            completed_batches: 1,
            last_request_digest: Some(manifest_digest('e')),
            last_publication_digest: Some(manifest_digest('f')),
        };
        let published = PublishedVectorGenerationV1 {
            generation_id: VectorGenerationIdV1::new(first.clone()),
            projection_key: plan.target_projection_key.clone(),
            source_generation: plan.source_generation.clone(),
            source_manifest_digest: plan.source_manifest_digest.clone(),
            base_generation: None,
            embedding_key,
            vectors: vectors.clone(),
            tombstones: vec![],
            tombstone_digests: BTreeMap::new(),
            receipts: vec![],
            checkpoint,
            manifest_digest: first,
        };
        let mut replayed = published.clone();
        replayed.checkpoint.completed_batches = 2;
        replayed.checkpoint.last_request_digest = Some(manifest_digest('0'));
        replayed.checkpoint.last_publication_digest = Some(manifest_digest('1'));

        assert_ne!(published.checkpoint, replayed.checkpoint);
        assert!(
            published.same_vector_content(&replayed),
            "execution checkpoint history does not redefine immutable vector content"
        );
        let mut rebuilt_from_another_base = published.clone();
        rebuilt_from_another_base.base_generation =
            Some(VectorGenerationIdV1::new(manifest_digest('9')));
        assert!(
            published.same_vector_content(&rebuilt_from_another_base),
            "execution lineage does not redefine identical immutable vector content"
        );

        let encoded = serde_json::to_string(&published).expect("serialize published generation");
        let decoded: PublishedVectorGenerationV1 =
            serde_json::from_str(&encoded).expect("deserialize published generation");
        assert!(published.same_vector_content(&decoded));
        assert_eq!(decoded.tombstones(), published.tombstones());
        assert_eq!(decoded.tombstone_digests(), published.tombstone_digests());
        assert_eq!(decoded.base_generation(), published.base_generation());
        assert_eq!(decoded.embedding_key(), published.embedding_key());
    }

    #[test]
    fn persisted_state_rejects_tombstone_vector_overlap_and_dangling_active() {
        let embedding_key = admitted_embedding();
        let projection_key = embedding_key.projection_key().clone();
        let chunk_id = id::<CodeSearchChunkId>("chunk.v1.alpha");
        let generation_id = VectorGenerationIdV1::new(manifest_digest('a'));
        let mut generation = PublishedVectorGenerationV1 {
            generation_id: generation_id.clone(),
            projection_key: projection_key.clone(),
            source_generation: id("code-generation.1"),
            source_manifest_digest: manifest_digest('b'),
            base_generation: None,
            embedding_key: embedding_key.clone(),
            vectors: BTreeMap::from([(
                chunk_id.clone(),
                ProjectedChunkVectorV1 {
                    projection_key,
                    source_generation: id("code-generation.1"),
                    source_manifest_digest: manifest_digest('b'),
                    chunk_id: chunk_id.clone(),
                    chunk_digest: content_digest('c'),
                    values: vec![1.0],
                    output_digest: content_digest('d'),
                },
            )]),
            tombstones: vec![chunk_id.clone()],
            tombstone_digests: BTreeMap::from([(chunk_id, content_digest('c'))]),
            receipts: vec![],
            checkpoint: VectorProjectionCheckpointV1 {
                target_projection_key: embedding_key.projection_key().clone(),
                source_generation: id("code-generation.1"),
                source_manifest_digest: manifest_digest('b'),
                completed_batches: 1,
                last_request_digest: None,
                last_publication_digest: None,
            },
            manifest_digest: generation_id.as_digest().clone(),
        };
        assert!(generation.validate_persisted().is_err());

        generation.vectors.clear();
        generation.canonicalize_tombstones();
        let request_digest = manifest_digest('e');
        let mut deletion_batch = ProjectionBatchReceiptV1 {
            target_projection_key: generation.projection_key.clone(),
            request_digest: request_digest.clone(),
            source_generation: generation.source_generation.clone(),
            source_manifest_digest: generation.source_manifest_digest.clone(),
            receipts: vec![tracedecay_domain::CodeChunkProjectionReceiptV1 {
                projection_key: generation.projection_key.clone(),
                request_digest: request_digest.clone(),
                prior_generation: Some(id("code-generation.0")),
                source_generation: generation.source_generation.clone(),
                source_manifest_digest: generation.source_manifest_digest.clone(),
                chunk_id: generation.tombstones[0].clone(),
                prior_chunk_digest: generation
                    .tombstone_digests
                    .get(&generation.tombstones[0])
                    .cloned(),
                current_chunk_digest: None,
                operation: ProjectionOperationV1::Deleted,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: None,
            }],
            reused_count: 0,
            publication_digest: manifest_digest('f'),
        };
        deletion_batch.publication_digest =
            expected_publication_digest(&deletion_batch).expect("deletion publication digest");
        generation.checkpoint.last_request_digest = Some(request_digest);
        generation.checkpoint.last_publication_digest =
            Some(deletion_batch.publication_digest.clone());
        generation.receipts = vec![deletion_batch];
        generation.manifest_digest = generation_identity_digest(
            &VectorGenerationPlanV1 {
                target_projection_key: generation.projection_key.clone(),
                source_generation: generation.source_generation.clone(),
                source_manifest_digest: generation.source_manifest_digest.clone(),
                expected_chunk_ids: vec![],
                base_generation: None,
            },
            &generation.vectors,
            &generation.tombstone_digests,
        )
        .expect("tombstone generation manifest");
        generation.generation_id = VectorGenerationIdV1::new(generation.manifest_digest.clone());
        assert!(generation.validate_persisted().is_ok());

        let mut state = FakeVectorGenerationStoreV1::default();
        state.published.active_generation = Some(VectorGenerationIdV1::new(manifest_digest('9')));
        assert!(validate_loaded_state(&state).is_err());
    }

    #[test]
    fn persisted_generation_recomputes_immutable_manifest_content() {
        let mut generation = logical_generation(
            'a',
            admitted_embedding(),
            "code-generation.manifest-integrity",
            'b',
            "chunk.v1.manifest-integrity",
            'c',
            vec![0.25],
        );
        generation
            .validate_persisted()
            .expect("canonical generation");
        let vector = generation
            .vectors
            .values_mut()
            .next()
            .expect("fixture vector");
        vector.values = vec![0.75];
        vector.output_digest = tracedecay_semantic::projector::vector_output_digest(
            &vector.projection_key,
            &vector.chunk_id,
            &vector.chunk_digest,
            &vector.values,
        )
        .expect("tampered vector digest");
        generation.receipts[0].receipts[0].output_digest = Some(vector.output_digest.clone());
        generation.receipts[0].publication_digest =
            expected_publication_digest(&generation.receipts[0])
                .expect("tampered publication digest");
        generation.checkpoint.last_publication_digest =
            Some(generation.receipts[0].publication_digest.clone());

        assert!(
            generation.validate_persisted().is_err(),
            "self-consistent vector/receipt tampering must not retain the immutable generation id"
        );
    }
}
