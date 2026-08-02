#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PublishedVectorGenerationV1 {
    generation_id: VectorGenerationIdV1,
    projection_key: ProjectionKeyV1,
    source_generation: CodeGenerationId,
    source_manifest_digest: ManifestDigest,
    base_generation: Option<VectorGenerationIdV1>,
    embedding_key: AdmittedEmbeddingProjectionKeyV1,
    #[serde(with = "external_state")]
    vectors: ExternalV1<VectorRowMapV1>,
    #[serde(with = "external_state")]
    tombstones: ExternalV1<Vec<CodeSearchChunkId>>,
    #[serde(with = "external_state")]
    tombstone_digests: ExternalV1<BTreeMap<CodeSearchChunkId, ContentDigest>>,
    #[serde(with = "external_state")]
    receipts: ExternalV1<Vec<ProjectionBatchReceiptV1>>,
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

    pub(crate) fn into_vectors(self) -> BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1> {
        self.vectors.value.0
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

    #[cfg(test)]
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
        if *self.tombstones != canonical_tombstones {
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
    #[error("legacy vector generation singleton is not the exact empty state")]
    NonemptyLegacySingleton,
    #[error("vector generation active pointer belongs to another store shard")]
    ShardIdentityMismatch,
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
    #[serde(with = "external_state::plan")]
    plan: VectorGenerationPlanV1,
    embedding_key: Option<AdmittedEmbeddingProjectionKeyV1>,
    #[serde(with = "external_state")]
    vectors: ExternalV1<VectorRowMapV1>,
    #[serde(with = "external_state")]
    tombstones: ExternalV1<BTreeMap<CodeSearchChunkId, ContentDigest>>,
    #[serde(with = "external_state")]
    tombstone_ids: ExternalV1<Vec<CodeSearchChunkId>>,
    #[serde(with = "external_state")]
    batches: ExternalV1<PreparedBatchesV1>,
    #[serde(with = "external_state")]
    receipts: ExternalV1<Vec<ProjectionBatchReceiptV1>>,
    #[serde(with = "external_state")]
    committed_chunk_effects: ExternalV1<BTreeSet<CodeSearchChunkId>>,
    checkpoint: VectorProjectionCheckpointV1,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishedStateV1 {
    generations: BTreeMap<VectorGenerationIdV1, PublishedVectorGenerationV1>,
    active_generation: Option<VectorGenerationIdV1>,
    #[serde(skip, default)]
    physical_vectors: BTreeMap<ManifestDigest, PhysicalVectorPayloadV1>,
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
