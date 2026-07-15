//! Store-facing persistence contracts for TraceDecay.
//!
//! This crate owns only persistence contracts and their data transfer objects.
//! Connection ownership, transaction boundaries, recovery policy, and storage
//! resolution remain with the application crate's authoritative store adapter.

pub mod observation;
pub mod projection;
pub mod transcript;

pub use observation::{
    ObservationCommitReceipt, ObservationPersistOutcome, ObservationProjectionStatus,
    ObservationReplayRequest, ObservationStore, ObservationStoreError, ObservationStoreResult,
    ObservationWrite, StoredObservation,
};
pub use projection::{
    CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION, ClaudeObservationProjection,
    ClaudeSessionMessageProjection, ObservationProjection, ObservationProjectionStore,
    ProjectionCheckpoint, ProjectionPersistOutcome, ProjectionProvenance, ProjectionRebuildOutcome,
    ProjectionSkipReason, ProjectionStoreError, ProjectionStoreResult,
    SESSION_MESSAGE_PROJECTOR_VERSION_V1, SessionMessageProjection,
};
pub use transcript::{
    ParseOffset, SessionMessageRecord, SessionRecord, TranscriptStore, TranscriptStoreError,
    TranscriptStoreResult, TranscriptWriteBatch, TranscriptWriteKind,
};
