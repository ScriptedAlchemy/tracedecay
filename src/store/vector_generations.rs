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
    sync::{Arc, Mutex, Weak},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeGenerationId, CodeSearchChunkId, ContentDigest,
    ManifestDigest, ProjectionBatchReceiptV1, ProjectionKeyV1, ProjectionKindV1,
    ProjectionOperationV1, ProjectionOutcomeV1, canonical_sha256,
};

pub use tracedecay_domain::VectorGenerationIdV1;

use crate::code_index::projection::verify_batch_receipt;
use crate::db::{Database, engine::params};
use crate::semantic_code::projector::{
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
            && self.base_generation == other.base_generation
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
        if self.generation_id.as_digest() != &self.manifest_digest {
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
    /// identity and restart projection from its authoritative PR9 inputs.
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
        Ok(VectorGenerationPublicationV1 {
            generation_id,
            manifest_digest,
            checkpoint,
        })
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
        self.published.active_generation = Some(generation_id.clone());
        Ok(publication)
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

impl<'database> DatabaseVectorGenerationStoreV1<'database> {
    pub async fn open(database: &'database Database) -> Result<Self, VectorGenerationStoreErrorV1> {
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
        let store = Self { database };
        store.load_state().await?;
        Ok(store)
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

fn storage_error(error: impl std::fmt::Display) -> VectorGenerationStoreErrorV1 {
    VectorGenerationStoreErrorV1::Storage(error.to_string())
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
        ChunkerRevision, EmbeddingDeviceClassV1, EmbeddingMetricV1, EmbeddingNormalizationV1,
        EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingProjectionKeyV1,
        EmbeddingTruncationSideV1, PrivacyDomainId,
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
        let generation_id = VectorGenerationIdV1::new(manifest_digest(generation_digest));
        let projection_key = embedding_key.projection_key().clone();
        let source_generation: CodeGenerationId = id(source_generation);
        let source_manifest_byte = source_manifest_digest;
        let source_manifest_digest = manifest_digest(source_manifest_digest);
        let chunk_id: CodeSearchChunkId = id(chunk_id);
        PublishedVectorGenerationV1 {
            generation_id: generation_id.clone(),
            projection_key: projection_key.clone(),
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            base_generation: None,
            embedding_key,
            vectors: BTreeMap::from([(
                chunk_id.clone(),
                ProjectedChunkVectorV1 {
                    projection_key: projection_key.clone(),
                    source_generation: source_generation.clone(),
                    source_manifest_digest: source_manifest_digest.clone(),
                    chunk_id,
                    chunk_digest: content_digest(chunk_digest),
                    values,
                    // Physical reuse is keyed independently from the
                    // occurrence-bound projector receipt digest.
                    output_digest: content_digest(generation_digest),
                },
            )]),
            tombstones: Vec::new(),
            tombstone_digests: BTreeMap::new(),
            receipts: vec![ProjectionBatchReceiptV1 {
                target_projection_key: projection_key.clone(),
                request_digest: manifest_digest(generation_digest),
                source_generation: source_generation.clone(),
                source_manifest_digest: source_manifest_digest.clone(),
                receipts: Vec::new(),
                reused_count: 0,
                publication_digest: manifest_digest(source_manifest_byte),
            }],
            checkpoint: VectorProjectionCheckpointV1 {
                target_projection_key: projection_key,
                source_generation,
                source_manifest_digest,
                completed_batches: 1,
                last_request_digest: None,
                last_publication_digest: None,
            },
            manifest_digest: generation_id.as_digest().clone(),
        }
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
        // Empty vector sets remain valid when tombstones and embedding metadata
        // stay canonical; digest-valid rows are checked only when present.
        assert!(generation.validate_persisted().is_ok());

        let mut state = FakeVectorGenerationStoreV1::default();
        state.published.active_generation = Some(VectorGenerationIdV1::new(manifest_digest('9')));
        assert!(validate_loaded_state(&state).is_err());
    }
}
