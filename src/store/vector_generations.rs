//! Quarantined Plan 02 vector-generation store preparation.
//!
//! The in-memory store proves the persistence contract before a physical
//! schema is selected: projection effects and checkpoints commit together,
//! partial generations remain unqueryable, and immutable generation records
//! plus the active pointer publish in one atomic state swap.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkId, ContentDigest, EmbeddingProjectionKeyV1, ManifestDigest,
    ProjectionBatchReceiptV1, ProjectionKeyV1, ProjectionKindV1, ProjectionOperationV1,
    ProjectionOutcomeV1, canonical_sha256,
};

use crate::code_index::projection::verify_batch_receipt;
use crate::semantic_code::projector::{
    PreparedVectorGenerationV1, ProjectedChunkVectorV1, SemanticProjectionErrorV1,
};

const VECTOR_GENERATION_BUILD_DIGEST_DOMAIN: &str = "tracedecay.vector-generation-build.v1";
const VECTOR_GENERATION_MANIFEST_DIGEST_DOMAIN: &str = "tracedecay.vector-generation-manifest.v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct VectorGenerationIdV1(ManifestDigest);

impl VectorGenerationIdV1 {
    pub fn as_digest(&self) -> &ManifestDigest {
        &self.0
    }
}

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

#[derive(Clone, Debug, PartialEq)]
pub struct PublishedVectorGenerationV1 {
    generation_id: VectorGenerationIdV1,
    projection_key: ProjectionKeyV1,
    source_generation: CodeGenerationId,
    source_manifest_digest: ManifestDigest,
    vectors: BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>,
    tombstones: Vec<CodeSearchChunkId>,
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

    pub fn vectors(&self) -> &BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1> {
        &self.vectors
    }

