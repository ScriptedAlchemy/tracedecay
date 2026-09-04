mod adjacency_id_index;
mod backup;
mod bundle;
mod epoch_cache;
mod error;
mod generation;
mod generation_runtime;
mod generation_staging_runtime;
mod hotpath_observe;
mod lease;
mod limits;
mod location;
mod mutation;
mod owner;
mod point_read;
mod projection;
mod projection_identity_index;
mod projection_read;
mod publication;
mod recovery;
mod registry;
mod runtime;
mod schema;
mod sealed_store;
pub mod semantic_vector_native;
mod state;
mod store_quarantine;
mod traversal;
mod vector;
mod verified_marker;

pub use backup::GraphBackupReceipt;
pub use bundle::{
    MAX_SEALED_READ_BUNDLE_ARTIFACT_BYTES_V1, SEALED_READ_BUNDLE_FORMAT_V1,
    SealedReadBundleArtifactStateV1, SealedReadBundleArtifactV1, SealedReadBundleManifestV1,
    SealedReadBundleWriterV1, load_sealed_read_bundle_artifact, retire_sealed_read_bundle,
    sweep_aborted_sealed_read_bundle_temporaries,
};
pub use error::{GraphBudgetKind, GraphConflictContextV1, GraphDbError};
pub use generation::{
    GraphEntityRef, GraphGenerationDependency, GraphGenerationManifest,
    GraphGenerationManifestIdentity, GraphGenerationManifestProvider, GraphGenerationRelation,
    GraphGenerationReplayMetadata, GraphGenerationReplaySource, GraphProjectionIdentity,
    GraphProjectorRevision, GraphRelationRef, GraphReplayCollectionOutcome,
    SealedCodeGenerationReplay, SealedGraphStateDigest, SemanticVectorGenerationReplay,
};
pub use generation_runtime::{SealedStagingRelease, SealedStagingRetentionReason};
pub use lease::{VerifiedGraphSnapshot, VerifiedTraversalResult, VerifiedTraversalVisit};
pub use limits::{
    MAX_GRAPH_BATCH_CANONICAL_BYTES, MAX_GRAPH_ENTITY_LABEL_BYTES, MAX_GRAPH_ENTITY_LABELS,
    MAX_GRAPH_IDENTIFIER_BYTES, MAX_GRAPH_PROPERTIES, MAX_GRAPH_PROPERTY_AGGREGATE_BYTES,
    MAX_GRAPH_PROPERTY_VALUE_BYTES, MAX_GRAPH_VECTOR_DIMENSION,
    MAX_SEMANTIC_VECTOR_GRAPH_BATCH_CANONICAL_BYTES, MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES,
    MAX_VERIFIED_GENERATION_BATCH_MUTATIONS, MAX_VERIFIED_GENERATION_ENTITIES,
    MAX_VERIFIED_GENERATION_RELATIONS,
};
#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
pub use location::{GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphFormatVersion};
#[cfg(not(any(feature = "test-helpers", feature = "eval-helpers")))]
pub(crate) use location::{
    GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphFormatVersion,
};
#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
pub use owner::GraphDbOwner;
#[cfg(not(any(feature = "test-helpers", feature = "eval-helpers")))]
pub(crate) use owner::GraphDbOwner;
pub use owner::{
    GraphDbLeaseV1, GraphDbOwnerAttachmentV1, GraphDbRetirementTarget, GraphDbRuntimeIdentityV1,
};
pub(crate) use owner::{GraphDbOwnerAttachmentId, GraphDbOwnerId, GraphDbRetirementReservationId};
pub use projection::{
    GraphCancellation, GraphCommit, GraphEntity, GraphEntityId, GraphGenerationId,
    GraphIdempotencyKey, GraphLabel, GraphMutation, GraphNamespace, GraphProjectionId,
    GraphProperty, GraphPropertyName, GraphRelation, GraphRelationId, GraphRelationKind,
    GraphVector, GraphWatermark, GraphWriteBatch, SourceGeneration,
};
pub use projection::{NeverCancelled, ProjectionReplacement};
#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
pub use projection_read::GraphProjectionLabelPage;
pub use projection_read::{
    GraphProjectionPage, GraphProjectionReadRequest, GraphProjectionTelemetry,
    GraphProjectionTelemetryRequest,
};
#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
pub use publication::{
    GraphPublication, GraphPublicationDigest, GraphPublicationInputDigest, GraphPublicationReceipt,
};
#[cfg(not(any(feature = "test-helpers", feature = "eval-helpers")))]
pub(crate) use publication::{
    GraphPublication, GraphPublicationDigest, GraphPublicationInputDigest, GraphPublicationReceipt,
};
pub use recovery::VerifiedGraphCommit;
pub use registry::{
    CODE_GRAPH_SHARD_NAMESPACE_PREFIX, LEGACY_PER_GENERATION_CODE_GRAPH_NAMESPACE_PREFIX,
    code_graph_shard_namespace, is_code_graph_shard_namespace,
    is_legacy_per_generation_code_graph_namespace,
};
pub use registry::{
    GraphDbOwnerRegistrationV1, GraphDbRegistration, GraphDbRegistry, GraphDbRegistryCapacity,
    GraphDbRegistryConfig, GraphDbRegistryStatus, GraphDbRetirementCommit,
    GraphDbRetirementOutcome, GraphDbRetirementRefusal, GraphDbRetirementReservation,
    GraphPublicationPreparationV1, ProvenGraphPublicationV1, SemanticVectorRetentionAction,
    SemanticVectorRetentionCensus, SemanticVectorRetentionStep,
    SemanticVectorRetirementReservation, VerifiedGenerationBatchApply,
    VerifiedGenerationBatchCommit, VerifiedGenerationBeginV1,
};
pub use runtime::{GraphDb, GraphDbRuntimeState, GraphSnapshot};

