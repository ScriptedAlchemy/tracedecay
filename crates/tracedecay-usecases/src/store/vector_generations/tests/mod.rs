use super::*;
use tracedecay_domain::{
    BoundedSanitizedText, ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision,
    CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, EmbeddingDeviceClassV1, EmbeddingMetricV1,
    EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingProjectionKeyV1,
    EmbeddingTruncationSideV1, FileOccurrenceId, LanguageDescriptorRevision, PolicyRevisionId,
    PrivacyDomainId, ProjectionBatchRequestV1, ProjectionReplayReasonV1, SanitizerRevision,
    SensitivityDecision, SensitivityLevelV1, SourceSpan,
};
use tracedecay_runtime_core::db::{DatabaseAuthority, TestDatabaseRuntimeMode};
use tracedecay_semantic::legacy_migration::{
    CanonicalEligibleChunkSetV1, NeverCancelLegacyVectorMigrationV1,
    ProductionLegacyVectorCanonicalRebuilderV1, StagedCanonicalVectorRebuildV1,
    prepare_legacy_vector_migration,
};

include!("common.rs");
include!("behavior_first.rs");
include!("behavior_second.rs");
include!("memory_probe.rs");
