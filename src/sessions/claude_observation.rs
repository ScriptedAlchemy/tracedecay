//! Observation-first Claude transcript ingestion.
//!
//! The provider owns framing and scope. This coordinator owns the mandatory
//! sanitizer/store boundary, feeds the exact committed sanitized value into
//! the existing V1 fold, then drains projection work in source order.

use std::path::{Path, PathBuf};

use thiserror::Error;
use tracedecay_domain::{
    ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1,
    ClaudeSourceCursorV1, ClaudeSourceIdentityV1, DomainError, ObservationContractError,
    ObservationScopeV1, RetentionClass, SessionId,
};
use tracedecay_store::observation::{
    CursorAdvanceOutcome, NonDurableFrameReason, ObservationCursorAdvance,
};
use tracedecay_store::{
    ObservationPersistOutcome, ObservationProjectionStore, ObservationStore, ObservationStoreError,
    ProjectionPersistOutcome, ProjectionStoreError,
};

use crate::application::observation::{
    CaptureClaudeObservationOutcome, CaptureClaudeObservationRequest,
    CaptureClaudeObservationRequestError, ObservationApplication, ObservationApplicationError,
    ObservationCancellation, ReplayObservationsRequest,
};
use crate::privacy::{ClaudeRecordSanitizerV1, PrivacySanitizerError};
use crate::sessions::claude::{
    ClaudeSkippedFrame, ClaudeSkippedFrameReason, ClaudeSource, ClaudeSourceFrame,
    identify_claude_source, scan_claude_source_frames,
};
use crate::sessions::shared::{StoredCursor, TranscriptIngestStats};
use crate::sessions::source::{
    TranscriptSource, load_transcript_cursor, persist_parsed_transcript,
};
use crate::store::{GlobalDbObservationStore, GlobalDbTranscriptStore};

pub(crate) const CLAUDE_TRANSCRIPT_RETENTION_CLASS: &str = "transcript.claude.v1";
/// A prompt hook never allocates an unbounded historical tail. Startup recovery
/// deliberately passes `None` so a deferred hook backlog has a drain path.
pub(crate) const CLAUDE_HOOK_MAX_NEW_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PROJECTIONS_PER_PASS: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClaudeObservationIngestStats {
    pub transcript: TranscriptIngestStats,
    pub observations_committed: u64,
    pub observation_duplicates: u64,
    pub cursor_advances: u64,
    pub cursor_duplicates: u64,
    pub records_rejected: u64,
    pub records_quarantined: u64,
    pub projections_completed: u64,
    pub projections_skipped: u64,
    pub projection_duplicates: u64,
    pub deferred_sources: u64,
}

