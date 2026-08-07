use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::VectorGenerationIdV1;

use super::super::{
    CodeShardScopeV1, GraphProjectionIdentityV1, GraphPublicationKeyV1, GraphPublicationReplayV1,
    GraphRecoveredGenerationDigestV1, GraphVerifiedHeadV1, StorageRuntimeContractErrorV1,
    StoreRuntimeBindingV1, StoreShardIdV1, StoreShardScopeV1,
};

pub const MAX_SEMANTIC_VECTOR_STAGE_CHUNKS: u64 = 100_000;
pub const MAX_SEMANTIC_VECTOR_STAGE_CHUNKS_PER_BATCH: usize = 512;
pub const MAX_SEMANTIC_VECTOR_STAGE_PAGE_RECORDS: u16 = 64;
pub const MAX_SEMANTIC_VECTOR_PENDING_EFFECT_PAGE_RECORDS: u16 = 64;
pub const MAX_SEMANTIC_VECTOR_EMBEDDING_DIMENSION: u16 = 4_096;

macro_rules! canonical_id {
    ($name:ident, $field:literal) => {
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, StorageRuntimeContractErrorV1> {
                let value = value.into();
                super::super::identity::validate_canonical_id(&value, $field, 512)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<String> for $name {
            type Error = StorageRuntimeContractErrorV1;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

macro_rules! sha256_digest {
    ($name:ident, $field:literal) => {
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, StorageRuntimeContractErrorV1> {
                let value = value.into();
                validate_sha256(&value, $field)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<String> for $name {
            type Error = StorageRuntimeContractErrorV1;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }
    };
}

fn validate_sha256(value: &str, field: &'static str) -> Result<(), StorageRuntimeContractErrorV1> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(StorageRuntimeContractErrorV1::NonCanonical { field });
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StorageRuntimeContractErrorV1::NonCanonical { field });
    }
    Ok(())
}

canonical_id!(SemanticVectorBuildId, "semantic vector build id");
canonical_id!(
    SemanticVectorSourceGenerationId,
    "semantic vector source generation id"
);
canonical_id!(SemanticVectorChunkId, "semantic vector chunk id");

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct SemanticVectorCodeScopeHash(String);

impl SemanticVectorCodeScopeHash {
    pub fn new(value: impl Into<String>) -> Result<Self, StorageRuntimeContractErrorV1> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(StorageRuntimeContractErrorV1::NonCanonical {
                field: "semantic vector code scope hash",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SemanticVectorCodeScopeHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

sha256_digest!(SemanticVectorPlanDigest, "semantic vector plan digest");
sha256_digest!(
    SemanticVectorSourceManifestDigest,
    "semantic vector source manifest digest"
);
sha256_digest!(
    SemanticEmbeddingProjectionDigestV1,
    "semantic embedding projection digest"
);
sha256_digest!(
    SemanticModelArtifactDigestV1,
    "semantic model artifact digest"
);
sha256_digest!(
    SemanticProjectionManifestDigestV1,
    "semantic projection manifest digest"
);
sha256_digest!(
    SemanticPrivacyDomainDigestV1,
    "semantic privacy domain digest"
);
sha256_digest!(
    SemanticVectorChunkManifestDigest,
    "semantic vector chunk manifest digest"
);
sha256_digest!(SemanticVectorChunkDigest, "semantic vector chunk digest");
sha256_digest!(SemanticVectorOutputDigest, "semantic vector output digest");
sha256_digest!(
    SemanticVectorBatchInputDigest,
    "semantic vector batch input digest"
);
sha256_digest!(
    SemanticVectorBatchOutputDigest,
    "semantic vector batch output digest"
);
sha256_digest!(
    SemanticVectorBatchReceiptDigest,
    "semantic vector batch receipt digest"
);
sha256_digest!(
    SemanticVectorCheckpointDigest,
    "semantic vector checkpoint digest"
);
sha256_digest!(
    SemanticVectorPublicationIntentDigest,
    "semantic vector publication intent digest"
);
sha256_digest!(
    SemanticVectorEffectFailureDigest,
    "semantic vector effect failure digest"
);
sha256_digest!(
    SemanticVectorGraphBatchDigest,
    "semantic vector graph batch digest"
);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStageKey {
    pub projection: GraphProjectionIdentityV1,
    pub build_id: SemanticVectorBuildId,
    pub plan_digest: SemanticVectorPlanDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorWriterFence {
    pub binding: StoreRuntimeBindingV1,
}

impl SemanticVectorWriterFence {
    pub fn validate_for(
        &self,
        projection: &GraphProjectionIdentityV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.binding.shard_id != projection.shard_id {
            return Err(StorageRuntimeContractErrorV1::ShardMismatch {
                field: "semantic vector writer fence",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorReconstructionRecipe {
    pub source_manifest_digest: SemanticVectorSourceManifestDigest,
    pub embedding_projection_digest: SemanticEmbeddingProjectionDigestV1,
    pub embedding_dimension: u16,
    pub model_artifact_digest: SemanticModelArtifactDigestV1,
    pub projection_manifest_digest: SemanticProjectionManifestDigestV1,
    pub privacy_domain_digest: SemanticPrivacyDomainDigestV1,
    pub privacy_key_epoch: u64,
    pub expected_chunk_manifest_digest: SemanticVectorChunkManifestDigest,
}

impl SemanticVectorReconstructionRecipe {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.embedding_dimension == 0
            || self.embedding_dimension > MAX_SEMANTIC_VECTOR_EMBEDDING_DIMENSION
        {
            return Err(StorageRuntimeContractErrorV1::InvalidRange {
                field: "semantic vector embedding dimension",
                min: 1,
                max: u64::from(MAX_SEMANTIC_VECTOR_EMBEDDING_DIMENSION),
            });
        }
        if self.privacy_key_epoch == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "semantic vector privacy key epoch",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStagePlan {
    pub key: SemanticVectorStageKey,
    pub semantic_generation_id: VectorGenerationIdV1,
    pub base_generation: Option<VectorGenerationIdV1>,
    pub publication_key: GraphPublicationKeyV1,
    pub source_scope: StoreShardIdV1,
    pub code_scope_hash: SemanticVectorCodeScopeHash,
    pub source_generation: SemanticVectorSourceGenerationId,
    pub source_dependency: super::SemanticVectorSourceDependencyV1,
    pub recipe: SemanticVectorReconstructionRecipe,
    pub expected_chunk_count: u64,
    pub expected_prior_verified_head: Option<GraphVerifiedHeadV1>,
    pub initial_checkpoint_digest: SemanticVectorCheckpointDigest,
    pub writer_fence: SemanticVectorWriterFence,
}

impl SemanticVectorStagePlan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        projection: GraphProjectionIdentityV1,
        build_id: SemanticVectorBuildId,
        semantic_generation_id: VectorGenerationIdV1,
        base_generation: Option<VectorGenerationIdV1>,
        publication_key: GraphPublicationKeyV1,
        source_scope: StoreShardIdV1,
        code_scope_hash: SemanticVectorCodeScopeHash,
        source_generation: SemanticVectorSourceGenerationId,
        source_dependency: super::SemanticVectorSourceDependencyV1,
        recipe: SemanticVectorReconstructionRecipe,
        expected_chunk_count: u64,
        expected_prior_verified_head: Option<GraphVerifiedHeadV1>,
        initial_checkpoint_digest: SemanticVectorCheckpointDigest,
        writer_fence: SemanticVectorWriterFence,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let plan_digest = Self::compute_digest(
            &projection,
            &build_id,
            &semantic_generation_id,
            base_generation.as_ref(),
            &publication_key,
            &source_scope,
            &code_scope_hash,
            &source_generation,
            &source_dependency,
            &recipe,
            expected_chunk_count,
            expected_prior_verified_head.as_ref(),
            &initial_checkpoint_digest,
        )?;
        let plan = Self {
            key: SemanticVectorStageKey {
                projection,
                build_id,
                plan_digest,
            },
            semantic_generation_id,
            base_generation,
            publication_key,
            source_scope,
            code_scope_hash,
            source_generation,
            source_dependency,
            recipe,
            expected_chunk_count,
            expected_prior_verified_head,
            initial_checkpoint_digest,
            writer_fence,
        };
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.recipe.validate()?;
        self.semantic_generation_id.validate().map_err(|_| {
            StorageRuntimeContractErrorV1::NonCanonical {
                field: "semantic vector generation id",
            }
        })?;
        if let Some(base_generation) = &self.base_generation {
            base_generation.validate().map_err(|_| {
                StorageRuntimeContractErrorV1::NonCanonical {
                    field: "semantic vector base generation id",
                }
            })?;
            if base_generation == &self.semantic_generation_id {
                return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                    field: "semantic vector base generation",
                });
            }
        }
        self.writer_fence.validate_for(&self.key.projection)?;
        if self.publication_key.projection != self.key.projection {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "semantic vector publication projection",
            });
        }
        if self.expected_chunk_count > MAX_SEMANTIC_VECTOR_STAGE_CHUNKS {
            return Err(StorageRuntimeContractErrorV1::InvalidRange {
                field: "semantic vector expected chunk count",
                min: 0,
                max: MAX_SEMANTIC_VECTOR_STAGE_CHUNKS,
            });
        }
        if !matches!(
            &self.source_scope.scope,
            StoreShardScopeV1::Code { scope, .. }
                if matches!(
                    scope,
                    CodeShardScopeV1::Worktree { .. }
                        | CodeShardScopeV1::Branch { .. }
                        | CodeShardScopeV1::Snapshot { worktree_id: Some(_), .. }
                )
        ) {
            return Err(StorageRuntimeContractErrorV1::OperationScopeMismatch {
                operation: "semantic vector stage",
                shard_family: "non-worktree-code",
            });
        }
        if self.source_scope.brain_id != self.key.projection.shard_id.brain_id
            || self.source_scope.profile_id != self.key.projection.shard_id.profile_id
            || self.source_scope.scope.project_id()
                != self.key.projection.shard_id.scope.project_id()
        {
            return Err(StorageRuntimeContractErrorV1::ShardMismatch {
                field: "semantic vector source scope",
            });
        }
        if self.source_dependency.generation.projection.shard_id != self.key.projection.shard_id {
            return Err(StorageRuntimeContractErrorV1::ShardMismatch {
                field: "semantic vector source dependency",
            });
        }
        if self
            .expected_prior_verified_head
            .as_ref()
            .is_some_and(|head| head.key.projection != self.key.projection)
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "semantic vector expected prior head",
            });
        }
        let expected_digest = Self::compute_digest(
            &self.key.projection,
            &self.key.build_id,
            &self.semantic_generation_id,
            self.base_generation.as_ref(),
            &self.publication_key,
            &self.source_scope,
            &self.code_scope_hash,
            &self.source_generation,
            &self.source_dependency,
            &self.recipe,
            self.expected_chunk_count,
            self.expected_prior_verified_head.as_ref(),
            &self.initial_checkpoint_digest,
        )?;
        if self.key.plan_digest != expected_digest {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "semantic vector plan digest",
            });
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_digest(
        projection: &GraphProjectionIdentityV1,
        build_id: &SemanticVectorBuildId,
        semantic_generation_id: &VectorGenerationIdV1,
        base_generation: Option<&VectorGenerationIdV1>,
        publication_key: &GraphPublicationKeyV1,
        source_scope: &StoreShardIdV1,
        code_scope_hash: &SemanticVectorCodeScopeHash,
        source_generation: &SemanticVectorSourceGenerationId,
        source_dependency: &super::SemanticVectorSourceDependencyV1,
        recipe: &SemanticVectorReconstructionRecipe,
        expected_chunk_count: u64,
        expected_prior_verified_head: Option<&GraphVerifiedHeadV1>,
        initial_checkpoint_digest: &SemanticVectorCheckpointDigest,
    ) -> Result<SemanticVectorPlanDigest, StorageRuntimeContractErrorV1> {
        tracedecay_domain::canonical_sha256(&(
            "tracedecay.semantic-vector-stage-plan",
            projection,
            build_id,
            semantic_generation_id,
            base_generation,
            publication_key,
            source_scope,
            code_scope_hash,
            source_generation,
            source_dependency,
            recipe,
            expected_chunk_count,
            expected_prior_verified_head,
            initial_checkpoint_digest,
        ))
        .map_err(|_| StorageRuntimeContractErrorV1::NonCanonical {
            field: "semantic vector plan digest preimage",
        })
        .and_then(|digest| SemanticVectorPlanDigest::new(digest.as_str()))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStageBatchKey {
    pub stage: SemanticVectorStageKey,
    pub ordinal: u64,
}

impl SemanticVectorStageBatchKey {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.ordinal > i64::MAX.unsigned_abs() {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "semantic vector batch ordinal",
                actual: self.ordinal,
                max: i64::MAX.unsigned_abs(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticVectorStageChunkOperation {
    Embed,
    Tombstone,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStageChunkReceipt {
    pub effect_ordinal: u32,
    pub chunk_id: SemanticVectorChunkId,
    pub chunk_digest: SemanticVectorChunkDigest,
    pub operation: SemanticVectorStageChunkOperation,
    pub output_digest: Option<SemanticVectorOutputDigest>,
}

impl SemanticVectorStageChunkReceipt {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        let valid_output = matches!(
            (self.operation, &self.output_digest),
            (SemanticVectorStageChunkOperation::Embed, Some(_))
                | (SemanticVectorStageChunkOperation::Tombstone, None)
        );
        if !valid_output {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "semantic vector chunk output digest",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStageBatchReceipt {
    pub key: SemanticVectorStageBatchKey,
    pub expected_checkpoint_digest: SemanticVectorCheckpointDigest,
    pub input_digest: SemanticVectorBatchInputDigest,
    /// Exact canonical digest of the native graph write batch represented by this receipt.
    pub output_digest: SemanticVectorBatchOutputDigest,
    pub receipt_digest: SemanticVectorBatchReceiptDigest,
    pub checkpoint_digest: SemanticVectorCheckpointDigest,
    pub chunks: Vec<SemanticVectorStageChunkReceipt>,
}

impl SemanticVectorStageBatchReceipt {
    pub fn new(
        key: SemanticVectorStageBatchKey,
        expected_checkpoint_digest: SemanticVectorCheckpointDigest,
        input_digest: SemanticVectorBatchInputDigest,
        output_digest: SemanticVectorBatchOutputDigest,
        checkpoint_digest: SemanticVectorCheckpointDigest,
        chunks: Vec<SemanticVectorStageChunkReceipt>,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let receipt_digest = Self::compute_digest(
            &key,
            &expected_checkpoint_digest,
            &input_digest,
            &output_digest,
            &checkpoint_digest,
            &chunks,
        )?;
        let receipt = Self {
            key,
            expected_checkpoint_digest,
            input_digest,
            output_digest,
            receipt_digest,
            checkpoint_digest,
            chunks,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.key.validate()?;
        if self.chunks.is_empty() && self.key.ordinal != 0 {
            return Err(StorageRuntimeContractErrorV1::Empty {
                field: "semantic vector non-control batch chunks",
            });
        }
        if self.chunks.len() > MAX_SEMANTIC_VECTOR_STAGE_CHUNKS_PER_BATCH {
            return Err(StorageRuntimeContractErrorV1::TooLong {
                field: "semantic vector batch chunks",
                actual: self.chunks.len(),
                max: MAX_SEMANTIC_VECTOR_STAGE_CHUNKS_PER_BATCH,
            });
        }
        for (ordinal, chunk) in self.chunks.iter().enumerate() {
            chunk.validate()?;
            if usize::try_from(chunk.effect_ordinal).ok() != Some(ordinal) {
                return Err(StorageRuntimeContractErrorV1::NonCanonical {
                    field: "semantic vector chunk effect order",
                });
            }
        }
        if self.chunks.iter().enumerate().any(|(index, chunk)| {
            self.chunks[..index]
                .iter()
                .any(|prior| prior.chunk_id == chunk.chunk_id)
        }) {
            return Err(StorageRuntimeContractErrorV1::NonCanonical {
                field: "semantic vector batch chunk identity",
            });
        }
        let expected_digest = Self::compute_digest(
            &self.key,
            &self.expected_checkpoint_digest,
            &self.input_digest,
            &self.output_digest,
            &self.checkpoint_digest,
            &self.chunks,
        )?;
        if self.receipt_digest != expected_digest {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "semantic vector batch receipt digest",
            });
        }
        Ok(())
    }

    fn compute_digest(
        key: &SemanticVectorStageBatchKey,
        expected_checkpoint_digest: &SemanticVectorCheckpointDigest,
        input_digest: &SemanticVectorBatchInputDigest,
        output_digest: &SemanticVectorBatchOutputDigest,
        checkpoint_digest: &SemanticVectorCheckpointDigest,
        chunks: &[SemanticVectorStageChunkReceipt],
    ) -> Result<SemanticVectorBatchReceiptDigest, StorageRuntimeContractErrorV1> {
        tracedecay_domain::canonical_sha256(&(
            "tracedecay.semantic-vector-stage-batch-receipt",
            key,
            expected_checkpoint_digest,
            input_digest,
            output_digest,
            checkpoint_digest,
            chunks,
        ))
        .map_err(|_| StorageRuntimeContractErrorV1::NonCanonical {
            field: "semantic vector batch receipt digest preimage",
        })
        .and_then(|digest| SemanticVectorBatchReceiptDigest::new(digest.as_str()))
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticVectorStageState {
    Pending,
    ReadyToPublish,
    Published,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStagePublicationIntent {
    pub publication_key: GraphPublicationKeyV1,
    pub expected_recovered_digest: GraphRecoveredGenerationDigestV1,
    pub publication_intent_digest: SemanticVectorPublicationIntentDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStageRecord {
    pub plan: SemanticVectorStagePlan,
    pub state: SemanticVectorStageState,
    pub next_ordinal: u64,
    pub checkpoint_digest: SemanticVectorCheckpointDigest,
    pub recorded_chunk_count: u64,
    pub applied_ordinal: Option<u64>,
    pub applied_receipt_digest: Option<SemanticVectorBatchReceiptDigest>,
    pub applied_checkpoint_digest: Option<SemanticVectorCheckpointDigest>,
    pub applied_graph_batch_digest: Option<SemanticVectorGraphBatchDigest>,
    pub publication_intent: Option<SemanticVectorStagePublicationIntent>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticVectorStageEffectState {
    Pending,
    Applied,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(try_from = "u64", into = "u64")]
pub struct SemanticVectorOutboxSequence(u64);

impl SemanticVectorOutboxSequence {
    pub fn new(value: u64) -> Result<Self, StorageRuntimeContractErrorV1> {
        if value == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "semantic vector outbox sequence",
            });
        }
        if value > i64::MAX.unsigned_abs() {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "semantic vector outbox sequence",
                actual: value,
                max: i64::MAX.unsigned_abs(),
            });
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for SemanticVectorOutboxSequence {
    type Error = StorageRuntimeContractErrorV1;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SemanticVectorOutboxSequence> for u64 {
    fn from(value: SemanticVectorOutboxSequence) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStageGraphBatchEffect {
    pub sequence: SemanticVectorOutboxSequence,
    pub receipt: SemanticVectorStageBatchReceipt,
    pub state: SemanticVectorStageEffectState,
    pub terminal_digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorStageAppendOutcome {
    Appended {
        stage: SemanticVectorStageRecord,
        effect: SemanticVectorStageGraphBatchEffect,
    },
    ExactReplay {
        receipt: SemanticVectorStageBatchReceipt,
        effect: SemanticVectorStageGraphBatchEffect,
    },
    InputConflict {
        existing: SemanticVectorStageBatchReceipt,
    },
    DuplicateChunk {
        chunk_id: SemanticVectorChunkId,
    },
    StaleOrdinal {
        next_ordinal: u64,
    },
    StaleCheckpoint {
        actual: SemanticVectorCheckpointDigest,
    },
    StaleFence {
        actual: SemanticVectorWriterFence,
    },
    ReadyToPublish(SemanticVectorStageRecord),
    Cancelled(SemanticVectorStageRecord),
    MissingStage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorStageBatchReceiptLookup {
    Found(SemanticVectorStageBatchReceipt),
    Missing,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStageBatchCursor {
    pub stage: SemanticVectorStageKey,
    pub ordinal: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStageBatchPageRequest {
    pub stage: SemanticVectorStageKey,
    pub after: Option<SemanticVectorStageBatchCursor>,
    pub max_records: u16,
}

impl SemanticVectorStageBatchPageRequest {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        validate_page(self.max_records, MAX_SEMANTIC_VECTOR_STAGE_PAGE_RECORDS)?;
        if self
            .after
            .as_ref()
            .is_some_and(|cursor| cursor.stage != self.stage)
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "semantic vector batch page cursor",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticVectorStageBatchPage {
    pub receipts: Vec<SemanticVectorStageBatchReceipt>,
    pub continuation: Option<SemanticVectorStageBatchCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStagePendingEffectCursor {
    pub sequence: SemanticVectorOutboxSequence,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStagePendingEffectPageRequest {
    pub projection: GraphProjectionIdentityV1,
    pub after: Option<SemanticVectorStagePendingEffectCursor>,
    pub max_records: u16,
}

impl SemanticVectorStagePendingEffectPageRequest {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        validate_page(
            self.max_records,
            MAX_SEMANTIC_VECTOR_PENDING_EFFECT_PAGE_RECORDS,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticVectorStagePendingEffectPage {
    pub effects: Vec<SemanticVectorStageGraphBatchEffect>,
    pub continuation: Option<SemanticVectorStagePendingEffectCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticVectorStageEffectTerminal {
    Applied {
        graph_batch_digest: SemanticVectorGraphBatchDigest,
    },
    Failed {
        failure_digest: SemanticVectorEffectFailureDigest,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStageSettlement {
    pub batch: SemanticVectorStageBatchKey,
    pub expected_receipt_digest: SemanticVectorBatchReceiptDigest,
    pub terminal: SemanticVectorStageEffectTerminal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorStageSettlementOutcome {
    Settled(SemanticVectorStageGraphBatchEffect),
    ExactReplay(SemanticVectorStageGraphBatchEffect),
    Conflict(SemanticVectorStageGraphBatchEffect),
    StaleOrdinal { next_applied_ordinal: u64 },
    StaleFence { actual: SemanticVectorWriterFence },
    Cancelled(SemanticVectorStageRecord),
    MissingBatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorStageCancelOutcome {
    Cancelled(SemanticVectorStageRecord),
    ExactReplay(SemanticVectorStageRecord),
    StaleFence { actual: SemanticVectorWriterFence },
    ReadyToPublish(SemanticVectorStageRecord),
    MissingStage,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStageWriterAdoption {
    pub stage: SemanticVectorStageKey,
    pub expected: SemanticVectorWriterFence,
    pub replacement: SemanticVectorWriterFence,
    pub ready_publication_replay: Option<GraphPublicationReplayV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorStageWriterAdoptionOutcome {
    Adopted(SemanticVectorStageRecord),
    ExactReplay(SemanticVectorStageRecord),
    StaleFence { actual: SemanticVectorWriterFence },
    VerifiedHeadConflict { actual: Option<GraphVerifiedHeadV1> },
    NotAdoptable(SemanticVectorStageRecord),
    MissingStage,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStagePublicationPrepareRequest {
    pub stage: SemanticVectorStageKey,
    pub publication_replay: GraphPublicationReplayV1,
    pub expected_checkpoint_digest: SemanticVectorCheckpointDigest,
    pub publication_intent_digest: SemanticVectorPublicationIntentDigest,
}

impl SemanticVectorStagePublicationPrepareRequest {
    pub fn new(
        stage: SemanticVectorStageKey,
        publication_replay: GraphPublicationReplayV1,
        expected_checkpoint_digest: SemanticVectorCheckpointDigest,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let publication_intent_digest =
            Self::compute_digest(&stage, &publication_replay, &expected_checkpoint_digest)?;
        let request = Self {
            stage,
            publication_replay,
            expected_checkpoint_digest,
            publication_intent_digest,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.publication_replay.validate()?;
        if self.publication_replay.key.projection != self.stage.projection {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "semantic vector publication replay projection",
            });
        }
        if self.publication_intent_digest
            != Self::compute_digest(
                &self.stage,
                &self.publication_replay,
                &self.expected_checkpoint_digest,
            )?
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "semantic vector publication intent digest",
            });
        }
        Ok(())
    }

    fn compute_digest(
        stage: &SemanticVectorStageKey,
        publication_replay: &GraphPublicationReplayV1,
        expected_checkpoint_digest: &SemanticVectorCheckpointDigest,
    ) -> Result<SemanticVectorPublicationIntentDigest, StorageRuntimeContractErrorV1> {
        tracedecay_domain::canonical_sha256(&(
            "tracedecay.semantic-vector-publication-intent",
            stage,
            publication_replay,
            expected_checkpoint_digest,
        ))
        .map_err(|_| StorageRuntimeContractErrorV1::NonCanonical {
            field: "semantic vector publication intent preimage",
        })
        .and_then(|digest| SemanticVectorPublicationIntentDigest::new(digest.as_str()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticVectorStageIncomplete {
    pub expected_chunks: u64,
    pub recorded_chunks: u64,
    pub pending_batches: u64,
    pub failed_batches: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorStagePublicationPrepareOutcome {
    ReadyToPublish(SemanticVectorStageRecord),
    ExactReplay(SemanticVectorStageRecord),
    Incomplete(SemanticVectorStageIncomplete),
    StaleCheckpoint {
        actual: SemanticVectorCheckpointDigest,
    },
    StaleFence {
        actual: SemanticVectorWriterFence,
    },
    PublicationConflict,
    SemanticGenerationConflict {
        existing: SemanticVectorStageRecord,
    },
    ChunkManifestConflict {
        actual_digest: String,
    },
    Cancelled(SemanticVectorStageRecord),
    MissingStage,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorReadyPublicationCursor {
    pub stage: SemanticVectorStageKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorReadyPublicationPageRequest {
    pub projection: GraphProjectionIdentityV1,
    pub after: Option<SemanticVectorReadyPublicationCursor>,
    pub max_records: u16,
}

impl SemanticVectorReadyPublicationPageRequest {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        validate_page(self.max_records, MAX_SEMANTIC_VECTOR_STAGE_PAGE_RECORDS)?;
        if self
            .after
            .as_ref()
            .is_some_and(|cursor| cursor.stage.projection != self.projection)
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "semantic vector ready publication cursor",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticVectorReadyPublicationPage {
    pub stages: Vec<SemanticVectorStageRecord>,
    pub continuation: Option<SemanticVectorReadyPublicationCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStagePublishSettlement {
    pub stage: SemanticVectorStageKey,
    pub verified_head: GraphVerifiedHeadV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorStagePublishOutcome {
    Published(SemanticVectorStageRecord),
    ExactReplay(SemanticVectorStageRecord),
    VerifiedHeadConflict,
    SemanticGenerationConflict { existing: SemanticVectorStageRecord },
    NotReady(SemanticVectorStageRecord),
    StaleFence { actual: SemanticVectorWriterFence },
    MissingStage,
}

fn validate_page(actual: u16, max: u16) -> Result<(), StorageRuntimeContractErrorV1> {
    if actual == 0 {
        return Err(StorageRuntimeContractErrorV1::Zero {
            field: "semantic vector page records",
        });
    }
    if actual > max {
        return Err(StorageRuntimeContractErrorV1::LimitExceeded {
            field: "semantic vector page records",
            actual: u64::from(actual),
            max: u64::from(max),
        });
    }
    Ok(())
}