/// What hydration decoded on **this thread** since the last take.
///
/// Scoped to the calling thread so that a reading is exactly the work that
/// thread drove, and a test running in parallel cannot contribute to another
/// test's counts.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GraphDbHydrationCounters {
    pub nodes: u64,
    pub edges: u64,
    pub replay_rows: u64,
    pub generation_bytes: u64,
    pub hydration_source: Option<&'static str>,
}

#[cfg(any(test, feature = "test-helpers"))]
pub fn take_graph_db_hydration_counters() -> GraphDbHydrationCounters {
    hotpath_observe::take_hydration_counters()
}

/// How sealed-generation verification resolved on **this thread** since the
/// last take.
///
/// Independent of the Hotpath feature on purpose: a test that asserts a marker
/// hit skipped the row enumeration must be able to observe that in an ordinary
/// build.
///
/// Scoped to the calling thread so that a reading is exactly the verification
/// work that thread drove, and a test running in parallel cannot contribute to
/// another test's counts.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GraphDbVerificationCounters {
    /// Generations whose digest a verified-generation marker already proved.
    pub marker_hits: u64,
    /// Canonical bytes those marker hits did **not** re-hash.
    pub marker_hit_bytes: u64,
    /// Generations whose rows were streamed and hashed in full.
    pub full_verifications: u64,
    /// Canonical bytes those full proofs hashed.
    pub full_verification_bytes: u64,
}

#[cfg(any(test, feature = "test-helpers"))]
pub fn take_graph_db_verification_counters() -> GraphDbVerificationCounters {
    hotpath_observe::take_verification_counters()
}

/// Fan-out decode, quarantine-lock, and epoch-cache work on **this thread**
/// since the last take.
///
/// Used by operation-count tests that prove ID-only paging stays O(page +
/// frontier) instead of hydrating every property and re-locking quarantine
/// on every edge.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GraphDbTraversalCounters {
    pub property_decodes: u64,
    pub relation_identity_decodes: u64,
    pub quarantine_lock_acquisitions: u64,
    pub label_universe_scans: u64,
    pub adjacency_index_builds: u64,
    pub adjacency_index_hits: u64,
}

#[cfg(any(test, feature = "test-helpers"))]
pub fn take_graph_db_traversal_counters() -> GraphDbTraversalCounters {
    hotpath_observe::take_traversal_counters()
}
pub use traversal::{
    GraphRelationTarget, GraphTraversalDirection, RelationFanoutOverflow, TraversalRequest,
    TraversalResult, TraversalVisit,
};
pub use vector::{
    GraphVectorIndexRequest, GraphVectorIndexStatus, MAX_VECTOR_SEARCH_LIMIT, VectorMatch,
    VectorMetric, VectorSearchRequest, VectorSearchResult,
};