impl ClaudeObservationIngestStats {
    #[must_use]
    fn merge(mut self, other: Self) -> Self {
        self.transcript = self.transcript.merge(other.transcript);
        self.observations_committed = self
            .observations_committed
            .saturating_add(other.observations_committed);
        self.observation_duplicates = self
            .observation_duplicates
            .saturating_add(other.observation_duplicates);
        self.cursor_advances = self.cursor_advances.saturating_add(other.cursor_advances);
        self.cursor_duplicates = self
            .cursor_duplicates
            .saturating_add(other.cursor_duplicates);
        self.records_rejected = self.records_rejected.saturating_add(other.records_rejected);
        self.records_quarantined = self
            .records_quarantined
            .saturating_add(other.records_quarantined);
        self.projections_completed = self
            .projections_completed
            .saturating_add(other.projections_completed);
        self.projections_skipped = self
            .projections_skipped
            .saturating_add(other.projections_skipped);
        self.projection_duplicates = self
            .projection_duplicates
            .saturating_add(other.projection_duplicates);
        self.deferred_sources = self.deferred_sources.saturating_add(other.deferred_sources);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CapturedClaudeFrame {
    pub committed_cursor: ClaudeSourceCursorV1,
    pub exact_duplicate: bool,
}

#[derive(Debug, Error)]
pub(crate) enum ClaudeObservationIngestError {
    #[error("Claude observation domain value is invalid")]
    Domain(#[from] DomainError),
    #[error("Claude observation contract is invalid")]
    Contract(#[from] ObservationContractError),
    #[error("Claude observation application failed")]
    Application(#[from] ObservationApplicationError),
    #[error("Claude observation request is invalid")]
    Request(#[from] CaptureClaudeObservationRequestError),
    #[error("Claude observation privacy policy is unavailable")]
    Privacy(#[from] PrivacySanitizerError),
    #[error("Claude observation store operation failed")]
    Store(#[from] ObservationStoreError),
    #[error("Claude observation projection failed")]
    Projection(#[from] ProjectionStoreError),
    #[error("Claude transcript cursor could not be loaded")]
    TranscriptCursorUnavailable,
    #[error("Claude observation frame is not in the parsed state")]
    MissingParsedRecord,
    #[error("Claude observation frame rejected its sanitized replacement")]
    InvalidFrameState,
    #[error("Claude observation scanner returned non-contiguous coverage")]
    NonContiguousCoverage,
}

enum FrameCaptureOutcome {
    Persisted(CapturedClaudeFrame),
    Rejected,
    Quarantined,
}

struct FrameCaptureContext {
    source: ClaudeSourceIdentityV1,
    scope: ObservationScopeV1,
    generation: ClaudeFileGenerationV1,
    retention_class: RetentionClass,
    cancellation: ObservationCancellation,
}

enum ScannedSegment {
    Frame(ClaudeSourceFrame),
    Skipped(ClaudeSkippedFrame),
}

impl ScannedSegment {
    fn start(&self) -> u64 {
        match self {
            Self::Frame(frame) => frame.offset,
            Self::Skipped(frame) => frame.offset,
        }
    }

    fn end(&self) -> u64 {
        match self {
            Self::Frame(frame) => frame.end_offset,
            Self::Skipped(frame) => frame.end_offset,
        }
    }
}

/// Converts a durable observation cursor into a provider scanner cursor.
pub(crate) fn scanner_cursor(cursor: Option<&ClaudeSourceCursorV1>) -> StoredCursor {
    cursor.map_or_else(StoredCursor::default, |cursor| StoredCursor {
        position: cursor.byte_offset(),
        mtime: 0,
        file_id: cursor.generation().file_id(),
    })
}

fn earliest_scanner_cursor(
    v1: StoredCursor,
    observation: Option<&ClaudeSourceCursorV1>,
) -> StoredCursor {
    let observation = scanner_cursor(observation);
    if v1.file_id == observation.file_id {
        if v1.position <= observation.position {
            v1
        } else {
            observation
        }
    } else {
        // A missing cursor or a generation disagreement requires a replay from
        // zero. Both stores are idempotent, so this cannot regress either sink.
        StoredCursor::default()
    }
}

fn cursor_at(
    source: &ClaudeSourceIdentityV1,
    scope: &ObservationScopeV1,
    generation: ClaudeFileGenerationV1,
    offset: u64,
) -> Result<ClaudeSourceCursorV1, ObservationContractError> {
    ClaudeSourceCursorV1::new(source.clone(), scope.clone(), generation, offset)
}

fn expected_cursor_for_frame(
    actual: Option<&ClaudeSourceCursorV1>,
    source: &ClaudeSourceIdentityV1,
    scope: &ObservationScopeV1,
    generation: ClaudeFileGenerationV1,
    frame_start: u64,
) -> Result<Option<ClaudeSourceCursorV1>, ObservationContractError> {
    if actual.is_some_and(|cursor| {
        cursor.generation() == generation && cursor.byte_offset() > frame_start
    }) {
        // Commit-before-ACK replay: persistence classifies the immutable
        // observation before cursor CAS, so its original frame-start cursor is
        // the only valid duplicate request.
        return cursor_at(source, scope, generation, frame_start).map(Some);
    }
    Ok(actual.cloned())
}

fn cursor_after_receipt(
    actual: Option<ClaudeSourceCursorV1>,
    committed: &ClaudeSourceCursorV1,
) -> ClaudeSourceCursorV1 {
    match actual {
        Some(actual)
            if actual.generation() == committed.generation()
                && actual.byte_offset() > committed.byte_offset() =>
        {
            actual
        }
        _ => committed.clone(),
    }
}

/// Sanitize and commit one already-framed record before any V1 sink.
async fn capture_frame<S>(
    application: &ObservationApplication<S>,
    frame: &mut ClaudeSourceFrame,
    expected_cursor: Option<ClaudeSourceCursorV1>,
    context: &FrameCaptureContext,
) -> Result<FrameCaptureOutcome, ClaudeObservationIngestError>
where
    S: ObservationStore,
{
    let parsed_record = frame
        .take_parsed_record()
        .ok_or(ClaudeObservationIngestError::MissingParsedRecord)?;
    let identity = ClaudeObservationIdentityMaterialV1::new(
        context.source.clone(),
        context.scope.clone(),
        context.generation,
        *parsed_record.source_range(),
    )?;
    let request = CaptureClaudeObservationRequest::new(
        parsed_record,
        identity,
        expected_cursor,
        context.retention_class.clone(),
        context.cancellation.clone(),
    )?;
    match application.capture_claude_observation(request).await? {
        CaptureClaudeObservationOutcome::Persisted {
            outcome,
            sanitized_record,
            ..
        } => {
            let receipt = outcome.receipt();
            if !frame.set_sanitized_record(sanitized_record) {
                return Err(ClaudeObservationIngestError::InvalidFrameState);
            }
            Ok(FrameCaptureOutcome::Persisted(CapturedClaudeFrame {
                committed_cursor: receipt.committed_cursor().clone(),
                exact_duplicate: matches!(outcome, ObservationPersistOutcome::ExactDuplicate(_)),
            }))
        }
        CaptureClaudeObservationOutcome::Rejected { .. } => Ok(FrameCaptureOutcome::Rejected),
        CaptureClaudeObservationOutcome::Quarantined { .. } => Ok(FrameCaptureOutcome::Quarantined),
    }
}

async fn advance_non_durable<S>(
    application: &ObservationApplication<S>,
    source: &ClaudeSourceIdentityV1,
    scope: &ObservationScopeV1,
    generation: ClaudeFileGenerationV1,
    expected_cursor: Option<ClaudeSourceCursorV1>,
    covered: ClaudeByteRangeV1,
    reason: NonDurableFrameReason,
) -> Result<CursorAdvanceOutcome, ClaudeObservationIngestError>
where
    S: ObservationStore,
{
    let advance = ObservationCursorAdvance::new(
        source.clone(),
        scope.clone(),
        generation,
        expected_cursor,
        covered,
        reason,
    )?;
    Ok(application
        .advance_non_durable_source_cursor(advance)
        .await?)
}

async fn process_source(
    db: &crate::global_db::GlobalDb,
    source_adapter: &ClaudeSource,
    path: &Path,
    project_root: &Path,
    scope: &ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> Result<ClaudeObservationIngestStats, ClaudeObservationIngestError> {
    if cancellation.is_cancelled() {
        return Err(ObservationApplicationError::Cancelled.into());
    }
    let Some(identity) = identify_claude_source(path) else {
        return Ok(ClaudeObservationIngestStats::default());
    };
    let source = ClaudeSourceIdentityV1::new(SessionId::new(identity.session_id.clone())?)?;
    let observation_store = GlobalDbObservationStore::new(db);
    let transcript_store = GlobalDbTranscriptStore::new(db);
    let loaded = load_transcript_cursor(&transcript_store, identity.cursor_key.clone())
        .await
        .ok_or(ClaudeObservationIngestError::TranscriptCursorUnavailable)?;
    let observation_cursor = observation_store.get_source_cursor(&source, scope).await?;
    let previous = earliest_scanner_cursor(loaded.checkpoint.state, observation_cursor.as_ref());
    let Some(mut scan) = scan_claude_source_frames(identity, previous, max_new_bytes) else {
        return Ok(ClaudeObservationIngestStats::default());
    };
    if source_adapter
        .retain_scoped_frames(&mut scan, project_root)
        .is_none()
    {
        return Ok(ClaudeObservationIngestStats {
            deferred_sources: 1,
            ..ClaudeObservationIngestStats::default()
        });
    }

    let generation = ClaudeFileGenerationV1::new(scan.file_generation)?;
    let sanitizer = ClaudeRecordSanitizerV1::pr5()?;
    let application = ObservationApplication::new(observation_store, sanitizer);
    let retention_class = RetentionClass::new(CLAUDE_TRANSCRIPT_RETENTION_CLASS)?;
    let capture_context = FrameCaptureContext {
        source: source.clone(),
        scope: scope.clone(),
        generation,
        retention_class,
        cancellation: cancellation.clone(),
    };
    let mut segments = Vec::with_capacity(scan.frames.len() + scan.skipped_frames.len());
    segments.extend(
        std::mem::take(&mut scan.frames)
            .into_iter()
            .map(ScannedSegment::Frame),
    );
    segments.extend(
        std::mem::take(&mut scan.skipped_frames)
            .into_iter()
            .map(ScannedSegment::Skipped),
    );
    segments.sort_by_key(ScannedSegment::start);
    if segments
        .windows(2)
        .any(|pair| pair[0].end() != pair[1].start())
    {
        return Err(ClaudeObservationIngestError::NonContiguousCoverage);
    }

    let mut stats = ClaudeObservationIngestStats::default();
    let mut observation_cursor = observation_cursor;
    let mut sanitized_frames = Vec::new();
    for segment in segments {
        if cancellation.is_cancelled() {
            return Err(ObservationApplicationError::Cancelled.into());
        }
        match segment {
            ScannedSegment::Skipped(skipped) => {
                if observation_cursor.as_ref().is_some_and(|cursor| {
                    cursor.generation() == generation && cursor.byte_offset() >= skipped.end_offset
                }) {
                    stats.cursor_duplicates = stats.cursor_duplicates.saturating_add(1);
                    continue;
                }
                let covered = ClaudeByteRangeV1::new(skipped.offset, skipped.end_offset)?;
                let reason = match skipped.reason {
                    ClaudeSkippedFrameReason::Whitespace => NonDurableFrameReason::BlankFrame,
                    ClaudeSkippedFrameReason::OutOfScope => NonDurableFrameReason::OutOfScope,
                };
                let outcome = advance_non_durable(
                    &application,
                    &source,
                    scope,
                    generation,
                    observation_cursor.clone(),
                    covered,
                    reason,
                )
                .await?;
                observation_cursor =
                    Some(cursor_at(&source, scope, generation, skipped.end_offset)?);
                match outcome {
                    CursorAdvanceOutcome::Committed => {
                        stats.cursor_advances = stats.cursor_advances.saturating_add(1);
                    }
                    CursorAdvanceOutcome::ExactDuplicate => {
                        stats.cursor_duplicates = stats.cursor_duplicates.saturating_add(1);
                    }
                }
            }
            ScannedSegment::Frame(mut frame) => {
                let expected = expected_cursor_for_frame(
                    observation_cursor.as_ref(),
                    &source,
                    scope,
                    generation,
                    frame.offset,
                )?;
                let range = ClaudeByteRangeV1::new(frame.offset, frame.end_offset)?;
                match capture_frame(&application, &mut frame, expected, &capture_context).await? {
                    FrameCaptureOutcome::Persisted(captured) => {
                        observation_cursor = Some(cursor_after_receipt(
                            observation_cursor,
                            &captured.committed_cursor,
                        ));
                        if captured.exact_duplicate {
                            stats.observation_duplicates =
                                stats.observation_duplicates.saturating_add(1);
                        } else {
                            stats.observations_committed =
                                stats.observations_committed.saturating_add(1);
                        }
                        sanitized_frames.push(frame);
                    }
                    FrameCaptureOutcome::Rejected => {
                        stats.records_rejected = stats.records_rejected.saturating_add(1);
                        if observation_cursor.as_ref().is_some_and(|cursor| {
                            cursor.generation() == generation && cursor.byte_offset() >= range.end()
                        }) {
                            stats.cursor_duplicates = stats.cursor_duplicates.saturating_add(1);
                            continue;
                        }
                        let outcome = advance_non_durable(
                            &application,
                            &source,
                            scope,
                            generation,
                            observation_cursor.clone(),
                            range,
                            NonDurableFrameReason::SanitizerRejected,
                        )
                        .await?;
                        observation_cursor =
                            Some(cursor_at(&source, scope, generation, range.end())?);
                        match outcome {
                            CursorAdvanceOutcome::Committed => {
                                stats.cursor_advances = stats.cursor_advances.saturating_add(1);
                            }
                            CursorAdvanceOutcome::ExactDuplicate => {
                                stats.cursor_duplicates = stats.cursor_duplicates.saturating_add(1);
                            }
                        }
                    }
                    FrameCaptureOutcome::Quarantined => {
                        stats.records_quarantined = stats.records_quarantined.saturating_add(1);
                        if observation_cursor.as_ref().is_some_and(|cursor| {
                            cursor.generation() == generation && cursor.byte_offset() >= range.end()
                        }) {
                            stats.cursor_duplicates = stats.cursor_duplicates.saturating_add(1);
                            continue;
                        }
                        let outcome = advance_non_durable(
                            &application,
                            &source,
                            scope,
                            generation,
                            observation_cursor.clone(),
                            range,
                            NonDurableFrameReason::SanitizerQuarantined,
                        )
                        .await?;
                        observation_cursor =
                            Some(cursor_at(&source, scope, generation, range.end())?);
                        match outcome {
                            CursorAdvanceOutcome::Committed => {
                                stats.cursor_advances = stats.cursor_advances.saturating_add(1);
                            }
                            CursorAdvanceOutcome::ExactDuplicate => {
                                stats.cursor_duplicates = stats.cursor_duplicates.saturating_add(1);
                            }
                        }
                    }
                }
            }
        }
    }
    scan.frames = sanitized_frames;
    let Some(parsed) = source_adapter.fold_scanned_frames(&scan, project_root) else {
        return Err(ClaudeObservationIngestError::InvalidFrameState);
    };
    stats.transcript = persist_parsed_transcript(
        &transcript_store,
        "claude",
        path,
        project_root,
        loaded,
        &scan.previous_cursor,
        parsed,
    )
    .await;
    Ok(stats)
}

async fn drain_projection_queue(
    db: &crate::global_db::GlobalDb,
    cancellation: &ObservationCancellation,
) -> Result<ClaudeObservationIngestStats, ClaudeObservationIngestError> {
    let store = GlobalDbObservationStore::new(db);
    let mut stats = ClaudeObservationIngestStats::default();
    for _ in 0..MAX_PROJECTIONS_PER_PASS {
        if cancellation.is_cancelled() {
            return Err(ObservationApplicationError::Cancelled.into());
        }
        let Some(observation_id) = store.next_queued_observation().await? else {
            break;
        };
        match store.project_observation(&observation_id).await? {
            ProjectionPersistOutcome::Projected(_) => {
                stats.projections_completed = stats.projections_completed.saturating_add(1);
            }
            ProjectionPersistOutcome::Skipped { .. } => {
                stats.projections_skipped = stats.projections_skipped.saturating_add(1);
            }
            ProjectionPersistOutcome::ExactDuplicate(_) => {
                stats.projection_duplicates = stats.projection_duplicates.saturating_add(1);
            }
        }
    }
    Ok(stats)
}

/// Ingest one Claude source against an already-open authoritative database.
pub(crate) async fn ingest_source_with_observations(
    db: &crate::global_db::GlobalDb,
    source: &ClaudeSource,
    project_root: &Path,
    scope: ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: ObservationCancellation,
) -> Result<ClaudeObservationIngestStats, ClaudeObservationIngestError> {
    let mut stats = ClaudeObservationIngestStats::default();
    for path in source.transcript_paths(project_root) {
        stats = stats.merge(
            process_source(
                db,
                source,
                &path,
                project_root,
                &scope,
                max_new_bytes,
                &cancellation,
            )
            .await?,
        );
    }
    Ok(stats.merge(drain_projection_queue(db, &cancellation).await?))
}

/// Production profile-scope Claude path used by hooks and startup recovery.
pub(crate) async fn ingest_user_sessions(
    db: &crate::global_db::GlobalDb,
    profile_root: &Path,
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
    max_new_bytes: Option<u64>,
    cancellation: ObservationCancellation,
) -> Result<ClaudeObservationIngestStats, ClaudeObservationIngestError> {
    let Some(source) = ClaudeSource::new() else {
        return Ok(ClaudeObservationIngestStats::default());
    };
    let source = source.for_user_scope(session_id, registered_roots);
    ingest_source_with_observations(
        db,
        &source,
        profile_root,
        ObservationScopeV1::Profile,
        max_new_bytes,
        cancellation,
    )
    .await
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;
    use tracedecay_store::{ObservationReplayRequest, ObservationStore};

    use super::*;

    struct Fixture {
        temp: TempDir,
        home: PathBuf,
        profile: PathBuf,
        transcript: PathBuf,
        db: crate::global_db::GlobalDb,
    }

    impl Fixture {
        async fn new(session_id: &str) -> Self {
            let temp = TempDir::new().expect("temporary observation fixture");
            let home = temp.path().join("home");
            let profile = home.join(".tracedecay");
            let transcript = home
                .join(".claude/projects/project-scope")
                .join(format!("{session_id}.jsonl"));
            fs::create_dir_all(transcript.parent().expect("transcript parent"))
                .expect("create Claude fixture tree");
            fs::create_dir_all(&profile).expect("create profile root");
            let db = crate::global_db::GlobalDb::open_at(&profile.join("sessions.db"))
                .await
                .expect("open authoritative session database");
            Self {
                temp,
                home,
                profile,
                transcript,
                db,
            }
        }

        fn source(&self, session_id: &str) -> ClaudeSource {
            ClaudeSource::with_home(&self.home)
                .for_user_scope(Some(session_id.to_string()), Vec::new())
        }

        fn write_record(&self, content: &str, secret: &str) {
            let record = json!({
                "type": "user",
                "sessionId": self.transcript.file_stem().and_then(|value| value.to_str()),
                "uuid": "message-production-vertical",
                "timestamp": "2026-07-15T00:00:00Z",
                "cwd": self.temp.path(),
                "message": {
                    "role": "user",
                    "content": content,
                    "secret_key": secret,
                }
            });
            fs::write(&self.transcript, format!("{record}\n"))
                .expect("write Claude observation fixture");
        }

        async fn ingest(
            &self,
            source: &ClaudeSource,
            max_new_bytes: Option<u64>,
            cancellation: ObservationCancellation,
        ) -> Result<ClaudeObservationIngestStats, ClaudeObservationIngestError> {
            ingest_source_with_observations(
                &self.db,
                source,
                &self.profile,
                ObservationScopeV1::Profile,
                max_new_bytes,
                cancellation,
            )
            .await
        }
    }

    #[tokio::test]
    async fn production_vertical_persists_only_sanitized_payload_and_searchable_v1_row() {
        let fixture = Fixture::new("production-session").await;
        fixture.write_record(
            "production vertical searchable",
            "never-persist-this-secret",
        );
        let source = fixture.source("production-session");

        let stats = fixture
            .ingest(&source, None, ObservationCancellation::default())
            .await
            .expect("ingest production Claude observation");

        assert_eq!(stats.observations_committed, 1);
        assert_eq!(stats.transcript.messages_upserted, 1);
        assert_eq!(stats.projections_completed, 1);
        let store = GlobalDbObservationStore::new(&fixture.db);
        let observations = store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap();
        assert_eq!(observations.len(), 1);
        let payload = observations[0].observation().payload();
        assert!(!payload.to_string().contains("never-persist-this-secret"));
        assert!(
            payload["message"]["secret_key"]
                .as_str()
                .is_some_and(|value| value.starts_with("[TraceDecay redacted:"))
        );
        assert_eq!(
            observations[0].projection_status(),
            tracedecay_store::ObservationProjectionStatus::NotQueued
        );
        let hits = fixture
            .db
            .search_session_messages("claude", Some("user"), "production vertical searchable", 10)
            .await;
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn commit_before_ack_retry_backfills_v1_without_duplicate_observation() {
        let fixture = Fixture::new("retry-session").await;
        fixture.write_record("retry backfill searchable", "retry-secret");
        let source_adapter = fixture.source("retry-session");
        let identity = identify_claude_source(&fixture.transcript).unwrap();
        let mut scan = scan_claude_source_frames(identity.clone(), StoredCursor::default(), None)
            .expect("scan complete retry frame");
        source_adapter
            .retain_scoped_frames(&mut scan, &fixture.profile)
            .expect("retain profile-scoped retry frame");
        let source =
            ClaudeSourceIdentityV1::new(SessionId::new(identity.session_id).unwrap()).unwrap();
        let store = GlobalDbObservationStore::new(&fixture.db);
        let application = ObservationApplication::new(
            store,
            ClaudeRecordSanitizerV1::pr5().expect("PR5 sanitizer"),
        );
        let capture = capture_frame(
            &application,
            scan.frames.first_mut().expect("retry frame"),
            None,
            &FrameCaptureContext {
                source,
                scope: ObservationScopeV1::Profile,
                generation: ClaudeFileGenerationV1::new(scan.file_generation).unwrap(),
                retention_class: RetentionClass::new(CLAUDE_TRANSCRIPT_RETENTION_CLASS).unwrap(),
                cancellation: ObservationCancellation::default(),
            },
        )
        .await
        .expect("commit observation before simulated lost acknowledgement");
        assert!(matches!(capture, FrameCaptureOutcome::Persisted(_)));

        let stats = fixture
            .ingest(&source_adapter, None, ObservationCancellation::default())
            .await
            .expect("retry production coordinator");

        assert_eq!(stats.observations_committed, 0);
        assert_eq!(stats.observation_duplicates, 1);
        assert_eq!(stats.transcript.messages_upserted, 1);
        assert_eq!(stats.projections_completed, 1);
        let observations = application
            .replay_observations(ReplayObservationsRequest::new(
                ObservationReplayRequest::new(0, 10).unwrap(),
                ObservationCancellation::default(),
            ))
            .await
            .unwrap();
        assert_eq!(observations.observations().len(), 1);
    }

    #[tokio::test]
    async fn partial_backlog_and_cancellation_never_advance_observation_state() {
        let fixture = Fixture::new("deferred-session").await;
        fs::write(&fixture.transcript, b"{\"type\":\"user\"").expect("write partial Claude frame");
        let source = fixture.source("deferred-session");

        let partial = fixture
            .ingest(&source, None, ObservationCancellation::default())
            .await
            .expect("defer partial frame");
        assert_eq!(partial.observations_committed, 0);
        let backlog = fixture
            .ingest(&source, Some(1), ObservationCancellation::default())
            .await
            .expect("defer bounded backlog");
        assert_eq!(backlog.deferred_sources, 1);

        let cancellation = ObservationCancellation::default();
        cancellation.cancel();
        assert!(matches!(
            fixture.ingest(&source, None, cancellation).await,
            Err(ClaudeObservationIngestError::Application(
                ObservationApplicationError::Cancelled
            ))
        ));
        let observations = GlobalDbObservationStore::new(&fixture.db)
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap();
        assert!(observations.is_empty());
    }
}
