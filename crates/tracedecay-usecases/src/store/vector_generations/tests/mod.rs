use super::*;
use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision, EmbeddingDeviceClassV1,
    EmbeddingMetricV1, EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
    EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1, PrivacyDomainId, ProjectionBatchRequestV1,
    ProjectionReplayReasonV1,
};
use tracedecay_runtime_core::db::{DatabaseAuthority, TestDatabaseRuntimeMode};

include!("common.rs");
include!("behavior_first.rs");
include!("behavior_second.rs");
include!("memory_probe.rs");
