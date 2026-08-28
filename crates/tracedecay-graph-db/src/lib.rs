mod backup;
mod bundle;
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
pub mod semantic_vector_native;
mod state;
mod traversal;
mod vector;

pub use backup::GraphBackupReceipt;
pub use bundle::{
    MAX_SEALED_READ_BUNDLE_ARTIFACT_BYTES_V1, SEALED_READ_BUNDLE_FORMAT_V1,
    SealedReadBundleArtifactStateV1, SealedReadBundleArtifactV1, SealedReadBundleManifestV1,
    SealedReadBundleWriterV1, load_sealed_read_bundle_artifact, retire_sealed_read_bundle,
};
pub use error::{GraphBudgetKind, GraphDbError};
pub use generation::{
    GraphEntityRef, GraphGenerationDependency, GraphGenerationManifest,
    GraphGenerationManifestIdentity, GraphGenerationManifestProvider, GraphGenerationRelation,
    GraphGenerationReplayMetadata, GraphGenerationReplaySource, GraphProjectionIdentity,
    GraphProjectorRevision, GraphRelationRef, GraphReplayCollectionOutcome,
    SealedCodeGenerationReplay, SealedGraphStateDigest, SemanticVectorGenerationReplay,
};
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
    GraphDbOwnerRegistrationV1, GraphDbRegistration, GraphDbRegistry, GraphDbRegistryCapacity,
    GraphDbRegistryConfig, GraphDbRegistryStatus, GraphDbRetirementCommit,
    GraphDbRetirementOutcome, GraphDbRetirementRefusal, GraphDbRetirementReservation,
    SemanticVectorRetentionAction, SemanticVectorRetentionCensus, SemanticVectorRetentionStep,
    SemanticVectorRetirementReservation, VerifiedGenerationBatchApply,
    VerifiedGenerationBatchCommit,
};
pub use runtime::{GraphDb, GraphDbRuntimeState, GraphSnapshot};

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
pub use traversal::{GraphTraversalDirection, TraversalRequest, TraversalResult, TraversalVisit};
#[cfg(not(any(feature = "test-helpers", feature = "eval-helpers")))]
pub(crate) use vector::{GraphVectorIndexRequest, GraphVectorIndexStatus};
#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
pub use vector::{
    GraphVectorIndexRequest, GraphVectorIndexStatus, MAX_VECTOR_SEARCH_LIMIT, VectorMatch,
    VectorMetric, VectorSearchRequest, VectorSearchResult,
};
#[cfg(not(any(feature = "test-helpers", feature = "eval-helpers")))]
pub use vector::{
    MAX_VECTOR_SEARCH_LIMIT, VectorMatch, VectorMetric, VectorSearchRequest, VectorSearchResult,
};