    pub fn tombstones(&self) -> &[CodeSearchChunkId] {
        &self.tombstones
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    #[error("injected failure before atomic publication swap")]
    InjectedPublicationFailure,
    #[error("semantic projector handoff rejected: {0}")]
    Projection(#[from] SemanticProjectionErrorV1),
}

#[derive(Clone)]
struct StagedVectorGenerationV1 {
    plan: VectorGenerationPlanV1,
    embedding_key: Option<EmbeddingProjectionKeyV1>,
    vectors: BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>,
    tombstones: BTreeMap<CodeSearchChunkId, ContentDigest>,
    batches: Vec<PreparedVectorGenerationV1>,
    committed_chunk_effects: BTreeSet<CodeSearchChunkId>,
    checkpoint: VectorProjectionCheckpointV1,
}

#[derive(Clone, Default)]
struct PublishedStateV1 {
    generations: BTreeMap<VectorGenerationIdV1, PublishedVectorGenerationV1>,
    active_generation: Option<VectorGenerationIdV1>,
}

/// Deterministic fake store used to lock the Plan 02 publication interface.
/// It performs no I/O and is not wired into production.
#[derive(Clone, Default)]
pub struct FakeVectorGenerationStoreV1 {
    staged: BTreeMap<VectorGenerationBuildIdV1, StagedVectorGenerationV1>,
    published: PublishedStateV1,
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
                    validate_vector_row(&next.plan, &prepared.embedding_key, vector)?;
                    if receipt.outcome != ProjectionOutcomeV1::Applied
                        || receipt.output_digest.as_ref() != Some(&vector.output_digest)
                    {
                        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
                    }
                    next.tombstones.remove(&receipt.chunk_id);
                    next.vectors
                        .insert(receipt.chunk_id.clone(), (*vector).clone());
                }
                ProjectionOperationV1::Deleted => {
                    let tombstone = tombstone_by_chunk
                        .get(&receipt.chunk_id)
                        .ok_or_else(|| VectorGenerationStoreErrorV1::BatchIdentityMismatch)?;
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
            .as_ref()
            .ok_or(VectorGenerationStoreErrorV1::IncompleteGeneration)?;
        for vector in staged.vectors.values() {
            validate_vector_row(&staged.plan, embedding_key, vector)?;
        }

        let vector_digests = staged
            .vectors
            .values()
            .map(|vector| (&vector.chunk_id, &vector.output_digest))
            .collect::<Vec<_>>();
        let tombstone_ids = staged.tombstones.keys().collect::<Vec<_>>();
        let receipt_digests = staged
            .batches
            .iter()
            .map(|batch| &batch.receipt.publication_digest)
            .collect::<Vec<_>>();
        let manifest_digest = canonical_sha256(&(
            VECTOR_GENERATION_MANIFEST_DIGEST_DOMAIN,
            &staged.plan,
            vector_digests,
            tombstone_ids,
            receipt_digests,
            &staged.checkpoint,
        ))
        .map_err(|error| VectorGenerationStoreErrorV1::InvalidPlan(error.to_string()))?;
        let generation_id = VectorGenerationIdV1(manifest_digest.clone());
        let generation = PublishedVectorGenerationV1 {
            generation_id: generation_id.clone(),
            projection_key: staged.plan.target_projection_key,
            source_generation: staged.plan.source_generation,
            source_manifest_digest: staged.plan.source_manifest_digest,
            vectors: staged.vectors,
            tombstones: staged.tombstones.into_keys().collect(),
            receipts: staged
                .batches
                .into_iter()
                .map(|batch| batch.receipt)
                .collect(),
            checkpoint: staged.checkpoint.clone(),
            manifest_digest: manifest_digest.clone(),
        };
        let mut next = self.published.clone();
        if let Some(existing) = next.generations.get(&generation_id) {
            if existing != &generation {
                return Err(VectorGenerationStoreErrorV1::ImmutableGenerationConflict);
            }
        } else {
            next.generations.insert(generation_id.clone(), generation);
        }
        next.active_generation = Some(generation_id.clone());
        if self.fail_before_publication_swap {
            self.fail_before_publication_swap = false;
            return Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure);
        }
        self.published = next;
        Ok(VectorGenerationPublicationV1 {
            generation_id,
            manifest_digest,
            checkpoint: staged.checkpoint,
        })
    }

    pub fn active_generation_id(&self) -> Option<&VectorGenerationIdV1> {
        self.published.active_generation.as_ref()
    }

    pub fn active_checkpoint(&self) -> Option<&VectorProjectionCheckpointV1> {
        self.active_generation()
            .map(PublishedVectorGenerationV1::checkpoint)
    }

    pub fn active_generation(&self) -> Option<&PublishedVectorGenerationV1> {
        self.active_generation_id()
            .and_then(|id| self.published.generations.get(id))
    }

    pub fn generation(
        &self,
        generation_id: &VectorGenerationIdV1,
    ) -> Option<&PublishedVectorGenerationV1> {
        self.published.generations.get(generation_id)
    }

    #[cfg(test)]
    pub fn fail_before_publication_swap_once(&mut self) {
        self.fail_before_publication_swap = true;
    }
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
        || prepared.request.changes.manifest_digest != plan.source_manifest_digest
        || prepared.receipt.source_manifest_digest != plan.source_manifest_digest
    {
        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
    }
    let semantic_key = prepared
        .embedding_key
        .projection_key()
        .map_err(|error| VectorGenerationStoreErrorV1::InvalidPlan(error.to_string()))?;
    if semantic_key != plan.target_projection_key {
        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
    }
    Ok(())
}

fn validate_vector_row(
    plan: &VectorGenerationPlanV1,
    embedding_key: &EmbeddingProjectionKeyV1,
    vector: &ProjectedChunkVectorV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if vector.projection_key != plan.target_projection_key
        || vector.source_generation != plan.source_generation
        || vector.source_manifest_digest != plan.source_manifest_digest
    {
        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
    }
    vector.validate(embedding_key.dimensions)?;
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
