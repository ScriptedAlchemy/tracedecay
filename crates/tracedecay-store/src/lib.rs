//! Store-facing persistence contracts for TraceDecay.
//!
//! This crate owns only persistence contracts and their data transfer objects.
//! Connection ownership, transaction boundaries, recovery policy, and storage
//! resolution remain with the application crate's authoritative store adapter.

pub mod memory;
pub mod observation;
pub mod projection;
pub mod transcript;

pub use memory::{
    CurrentFactsQuery, FactAsOfQuery, FactCommitConflict, FactCommitOutcome, FactCommitReceipt,
    FactCurrentQuery, FactLineageCursor, FactLineageQuery, FactStore, FactStoreError,
    FactStoreResult, FactWriteBatch, LegacyFactQuery, RetrievalAnchorQuery, StoredFactV1,
};
pub use observation::{
    AnchoredObservationWrite, ObservationCommitReceipt, ObservationPersistOutcome,
    ObservationProjectionStatus, ObservationReplayRequest, ObservationStore, ObservationStoreError,
    ObservationStoreResult, ObservationWrite, StoredObservation,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};
pub use projection::{
    CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION, ClaudeObservationProjection,
    ClaudeSessionMessageProjection, ObservationProjection, ObservationProjectionStore,
    ProjectedObservation, ProjectionCheckpoint, ProjectionPersistOutcome, ProjectionProvenance,
    ProjectionRebuildOutcome, ProjectionSkipReason, ProjectionStoreError, ProjectionStoreResult,
    SESSION_MESSAGE_PROJECTOR_VERSION, SESSION_MESSAGE_PROJECTOR_VERSION_V1,
    SESSION_MESSAGE_PROJECTOR_VERSION_V2, SESSION_MESSAGE_PROJECTOR_VERSION_V3,
    SESSION_MESSAGE_PROJECTOR_VERSION_V4, SessionMessageProjection, WorkflowFactProjection,
    WorkflowFactRecord,
};
pub use transcript::{
    ParseOffset, SessionMessageRecord, SessionRecord, TranscriptStore, TranscriptStoreError,
    TranscriptStoreResult, TranscriptWriteBatch, TranscriptWriteKind,
};
