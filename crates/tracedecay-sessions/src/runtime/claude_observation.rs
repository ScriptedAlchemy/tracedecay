//! Observation-first Claude transcript ingestion.
//!
//! The provider owns framing and scope. This coordinator routes framed records
//! through the host admission authority, then drains projection work in source
//! order. The projector is the only writer of V1 session and message rows.

use std::path::{Path, PathBuf};

use thiserror::Error;
use tracedecay_domain::{
    ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1,
    ClaudeSourceCursorV1, ClaudeSourceIdentityV1, DomainError, ObservationContractError,
    ObservationId, ObservationScopeV1, RetentionClass, SanitizationReceiptV1, SessionId,
};
use tracedecay_store::observation::{
    CursorAdvanceOutcome, NonDurableFrameReason, ObservationCursorAdvance,
};
use tracedecay_store::{
    ObservationPersistOutcome, ObservationStoreError, ParseOffset, ProjectionStoreError,
    TranscriptStoreError,
};

use crate::admission::HostAdmission;
use crate::observation::{
    CaptureClaudeObservationOutcome, CaptureClaudeObservationRequest,
    CaptureClaudeObservationRequestError, ObservationApplicationError, ObservationCancellation,
};
use tracedecay_runtime_core::privacy::PrivacySanitizerError;
use crate::runtime::claude::{
    ClaudeFrameCoverage, ClaudeSkippedFrame, ClaudeSkippedFrameReason, ClaudeSource,
    ClaudeSourceFrame, identify_claude_source, try_scan_claude_source_frames_with_resume,
};
use crate::runtime::shared::{StoredCursor, TranscriptIngestStats};
use crate::runtime::snapshot_observation::host_admission_error;
use crate::runtime::source::{
    JsonlResumeState, STRICT_JSONL_BATCH_BYTES, TranscriptDiscoveryBounds, TranscriptIngestError,
    TranscriptSource,
};

pub const CLAUDE_TRANSCRIPT_RETENTION_CLASS: &str = "transcript.claude.v1";
/// Every pass, including startup recovery, bounds its raw and parsed backlog.
pub const CLAUDE_HOOK_MAX_NEW_BYTES: u64 = STRICT_JSONL_BATCH_BYTES;
const CLAUDE_RECOVERY_MAX_NEW_BYTES: u64 = STRICT_JSONL_BATCH_BYTES * 8;
const MAX_CLAUDE_SOURCES_PER_PASS: usize = 64;
const CLAUDE_SOURCE_FRONTIER_KEY: &str =
    "tracedecay-internal:claude-observation-source-frontier:v1";
const MAX_PROJECTIONS_PER_PASS: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClaudeObservationIngestStats {
    pub transcript: TranscriptIngestStats,
    pub observations_committed: u64,
    pub observation_duplicates: u64,
    pub cursor_advances: u64,
    pub cursor_duplicates: u64,
    pub records_rejected: u64,
    pub records_quarantined: u64,
    pub projections_completed: u64,
    pub projection_outputs: u64,
    pub projections_skipped: u64,
    pub projection_duplicates: u64,
    pub deferred_sources: u64,
    pub source_bytes_scanned: u64,
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
        self.projection_outputs = self
            .projection_outputs
            .saturating_add(other.projection_outputs);
        self.projections_skipped = self
            .projections_skipped
            .saturating_add(other.projections_skipped);
        self.projection_duplicates = self
            .projection_duplicates
            .saturating_add(other.projection_duplicates);
        self.deferred_sources = self.deferred_sources.saturating_add(other.deferred_sources);
        self.source_bytes_scanned = self
            .source_bytes_scanned
            .saturating_add(other.source_bytes_scanned);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedClaudeFrame {
    pub committed_cursor: ClaudeSourceCursorV1,
    pub exact_duplicate: bool,
}

#[derive(Debug, Error)]
pub enum ClaudeObservationIngestError {
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
    #[error("Claude transcript ingest failed")]
    Transcript(#[from] TranscriptIngestError),
    #[error("Claude observation frame is not in the parsed state")]
    MissingParsedRecord,
    #[error("Claude observation frame rejected its sanitized replacement")]
    InvalidFrameState,
    #[error("Claude observation scanner returned non-contiguous coverage")]
    NonContiguousCoverage,
    #[error(
        "Claude observation ingestion failed for {failed_sources} source(s); first reason: {first_reason_code}"
    )]
    SourceFailures {
        failed_sources: u64,
        first_reason_code: &'static str,
        first_retryable: bool,
    },
}

enum FrameCaptureOutcome {
    Persisted(CapturedClaudeFrame),
    Rejected(SanitizationReceiptV1),
    Quarantined(SanitizationReceiptV1),
}

struct FrameCaptureContext {
    source: ClaudeSourceIdentityV1,
    scope: ObservationScopeV1,
    generation: ClaudeFileGenerationV1,
    file_identity: u64,
    retention_class: RetentionClass,
    cancellation: ObservationCancellation,
}

struct NonDurableSegment {
    covered: ClaudeByteRangeV1,
    reason: NonDurableFrameReason,
    sanitization_receipt: Option<SanitizationReceiptV1>,
    resume_fingerprint: u64,
}

enum ScannedSegment {
    Frame(Box<ClaudeSourceFrame>),
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

    fn resume_fingerprint(&self) -> u64 {
        match self {
            Self::Frame(frame) => frame.resume_fingerprint,
            Self::Skipped(frame) => frame.resume_fingerprint,
        }
    }
}

/// Converts a durable observation cursor into a provider scanner cursor.
pub fn scanner_cursor(cursor: Option<&ClaudeSourceCursorV1>) -> StoredCursor {
    cursor.map_or_else(StoredCursor::default, |cursor| StoredCursor {
        position: cursor.byte_offset(),
        mtime: 0,
        file_id: cursor.generation().file_id(),
    })
}

fn authoritative_scanner_cursor(
    legacy: StoredCursor,
    observation: Option<&ClaudeSourceCursorV1>,
) -> StoredCursor {
    // Existing installs bootstrap observation capture at the legacy V1
    // frontier once. After the first observation cursor exists, that cursor is
    // the sole scan authority; the projector is the sole V1 writer.
    observation.map_or(legacy, |cursor| scanner_cursor(Some(cursor)))
}

fn cursor_at(
    source: &ClaudeSourceIdentityV1,
    scope: &ObservationScopeV1,
    generation: ClaudeFileGenerationV1,
    offset: u64,
    resume_checkpoint: Option<(u64, u64)>,
) -> Result<ClaudeSourceCursorV1, ObservationContractError> {
    let cursor = ClaudeSourceCursorV1::new(source.clone(), scope.clone(), generation, offset)?;
    Ok(
        resume_checkpoint.map_or(cursor.clone(), |(file_identity, resume_fingerprint)| {
            cursor.with_resume_checkpoint(file_identity, resume_fingerprint)
        }),
    )
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
        return cursor_at(source, scope, generation, frame_start, None).map(Some);
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
async fn capture_frame<A: HostAdmission + ?Sized>(
    admission: &A,
    frame: &mut ClaudeSourceFrame,
    expected_cursor: Option<ClaudeSourceCursorV1>,
    context: &FrameCaptureContext,
) -> Result<FrameCaptureOutcome, ClaudeObservationIngestError> {
    let parsed_record = frame
        .take_parsed_record()
        .ok_or(ClaudeObservationIngestError::MissingParsedRecord)?;
    let native_record_id = parsed_record
        .value()
        .get("stable_record_id")
        .and_then(serde_json::Value::as_str)
        .ok_or(ClaudeObservationIngestError::InvalidFrameState)
        .and_then(|record_id| {
            ObservationId::new(record_id.to_owned()).map_err(ClaudeObservationIngestError::from)
        })?;
    let identity = ClaudeObservationIdentityMaterialV1::for_native_record(
        context.source.clone(),
        context.scope.clone(),
        context.generation,
        *parsed_record.source_range(),
        parsed_record.ordering_domain(),
        native_record_id,
    )?;
    let request = CaptureClaudeObservationRequest::new(
        parsed_record,
        identity,
        expected_cursor,
        context.retention_class.clone(),
        context.cancellation.clone(),
    )?
    .with_resume_checkpoint(context.file_identity, frame.resume_fingerprint);
    match admission
        .capture_observation(request)
        .await
        .map_err(|outcome| host_admission_error("claude", outcome))?
    {
        CaptureClaudeObservationOutcome::Persisted {
            outcome,
            sanitized_record,
            ..
        } => {
            let receipt = outcome.receipt();
            if !frame.set_sanitized_record(*sanitized_record) {
                return Err(ClaudeObservationIngestError::InvalidFrameState);
            }
            Ok(FrameCaptureOutcome::Persisted(CapturedClaudeFrame {
                committed_cursor: receipt.committed_cursor().clone(),
                exact_duplicate: matches!(
                    *outcome,
                    ObservationPersistOutcome::ExactDuplicate(_)
                        | ObservationPersistOutcome::CoveredDuplicate(_)
                ),
            }))
        }
        CaptureClaudeObservationOutcome::Rejected { receipt, .. } => {
            Ok(FrameCaptureOutcome::Rejected(receipt))
        }
        CaptureClaudeObservationOutcome::Quarantined { receipt, .. } => {
            Ok(FrameCaptureOutcome::Quarantined(receipt))
        }
    }
}

async fn advance_non_durable_covered_range<A: HostAdmission + ?Sized>(
    admission: &A,
    context: &FrameCaptureContext,
    observation_cursor: &mut Option<ClaudeSourceCursorV1>,
    segment: NonDurableSegment,
    stats: &mut ClaudeObservationIngestStats,
) -> Result<(), ClaudeObservationIngestError> {
    let NonDurableSegment {
        covered,
        reason,
        sanitization_receipt,
        resume_fingerprint,
    } = segment;
    if observation_cursor.as_ref().is_some_and(|cursor| {
        cursor.generation() == context.generation && cursor.byte_offset() >= covered.end()
    }) {
        stats.cursor_duplicates = stats.cursor_duplicates.saturating_add(1);
        return Ok(());
    }

    let end = covered.end();
    let advance = match sanitization_receipt {
        Some(receipt) => ObservationCursorAdvance::new_with_sanitization_receipt(
            context.source.clone(),
            context.scope.clone(),
            context.generation,
            observation_cursor.clone(),
            covered,
            reason,
            receipt,
        )?,
        None => ObservationCursorAdvance::new(
            context.source.clone(),
            context.scope.clone(),
            context.generation,
            observation_cursor.clone(),
            covered,
            reason,
        )?,
    }
    .with_resume_checkpoint(context.file_identity, resume_fingerprint);
    let outcome = admission
        .advance_non_durable_source_cursor(advance, context.cancellation.clone())
        .await
        .map_err(|outcome| host_admission_error("claude", outcome))?;
    *observation_cursor = Some(cursor_at(
        &context.source,
        &context.scope,
        context.generation,
        end,
        Some((context.file_identity, resume_fingerprint)),
    )?);
    match outcome {
        CursorAdvanceOutcome::Committed => {
            stats.cursor_advances = stats.cursor_advances.saturating_add(1);
        }
        CursorAdvanceOutcome::ExactDuplicate => {
            stats.cursor_duplicates = stats.cursor_duplicates.saturating_add(1);
        }
    }
    Ok(())
}

struct PreparedSource {
    capture_context: FrameCaptureContext,
    observation_cursor: Option<ClaudeSourceCursorV1>,
    segments: Vec<ScannedSegment>,
    stats: ClaudeObservationIngestStats,
}

enum SourcePreparation {
    Finished(ClaudeObservationIngestStats),
    Ready(Box<PreparedSource>),
}

fn scan_stats(coverage: ClaudeFrameCoverage, read_through: u64) -> ClaudeObservationIngestStats {
    let start_offset = match coverage {
        ClaudeFrameCoverage::Complete { start_offset, .. }
        | ClaudeFrameCoverage::Deferred { start_offset, .. } => start_offset,
    };
    ClaudeObservationIngestStats {
        deferred_sources: u64::from(matches!(coverage, ClaudeFrameCoverage::Deferred { .. })),
        source_bytes_scanned: read_through.saturating_sub(start_offset),
        ..ClaudeObservationIngestStats::default()
    }
}

fn deferred_source_stats(source_bytes_scanned: u64) -> ClaudeObservationIngestStats {
    ClaudeObservationIngestStats {
        deferred_sources: 1,
        source_bytes_scanned,
        ..ClaudeObservationIngestStats::default()
    }
}

fn scanned_segments(
    frames: Vec<ClaudeSourceFrame>,
    skipped_frames: Vec<ClaudeSkippedFrame>,
) -> Result<Vec<ScannedSegment>, ClaudeObservationIngestError> {
    let mut segments = Vec::with_capacity(frames.len() + skipped_frames.len());
    segments.extend(frames.into_iter().map(Box::new).map(ScannedSegment::Frame));
    segments.extend(skipped_frames.into_iter().map(ScannedSegment::Skipped));
    segments.sort_by_key(ScannedSegment::start);
    if segments
        .windows(2)
        .any(|pair| pair[0].end() != pair[1].start())
    {
        return Err(ClaudeObservationIngestError::NonContiguousCoverage);
    }
    Ok(segments)
}

struct SourceProcessingContext<'a, A> {
    admission: &'a A,
    source_adapter: &'a ClaudeSource,
    project_root: &'a Path,
    scope: &'a ObservationScopeV1,
    cancellation: &'a ObservationCancellation,
}

async fn prepare_source<A>(
    context: &SourceProcessingContext<'_, A>,
    path: &Path,
    max_new_bytes: Option<u64>,
) -> Result<SourcePreparation, ClaudeObservationIngestError>
where
    A: HostAdmission + ?Sized,
{
    if context.cancellation.is_cancelled() {
        return Err(ObservationApplicationError::Cancelled.into());
    }
    let identity = identify_claude_source(path).ok_or_else(|| {
        TranscriptIngestError::InvalidSourceIdentity {
            provider: "claude",
            path: path.to_path_buf(),
        }
    })?;
    let source = ClaudeSourceIdentityV1::for_source(
        SessionId::new(identity.session_id.clone())?,
        SessionId::new(identity.source_id.clone())?,
    )?;
    let cursor_path = identity.cursor_key.store_path();
    let durable_cursor = context
        .admission
        .get_parse_offset(context.scope, cursor_path.to_string_lossy().as_ref())
        .await
        .map_err(|outcome| host_admission_error("claude", outcome))?
        .unwrap_or_default();
    let observation_cursor = context
        .admission
        .get_source_cursor(&source, context.scope)
        .await
        .map_err(|outcome| host_admission_error("claude", outcome))?;
    let previous = authoritative_scanner_cursor(
        StoredCursor {
            position: durable_cursor.byte_offset,
            mtime: durable_cursor.mtime,
            file_id: durable_cursor.file_id,
        },
        observation_cursor.as_ref(),
    );
    let resume_state = observation_cursor.as_ref().and_then(|cursor| {
        Some(JsonlResumeState {
            generation: cursor.generation().file_id(),
            file_identity: cursor.file_identity()?,
            fingerprint: cursor.resume_fingerprint()?,
        })
    });
    let Some(mut scan) =
        try_scan_claude_source_frames_with_resume(identity, previous, max_new_bytes, resume_state)?
    else {
        return Ok(SourcePreparation::Finished(
            ClaudeObservationIngestStats::default(),
        ));
    };
    let coverage = scan.coverage;
    let stats = scan_stats(coverage, scan.read_through);
    if matches!(
        coverage,
        ClaudeFrameCoverage::Deferred {
            start_offset,
            covered_through,
            ..
        } if start_offset == covered_through
    ) {
        return Ok(SourcePreparation::Finished(deferred_source_stats(
            stats.source_bytes_scanned,
        )));
    }
    if let ClaudeFrameCoverage::Deferred {
        start_offset,
        covered_through,
        ..
    } = coverage
    {
        // Scope filtering historically treated every backlog deferral as an
        // empty scan. A progressive bounded batch is complete through its
        // covered prefix; restore the deferral after filtering for accounting.
        scan.coverage = ClaudeFrameCoverage::Complete {
            start_offset,
            end_offset: covered_through,
        };
    }
    let retained = context
        .source_adapter
        .retain_scoped_frames(&mut scan, context.project_root);
    scan.coverage = coverage;
    if retained.is_none() {
        return Ok(SourcePreparation::Finished(deferred_source_stats(
            stats.source_bytes_scanned,
        )));
    }

    let generation = ClaudeFileGenerationV1::new(scan.file_generation)?;
    let retention_class = RetentionClass::new(CLAUDE_TRANSCRIPT_RETENTION_CLASS)?;
    let capture_context = FrameCaptureContext {
        source,
        scope: context.scope.clone(),
        generation,
        file_identity: scan.file_identity,
        retention_class,
        cancellation: context.cancellation.clone(),
    };
    let segments = scanned_segments(
        std::mem::take(&mut scan.frames),
        std::mem::take(&mut scan.skipped_frames),
    )?;
    Ok(SourcePreparation::Ready(Box::new(PreparedSource {
        capture_context,
        observation_cursor,
        segments,
        stats,
    })))
}

async fn apply_scanned_segment<A: HostAdmission + ?Sized>(
    admission: &A,
    capture_context: &FrameCaptureContext,
    observation_cursor: &mut Option<ClaudeSourceCursorV1>,
    segment: ScannedSegment,
    stats: &mut ClaudeObservationIngestStats,
) -> Result<bool, ClaudeObservationIngestError> {
    let resume_fingerprint = segment.resume_fingerprint();
    match segment {
        ScannedSegment::Skipped(skipped) => {
            let covered = ClaudeByteRangeV1::new(skipped.offset, skipped.end_offset)?;
            let reason = match skipped.reason {
                ClaudeSkippedFrameReason::Whitespace => NonDurableFrameReason::BlankFrame,
                ClaudeSkippedFrameReason::OutOfScope => NonDurableFrameReason::OutOfScope,
                ClaudeSkippedFrameReason::Malformed | ClaudeSkippedFrameReason::Oversized => {
                    stats.deferred_sources = 1;
                    return Ok(false);
                }
            };
            advance_non_durable_covered_range(
                admission,
                capture_context,
                observation_cursor,
                NonDurableSegment {
                    covered,
                    reason,
                    sanitization_receipt: None,
                    resume_fingerprint,
                },
                stats,
            )
            .await?;
        }
        ScannedSegment::Frame(mut frame) => {
            let expected = expected_cursor_for_frame(
                observation_cursor.as_ref(),
                &capture_context.source,
                &capture_context.scope,
                capture_context.generation,
                frame.offset,
            )?;
            let range = ClaudeByteRangeV1::new(frame.offset, frame.end_offset)?;
            match capture_frame(admission, &mut frame, expected, capture_context).await? {
                FrameCaptureOutcome::Persisted(captured) => {
                    *observation_cursor = Some(cursor_after_receipt(
                        observation_cursor.take(),
                        &captured.committed_cursor,
                    ));
                    if captured.exact_duplicate {
                        stats.observation_duplicates =
                            stats.observation_duplicates.saturating_add(1);
                    } else {
                        stats.observations_committed =
                            stats.observations_committed.saturating_add(1);
                    }
                }
                FrameCaptureOutcome::Rejected(receipt) => {
                    stats.records_rejected = stats.records_rejected.saturating_add(1);
                    advance_non_durable_covered_range(
                        admission,
                        capture_context,
                        observation_cursor,
                        NonDurableSegment {
                            covered: range,
                            reason: NonDurableFrameReason::SanitizerRejected,
                            sanitization_receipt: Some(receipt),
                            resume_fingerprint,
                        },
                        stats,
                    )
                    .await?;
                }
                FrameCaptureOutcome::Quarantined(receipt) => {
                    stats.records_quarantined = stats.records_quarantined.saturating_add(1);
                    advance_non_durable_covered_range(
                        admission,
                        capture_context,
                        observation_cursor,
                        NonDurableSegment {
                            covered: range,
                            reason: NonDurableFrameReason::SanitizerQuarantined,
                            sanitization_receipt: Some(receipt),
                            resume_fingerprint,
                        },
                        stats,
                    )
                    .await?;
                }
            }
        }
    }
    Ok(true)
}

async fn apply_prepared_source<A: HostAdmission + ?Sized>(
    prepared: PreparedSource,
    admission: &A,
    cancellation: &ObservationCancellation,
) -> Result<ClaudeObservationIngestStats, ClaudeObservationIngestError> {
    let PreparedSource {
        capture_context,
        mut observation_cursor,
        segments,
        mut stats,
    } = prepared;
    for segment in segments {
        if cancellation.is_cancelled() {
            return Err(ObservationApplicationError::Cancelled.into());
        }
        if !apply_scanned_segment(
            admission,
            &capture_context,
            &mut observation_cursor,
            segment,
            &mut stats,
        )
        .await?
        {
            break;
        }
    }
    Ok(stats)
}

async fn process_source<A>(
    context: &SourceProcessingContext<'_, A>,
    path: &Path,
    max_new_bytes: Option<u64>,
) -> Result<ClaudeObservationIngestStats, ClaudeObservationIngestError>
where
    A: HostAdmission + ?Sized,
{
    match prepare_source(context, path, max_new_bytes).await? {
        SourcePreparation::Finished(stats) => Ok(stats),
        SourcePreparation::Ready(prepared) => {
            apply_prepared_source(*prepared, context.admission, context.cancellation).await
        }
    }
}

pub async fn drain_projection_queue<A: HostAdmission + ?Sized>(
    admission: &A,
    scope: &ObservationScopeV1,
    cancellation: &ObservationCancellation,
) -> Result<ClaudeObservationIngestStats, ClaudeObservationIngestError> {
    let outcome = admission
        .drain_projection_queue("claude", scope, cancellation, MAX_PROJECTIONS_PER_PASS)
        .await
        .map_err(|outcome| host_admission_error("claude", outcome))?;
    Ok(ClaudeObservationIngestStats {
        transcript: TranscriptIngestStats {
            sessions_upserted: u64::try_from(outcome.session_ids.len()).unwrap_or(u64::MAX),
            messages_upserted: outcome.projected,
        },
        projections_completed: outcome.projected,
        projection_outputs: outcome.projected_outputs,
        projections_skipped: outcome.skipped,
        projection_duplicates: outcome.exact_duplicates,
        ..ClaudeObservationIngestStats::default()
    })
}

fn frontier_store_error(
    operation: &'static str,
    error: impl std::fmt::Debug,
) -> ClaudeObservationIngestError {
    TranscriptIngestError::Store(TranscriptStoreError::Storage {
        operation,
        source: Box::new(std::io::Error::other(format!("{error:?}"))),
    })
    .into()
}

async fn scheduled_source_paths<A: HostAdmission + ?Sized>(
    admission: &A,
    scope: &ObservationScopeV1,
    source: &ClaudeSource,
    project_root: &Path,
) -> Result<(Vec<PathBuf>, usize), ClaudeObservationIngestError> {
    let discovery =
        source.discover_transcript_paths(project_root, TranscriptDiscoveryBounds::default_walk());
    let discovery_truncated = discovery.is_truncated();
    let mut paths = discovery.paths;
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return Ok((paths, 0));
    }
    let frontier = admission
        .get_parse_offset(scope, CLAUDE_SOURCE_FRONTIER_KEY)
        .await
        .map_err(|error| frontier_store_error("read Claude source frontier", error))?
        .unwrap_or_default();
    let start = usize::try_from(frontier.byte_offset).unwrap_or(usize::MAX) % paths.len();
    paths.rotate_left(start);
    let mut deferred = paths.len().saturating_sub(MAX_CLAUDE_SOURCES_PER_PASS);
    if discovery_truncated {
        deferred = deferred.max(1);
    }
    paths.truncate(MAX_CLAUDE_SOURCES_PER_PASS);
    Ok((paths, deferred))
}

async fn advance_source_frontier<A: HostAdmission + ?Sized>(
    admission: &A,
    scope: &ObservationScopeV1,
    processed: usize,
) -> Result<(), ClaudeObservationIngestError> {
    if processed == 0 {
        return Ok(());
    }
    let previous = admission
        .get_parse_offset(scope, CLAUDE_SOURCE_FRONTIER_KEY)
        .await
        .map_err(|error| frontier_store_error("read Claude source frontier", error))?
        .unwrap_or_default();
    let processed = u64::try_from(processed).unwrap_or(u64::MAX);
    admission
        .advance_parse_offset(
            scope,
            CLAUDE_SOURCE_FRONTIER_KEY,
            ParseOffset {
                byte_offset: previous.byte_offset.saturating_add(processed),
                mtime: 0,
                file_id: 1,
            },
        )
        .await
        .map_err(|error| frontier_store_error("advance Claude source frontier", error))
}

/// Ingest one Claude source through caller-prepared project admission authority.
pub async fn ingest_source_with_observations_with_admission<A>(
    source: &ClaudeSource,
    project_root: &Path,
    scope: ObservationScopeV1,
    admission: &A,
    max_new_bytes: Option<u64>,
    cancellation: ObservationCancellation,
) -> Result<ClaudeObservationIngestStats, ClaudeObservationIngestError>
where
    A: HostAdmission + ?Sized,
{
    if cancellation.is_cancelled() {
        return Err(ObservationApplicationError::Cancelled.into());
    }
    let processing_context = SourceProcessingContext {
        admission,
        source_adapter: source,
        project_root,
        scope: &scope,
        cancellation: &cancellation,
    };
    let (paths, deferred) = scheduled_source_paths(admission, &scope, source, project_root).await?;
    let scheduled_source_count = paths.len();
    let mut stats = ClaudeObservationIngestStats {
        deferred_sources: u64::try_from(deferred).unwrap_or(u64::MAX),
        ..ClaudeObservationIngestStats::default()
    };
    let mut remaining_bytes = max_new_bytes.unwrap_or(CLAUDE_RECOVERY_MAX_NEW_BYTES);
    let mut attempted_sources = 0usize;
    let mut source_failures = None;
    for path in paths {
        if remaining_bytes == 0 {
            stats.deferred_sources = stats.deferred_sources.saturating_add(1);
            continue;
        }
        attempted_sources = attempted_sources.saturating_add(1);
        let source_budget = remaining_bytes.min(STRICT_JSONL_BATCH_BYTES);
        remaining_bytes = remaining_bytes.saturating_sub(source_budget);
        let outcome = match process_source(&processing_context, &path, Some(source_budget)).await {
            Ok(outcome) => outcome,
            Err(error) => {
                let failure = crate::runtime::classify_claude_observation_failure(&error);
                let summary =
                    source_failures.get_or_insert((0_u64, failure.reason_code, failure.retryable));
                summary.0 = summary.0.saturating_add(1);
                tracing::warn!(
                    reason_code = failure.reason_code,
                    retryable = failure.retryable,
                    "Claude observation source ingest failed"
                );
                continue;
            }
        };
        if outcome.source_bytes_scanned <= source_budget {
            remaining_bytes =
                remaining_bytes.saturating_add(source_budget - outcome.source_bytes_scanned);
        } else {
            remaining_bytes =
                remaining_bytes.saturating_sub(outcome.source_bytes_scanned - source_budget);
        }
        stats = stats.merge(outcome);
    }
    if deferred > 0 || attempted_sources < scheduled_source_count {
        advance_source_frontier(admission, &scope, attempted_sources).await?;
    }
    let projection_stats = drain_projection_queue(admission, &scope, &cancellation).await?;
    if let Some((failed_sources, first_reason_code, first_retryable)) = source_failures {
        return Err(ClaudeObservationIngestError::SourceFailures {
            failed_sources,
            first_reason_code,
            first_retryable,
        });
    }
    Ok(stats.merge(projection_stats))
}

pub async fn ingest_user_sessions_with_admission<A>(
    profile_root: &Path,
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
    admission: &A,
    max_new_bytes: Option<u64>,
    cancellation: ObservationCancellation,
) -> Result<ClaudeObservationIngestStats, ClaudeObservationIngestError>
where
    A: HostAdmission + ?Sized,
{
    let Some(source) = ClaudeSource::new() else {
        return Ok(ClaudeObservationIngestStats::default());
    };
    let source = source.for_user_scope(session_id, registered_roots);
    ingest_source_with_observations_with_admission(
        &source,
        profile_root,
        ObservationScopeV1::Profile,
        admission,
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
    use tracedecay_store::{
        ObservationProjectionStore, ObservationReplayRequest, ObservationStore,
    };

    use super::*;
    use std::future::Future;

    use crate::admission::{
        HostAdmissionOutcome, HostAdmissionScope, HostAdmissionTestRuntimeV1,
        HostProjectionDrainOutcome, HostAdmission, HostAdmission,
    };
    use crate::observation::{
        CaptureObservationOutcome, CaptureObservationRequest, ObservationApplication,
        ReplayObservationsRequest,
    };
    use tracedecay_runtime_core::privacy::ClaudeRecordSanitizerV1;
    use crate::runtime::claude::{scan_claude_source_frames, try_scan_claude_source_frames};
    use tracedecay_domain::{ObservationSourceCursorV1, ObservationSourceIdentityV1};
    use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};
    const INGEST_STATE_TABLES: &[&str] = &[
        "sanitization_receipts",
        "observations",
        "source_cursors",
        "source_cursor_advances",
        "projection_queue",
        "observation_projection_checkpoints",
        "observation_projection_provenance",
        "sessions",
        "session_messages",
        "session_messages_fts",
    ];

    #[derive(Default)]
    struct CapturePortSpy {
        capture_calls: std::sync::atomic::AtomicUsize,
        cursor_reads: std::sync::atomic::AtomicUsize,
        drain_calls: std::sync::atomic::AtomicUsize,
        last_drain_provider: std::sync::Mutex<Option<String>>,
        last_drain_max: std::sync::atomic::AtomicUsize,
    }

    impl HostAdmission for CapturePortSpy {
        fn capture_observation(
            &self,
            _request: CaptureObservationRequest,
        ) -> impl Future<Output = Result<CaptureObservationOutcome, HostAdmissionOutcome>> + Send
        {
            self.capture_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async {
                Err(HostAdmissionOutcome::retained_unavailable(
                    "capture_port_spy",
                ))
            }
        }

        fn advance_non_durable_source_cursor(
            &self,
            _advance: ObservationCursorAdvance,
            _cancellation: ObservationCancellation,
        ) -> impl Future<Output = Result<CursorAdvanceOutcome, HostAdmissionOutcome>> + Send
        {
            async { Err(HostAdmissionOutcome::retained_unavailable("unused")) }
        }

        fn get_source_cursor<'a>(
            &'a self,
            _source: &'a ObservationSourceIdentityV1,
            _scope: &'a ObservationScopeV1,
        ) -> impl Future<
            Output = Result<Option<ObservationSourceCursorV1>, HostAdmissionOutcome>,
        > + Send
        + 'a {
            async { Ok(None) }
        }

        fn drain_projection_queue<'a>(
            &'a self,
            provider: &'a str,
            _scope: &'a ObservationScopeV1,
            _cancellation: &'a ObservationCancellation,
            max: usize,
        ) -> impl Future<Output = Result<HostProjectionDrainOutcome, HostAdmissionOutcome>> + Send + 'a
        {
            self.drain_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.last_drain_max
                .store(max, std::sync::atomic::Ordering::SeqCst);
            *self
                .last_drain_provider
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(provider.to_string());
            async {
                Err(HostAdmissionOutcome::retained_unavailable(
                    "projection_drain_spy",
                ))
            }
        }
    }

    impl HostAdmission for CapturePortSpy {
        fn get_parse_offset<'a>(
            &'a self,
            _scope: &'a ObservationScopeV1,
            _path: &'a str,
        ) -> impl Future<Output = Result<Option<ParseOffset>, HostAdmissionOutcome>> + Send + 'a
        {
            self.cursor_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Ok(None) }
        }

        fn advance_parse_offset<'a>(
            &'a self,
            _scope: &'a ObservationScopeV1,
            _path: &'a str,
            _offset: ParseOffset,
        ) -> impl Future<Output = Result<(), HostAdmissionOutcome>> + Send + 'a {
            async { Ok(()) }
        }
    }

    #[tokio::test]
    async fn capture_frame_routes_through_observation_capture_port() {
        let fixture = Fixture::new("port-spy-session").await;
        fixture.write_record("port spy content", "port-spy-secret");
        let identity = identify_claude_source(&fixture.transcript).unwrap();
        let mut scan = scan_claude_source_frames(identity.clone(), StoredCursor::default(), None)
            .expect("scan complete spy frame");
        let source = ClaudeSourceIdentityV1::for_source(
            SessionId::new(identity.session_id).unwrap(),
            SessionId::new(identity.source_id).unwrap(),
        )
        .unwrap();
        let spy = CapturePortSpy::default();
        let result = capture_frame(
            &spy,
            scan.frames.first_mut().expect("spy frame"),
            None,
            &FrameCaptureContext {
                source,
                scope: ObservationScopeV1::Profile,
                generation: ClaudeFileGenerationV1::new(scan.file_generation).unwrap(),
                file_identity: scan.file_identity,
                retention_class: RetentionClass::new(CLAUDE_TRANSCRIPT_RETENTION_CLASS).unwrap(),
                cancellation: ObservationCancellation::default(),
            },
        )
        .await;

        assert_eq!(
            spy.capture_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        match result {
            Err(ClaudeObservationIngestError::Transcript(
                crate::runtime::source::TranscriptIngestError::NonDurableRecord { reason, .. },
            )) => assert_eq!(reason, "capture_port_spy"),
            Ok(_) => panic!("spy must reject capture through the admission port"),
            Err(other) => panic!("capture_frame must surface the capture-port rejection: {other}"),
        }
    }

    #[tokio::test]
    async fn scheduled_source_paths_read_cursor_through_transcript_cursor_port() {
        let fixture = Fixture::new("cursor-port-session").await;
        fixture.write_record("cursor port content", "cursor-port-secret");
        let source = fixture.source("cursor-port-session");
        let spy = CapturePortSpy::default();
        let (paths, _deferred) = scheduled_source_paths(
            &spy,
            &ObservationScopeV1::Profile,
            &source,
            &fixture.profile,
        )
        .await
        .expect("cursor port admits scheduling");
        assert!(
            !paths.is_empty(),
            "fixture transcript must be discoverable for cursor-port coverage"
        );
        assert!(
            spy.cursor_reads.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "scheduling must read the durable frontier through HostAdmission"
        );
    }

    #[tokio::test]
    async fn drain_projection_queue_routes_through_observation_capture_port() {
        let spy = CapturePortSpy::default();
        let scope = ObservationScopeV1::Profile;
        let cancellation = ObservationCancellation::default();
        let result = drain_projection_queue(&spy, &scope, &cancellation).await;

        assert_eq!(spy.drain_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            spy.last_drain_max.load(std::sync::atomic::Ordering::SeqCst),
            MAX_PROJECTIONS_PER_PASS
        );
        assert_eq!(
            spy.last_drain_provider
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_deref(),
            Some("claude")
        );
        match result {
            Err(ClaudeObservationIngestError::Transcript(
                crate::runtime::source::TranscriptIngestError::NonDurableRecord { reason, .. },
            )) => assert_eq!(reason, "projection_drain_spy"),
            Ok(_) => panic!("spy must reject projection drain through the admission port"),
            Err(other) => {
                panic!("drain_projection_queue must surface the drain-port rejection: {other}")
            }
        }
    }

    struct Fixture {
        temp: TempDir,
        home: PathBuf,
        profile: PathBuf,
        transcript: PathBuf,
        runtime: HostAdmissionTestRuntimeV1,
    }

    impl Fixture {
        async fn new(session_id: &str) -> Self {
            let temp = TempDir::new().expect("temporary observation fixture");
            let home = temp.path().join("home");
            Self::new_with_temp_and_home(session_id, temp, home).await
        }

        async fn new_in_home(session_id: &str, home: PathBuf) -> Self {
            let temp = TempDir::new().expect("temporary observation fixture");
            Self::new_with_temp_and_home(session_id, temp, home).await
        }

        async fn new_with_temp_and_home(session_id: &str, temp: TempDir, home: PathBuf) -> Self {
            let profile = home.join(".tracedecay");
            let transcript = home
                .join(".claude/projects/project-scope")
                .join(format!("{session_id}.jsonl"));
            fs::create_dir_all(transcript.parent().expect("transcript parent"))
                .expect("create Claude fixture tree");
            fs::create_dir_all(&profile).expect("create profile root");
            let runtime = HostAdmissionTestRuntimeV1::profile(&profile)
                .await
                .expect("open registered observation runtime");
            Self {
                temp,
                home,
                profile,
                transcript,
                runtime,
            }
        }

        fn registered(&self) -> &tracedecay_global_db::RegisteredGlobalDb {
            self.runtime
                .registered_database(HostAdmissionScope::Profile)
                .expect("registered profile session database")
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
            let admission = self.runtime.facade();
            ingest_source_with_observations_with_admission(
                source,
                &self.profile,
                ObservationScopeV1::Profile,
                &admission,
                max_new_bytes,
                cancellation,
            )
            .await
        }
    }

    async fn ingest_state_counts(fixture: &Fixture) -> Vec<i64> {
        let snapshot = fixture
            .runtime
            .read_snapshot(HostAdmissionScope::Profile)
            .await
            .expect("open registered observation state snapshot");
        let mut counts = Vec::with_capacity(INGEST_STATE_TABLES.len());
        for table in INGEST_STATE_TABLES {
            let mut rows = snapshot
                .query(&format!("SELECT COUNT(*) FROM {table}"), ())
                .await
                .expect("count observation state rows");
            counts.push(
                rows.next()
                    .await
                    .expect("read observation state count")
                    .expect("observation state count row")
                    .get(0)
                    .expect("decode observation state count"),
            );
        }
        counts
    }

    async fn persisted_observation_authority_json(fixture: &Fixture) -> Vec<String> {
        let snapshot = fixture
            .runtime
            .read_snapshot(HostAdmissionScope::Profile)
            .await
            .expect("open registered observation authority snapshot");
        let mut rows = snapshot
            .query(
                "SELECT observation_json FROM observations
                 UNION ALL SELECT receipt_json FROM sanitization_receipts
                 UNION ALL SELECT source_json FROM source_cursors",
                (),
            )
            .await
            .expect("read observation authority JSON");
        let mut documents = Vec::new();
        while let Some(row) = rows.next().await.expect("read authority JSON row") {
            documents.push(row.get(0).expect("decode authority JSON"));
        }
        documents
    }

    async fn matching_message_count(fixture: &Fixture, marker: &str) -> i64 {
        let snapshot = fixture.registered().read_snapshot().await.unwrap();
        let mut rows = snapshot
            .query(
                "SELECT COUNT(*) FROM session_messages
                 WHERE provider = 'claude' AND role = 'user' AND text LIKE ?1",
                (format!("%{marker}%"),),
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    fn observation_source(path: &Path) -> ClaudeSourceIdentityV1 {
        let identity = identify_claude_source(path).expect("Claude source identity");
        ClaudeSourceIdentityV1::for_source(
            SessionId::new(identity.session_id).unwrap(),
            SessionId::new(identity.source_id).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn observation_cursor_becomes_the_only_scan_authority_after_bootstrap() {
        let legacy = StoredCursor {
            position: 800,
            mtime: 17,
            file_id: 41,
        };
        assert_eq!(authoritative_scanner_cursor(legacy, None), legacy);

        let source =
            ClaudeSourceIdentityV1::new(SessionId::new("cursor-authority").unwrap()).unwrap();
        let observation = ClaudeSourceCursorV1::new(
            source,
            ObservationScopeV1::Profile,
            ClaudeFileGenerationV1::new(73).unwrap(),
            1_200,
        )
        .unwrap();
        assert_eq!(
            authoritative_scanner_cursor(legacy, Some(&observation)),
            StoredCursor {
                position: 1_200,
                mtime: 0,
                file_id: 73,
            }
        );
    }

    async fn assert_invalid_frame_preserves_observation_state(session_id: &str, frame: &[u8]) {
        let fixture = Fixture::new(session_id).await;
        fs::write(&fixture.transcript, frame).expect("write invalid Claude frame");
        let source_adapter = fixture.source(session_id);
        let source = observation_source(&fixture.transcript);
        let store = fixture
            .runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let before = ingest_state_counts(&fixture).await;

        let stats = fixture
            .ingest(&source_adapter, None, ObservationCancellation::default())
            .await
            .expect("invalid frame must defer without mutating observation state");

        assert_eq!(stats.observations_committed, 0);
        assert_eq!(stats.observation_duplicates, 0);
        assert_eq!(stats.cursor_advances, 0);
        assert_eq!(stats.projections_completed, 0);
        assert_eq!(stats.deferred_sources, 1);
        assert_eq!(stats.transcript, TranscriptIngestStats::default());
        let after = ingest_state_counts(&fixture).await;
        assert_eq!(after, before);
        assert!(
            store
                .get_source_cursor(&source, &ObservationScopeV1::Profile)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(store.next_queued_observation().await.unwrap().is_none());
        assert_eq!(
            store.projection_checkpoint().await.unwrap().last_sequence(),
            0
        );
    }

    async fn assert_invalid_suffix_preserves_valid_prefix(session_id: &str, suffix: &[u8]) {
        let fixture = Fixture::new(session_id).await;
        let marker = format!("valid prefix before {session_id}");
        let record = json!({
            "type": "user",
            "sessionId": session_id,
            "uuid": format!("message-{session_id}"),
            "timestamp": "2026-07-15T00:00:00Z",
            "cwd": fixture.temp.path(),
            "message": { "role": "user", "content": marker },
        });
        let mut bytes = format!("{record}\n").into_bytes();
        let suffix_start = u64::try_from(bytes.len()).unwrap();
        bytes.extend_from_slice(suffix);
        fs::write(&fixture.transcript, bytes).expect("write valid prefix and invalid suffix");

        let source_adapter = fixture.source(session_id);
        let source = observation_source(&fixture.transcript);
        let first = fixture
            .ingest(&source_adapter, None, ObservationCancellation::default())
            .await
            .expect("valid prefix must commit before invalid suffix defers");
        assert_eq!(first.observations_committed, 1);
        assert_eq!(first.transcript.messages_upserted, 1);
        assert_eq!(first.projections_completed, 1);
        assert_eq!(first.deferred_sources, 1);

        let store = fixture
            .runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let source_cursor = store
            .get_source_cursor(&source, &ObservationScopeV1::Profile)
            .await
            .unwrap()
            .expect("valid prefix source cursor");
        assert_eq!(source_cursor.byte_offset(), suffix_start);
        let identity = identify_claude_source(&fixture.transcript).unwrap();
        let cursor_path = identity.cursor_key.store_path();
        let transcript_cursor = fixture
            .registered()
            .get_parse_offset_result(cursor_path.to_string_lossy().as_ref())
            .await
            .expect("read valid prefix transcript cursor")
            .unwrap_or_default();
        assert_eq!(
            transcript_cursor.byte_offset, 0,
            "observation ingestion must not advance the legacy V1 cursor"
        );
        assert_eq!(
            store
                .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(matching_message_count(&fixture, &marker).await, 1);

        let committed = ingest_state_counts(&fixture).await;
        let retry = fixture
            .ingest(&source_adapter, None, ObservationCancellation::default())
            .await
            .expect("invalid suffix retry must remain deferred");
        assert_eq!(retry.deferred_sources, 1);
        assert_eq!(retry.transcript, TranscriptIngestStats::default());
        assert_eq!(ingest_state_counts(&fixture).await, committed);
    }

    #[tokio::test]
    async fn production_vertical_persists_only_sanitized_payload_and_searchable_v1_row() {
        let fixture = Fixture::new("production-session").await;
        fixture.write_record(
            "production vertical searchable",
            "never-persist-this-secret",
        );
        let source = fixture.source("production-session");
        assert_eq!(
            source.transcript_paths(&fixture.profile),
            vec![fixture.transcript.clone()]
        );
        let admission = fixture.runtime.facade();
        let (scheduled, deferred) = scheduled_source_paths(
            &admission,
            &ObservationScopeV1::Profile,
            &source,
            &fixture.profile,
        )
        .await
        .unwrap();
        assert_eq!(scheduled, vec![fixture.transcript.clone()]);
        assert_eq!(deferred, 0);
        let identity = identify_claude_source(&fixture.transcript).unwrap();
        let scan = try_scan_claude_source_frames(
            identity,
            StoredCursor::default(),
            Some(STRICT_JSONL_BATCH_BYTES),
        )
        .unwrap()
        .unwrap();
        assert_eq!(scan.frames.len(), 1);

        let stats = fixture
            .ingest(&source, None, ObservationCancellation::default())
            .await
            .expect("ingest production Claude observation");

        assert_eq!(stats.observations_committed, 1, "{stats:?}");
        assert_eq!(stats.transcript.sessions_upserted, 1);
        assert_eq!(stats.transcript.messages_upserted, 1);
        assert_eq!(stats.projections_completed, 1);
        assert_eq!(stats.deferred_sources, 0, "{stats:?}");
        assert!(
            fixture
                .registered()
                .get_parse_offset_result(CLAUDE_SOURCE_FRONTIER_KEY)
                .await
                .unwrap()
                .is_none(),
            "a fully covered source set does not need a durable scheduling frontier"
        );
        let store = fixture
            .runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let observations = store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap();
        assert_eq!(observations.len(), 1);
        let payload = observations[0].observation().payload();
        let payload = payload.to_string();
        assert!(!payload.contains("never-persist-this-secret"));
        assert_eq!(
            observations[0].projection_status(),
            tracedecay_store::ObservationProjectionStatus::NotQueued
        );
        let canonical_transcript = std::fs::canonicalize(&fixture.transcript).unwrap();
        let authority_json = persisted_observation_authority_json(&fixture).await;
        assert_eq!(authority_json.len(), 3);
        assert!(authority_json.iter().all(|document| {
            !document.contains(canonical_transcript.to_string_lossy().as_ref())
        }));
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;

            let raw_path_hex = hex::encode(canonical_transcript.as_os_str().as_bytes());
            assert!(
                authority_json
                    .iter()
                    .all(|document| !document.contains(&raw_path_hex))
            );
        }
        let hits = matching_message_count(&fixture, "production vertical searchable").await;
        assert_eq!(hits, 1);
        let cursor = store
            .get_source_cursor(
                observations[0].observation().source(),
                &ObservationScopeV1::Profile,
            )
            .await
            .unwrap()
            .expect("durable observation cursor");
        assert_eq!(cursor.file_identity(), Some(scan.file_identity));
        assert_eq!(
            cursor.resume_fingerprint(),
            Some(scan.frames[0].resume_fingerprint)
        );

        let committed = ingest_state_counts(&fixture).await;
        let retry = fixture
            .ingest(&source, None, ObservationCancellation::default())
            .await
            .expect("retry must resume from the observation cursor without a collision");
        assert_eq!(retry.transcript, TranscriptIngestStats::default());
        assert_eq!(retry.observations_committed, 0);
        assert_eq!(retry.observation_duplicates, 0);
        assert_eq!(retry.projections_completed, 0);
        assert_eq!(retry.projection_duplicates, 0);
        assert_eq!(retry.source_bytes_scanned, 0);
        assert_eq!(ingest_state_counts(&fixture).await, committed);
        assert_eq!(
            matching_message_count(&fixture, "production vertical searchable").await,
            hits
        );
    }

    #[tokio::test]
    async fn native_observation_id_survives_identical_transcript_relocation() {
        let fixture = Fixture::new("relocated-native-session").await;
        fixture.write_record("relocated native observation", "relocation-secret");
        let source = fixture.source("relocated-native-session");

        let first = fixture
            .ingest(&source, None, ObservationCancellation::default())
            .await
            .unwrap();
        assert_eq!(first.observations_committed, 1);
        let store = fixture
            .runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let before = store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap();
        let before_id = before[0].observation().observation_id().clone();

        let relocated = fixture
            .home
            .join(".claude/projects/relocated-scope/relocated-native-session.jsonl");
        fs::create_dir_all(relocated.parent().unwrap()).unwrap();
        fs::copy(&fixture.transcript, &relocated).unwrap();
        fs::remove_file(&fixture.transcript).unwrap();

        let second = fixture
            .ingest(&source, None, ObservationCancellation::default())
            .await
            .unwrap();
        assert_eq!(second.observations_committed, 0);
        assert_eq!(second.observation_duplicates, 1);
        let after = store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].observation().observation_id(), &before_id);
    }

    #[tokio::test]
    async fn persistence_failure_charges_the_source_scan_budget() {
        let fixture = Fixture::new("budget-failure-0").await;
        fixture.write_record(
            "first source is deliberately longer than the later source",
            "budget-failure-secret",
        );
        let later = fixture
            .transcript
            .parent()
            .unwrap()
            .join("budget-failure-1.jsonl");
        fs::write(
            &later,
            format!(
                "{}\n",
                json!({
                    "type": "user",
                    "sessionId": "budget-failure-1",
                    "uuid": "budget-failure-message-1",
                    "timestamp": "2026-07-15T00:00:00Z",
                    "message": {"role": "user", "content": "short"}
                })
            ),
        )
        .unwrap();
        let budget = fs::metadata(&fixture.transcript).unwrap().len();
        assert!(fs::metadata(&later).unwrap().len() < budget);
        let connection = rusqlite::Connection::open(
            fixture
                .runtime
                .database_path(HostAdmissionScope::Profile)
                .unwrap(),
        )
        .unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER fail_observation_insert
                 BEFORE INSERT ON observations
                 BEGIN
                     SELECT RAISE(ABORT, 'forced observation failure');
                 END;",
            )
            .unwrap();
        drop(connection);
        let source = ClaudeSource::with_home(&fixture.home).for_user_scope(None, Vec::new());

        let error = fixture
            .ingest(&source, Some(budget), ObservationCancellation::default())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ClaudeObservationIngestError::SourceFailures {
                failed_sources: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn registered_claude_ingest_api_routes_through_observation_authority() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let profile_root = tracedecay_runtime_core::storage::default_profile_root().unwrap();
        let fixture = Fixture::new_in_home(
            "legacy-api-session",
            profile_root.parent().unwrap().to_path_buf(),
        )
        .await;
        fixture.write_record("legacy API searchable", "legacy-api-secret");
        let admission = fixture.runtime.facade();
        let stats = crate::runtime::claude::ingest_user_sessions_with_admission(
            &fixture.profile,
            Some("legacy-api-session".to_string()),
            Vec::new(),
            &admission,
        )
        .await;

        assert_eq!(stats.messages_upserted, 1);
        let state = ingest_state_counts(&fixture).await;
        assert_eq!(state[0], 1, "sanitization receipt");
        assert_eq!(state[1], 1, "durable observation");
        assert_eq!(state[2], 1, "observation source cursor");
        assert_eq!(state[5], 1, "projection checkpoint");
        assert_eq!(state[6], 1, "projection provenance");
        assert_eq!(state[7], 1, "projected V1 session");
        assert_eq!(state[8], 1, "projected V1 message");
        let observations = fixture
            .runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap()
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap();
        assert_eq!(observations.len(), 1);
        assert!(
            !observations[0]
                .observation()
                .payload()
                .to_string()
                .contains("legacy-api-secret")
        );
    }

    #[tokio::test]
    async fn bounded_source_frontier_charges_actual_bytes_within_global_budget() {
        let fixture = Fixture::new("frontier-session-0").await;
        let payload = "x".repeat(600 * 1024);
        let mut expected_source_bytes = 0_u64;
        for index in 0..3 {
            let session_id = format!("frontier-session-{index}");
            let transcript = fixture
                .home
                .join(".claude/projects/project-scope")
                .join(format!("{session_id}.jsonl"));
            let record = json!({
                "type": "user",
                "sessionId": session_id,
                "uuid": format!("frontier-message-{index}"),
                "timestamp": "2026-07-15T00:00:00Z",
                "cwd": fixture.temp.path(),
                "message": {"role": "user", "content": format!("frontier {index} {payload}")}
            });
            let record = format!("{record}\n");
            expected_source_bytes =
                expected_source_bytes.saturating_add(u64::try_from(record.len()).unwrap());
            fs::write(transcript, record).unwrap();
        }
        assert!(expected_source_bytes <= CLAUDE_HOOK_MAX_NEW_BYTES);
        let source = ClaudeSource::with_home(&fixture.home).for_user_scope(None, Vec::new());

        let first = fixture
            .ingest(
                &source,
                Some(CLAUDE_HOOK_MAX_NEW_BYTES),
                ObservationCancellation::default(),
            )
            .await
            .unwrap();
        assert_eq!(first.observations_committed, 3);
        assert_eq!(first.source_bytes_scanned, expected_source_bytes);
        assert_eq!(first.deferred_sources, 0);

        let observations = fixture
            .runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap()
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap();
        assert_eq!(observations.len(), 3);
    }

    #[tokio::test]
    async fn deferred_sources_charge_work_without_pinning_the_round_robin_frontier() {
        let fixture = Fixture::new("partial-budget-session-0").await;
        let partial_bytes = usize::try_from(CLAUDE_HOOK_MAX_NEW_BYTES / 2).unwrap();
        for index in 0..2 {
            let transcript = fixture
                .home
                .join(".claude/projects/project-scope")
                .join(format!("partial-budget-session-{index}.jsonl"));
            fs::write(transcript, vec![b'x'; partial_bytes]).unwrap();
        }
        let ready = fixture
            .home
            .join(".claude/projects/project-scope")
            .join("partial-budget-session-2.jsonl");
        fs::write(
            ready,
            format!(
                "{}\n",
                json!({
                    "type": "user",
                    "sessionId": "partial-budget-session-2",
                    "uuid": "partial-budget-ready-message",
                    "timestamp": "2026-07-15T00:00:00Z",
                    "cwd": fixture.temp.path(),
                    "message": {"role": "user", "content": "ready after partial sources"}
                })
            ),
        )
        .unwrap();
        let source = ClaudeSource::with_home(&fixture.home).for_user_scope(None, Vec::new());

        let stats = fixture
            .ingest(
                &source,
                Some(CLAUDE_HOOK_MAX_NEW_BYTES),
                ObservationCancellation::default(),
            )
            .await
            .unwrap();

        assert_eq!(stats.observations_committed, 0);
        assert_eq!(stats.source_bytes_scanned, CLAUDE_HOOK_MAX_NEW_BYTES);
        assert_eq!(stats.deferred_sources, 3);

        let recovered = fixture
            .ingest(&source, Some(1), ObservationCancellation::default())
            .await
            .unwrap();
        assert_eq!(recovered.observations_committed, 1);
        assert_eq!(recovered.transcript.messages_upserted, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bad_source_is_isolated_and_committed_projection_work_still_drains() {
        let fixture = Fixture::new("queued-before-bad-source").await;
        fixture.write_record("queued before bad source", "queued-secret");
        let seed_source = fixture.source("queued-before-bad-source");
        let admission = fixture.runtime.facade();
        let scope = ObservationScopeV1::Profile;
        let cancellation = ObservationCancellation::default();
        let processing_context = SourceProcessingContext {
            admission: &admission,
            source_adapter: &seed_source,
            project_root: &fixture.profile,
            scope: &scope,
            cancellation: &cancellation,
        };
        let seeded = process_source(&processing_context, &fixture.transcript, None)
            .await
            .unwrap();
        assert_eq!(seeded.observations_committed, 1);
        assert_eq!(ingest_state_counts(&fixture).await[4], 1);

        let transcripts = fixture.transcript.parent().unwrap();
        fs::write(transcripts.join("!bad\nsource.jsonl"), b"{}\n").unwrap();
        fs::write(
            transcripts.join("zz-valid-after-bad.jsonl"),
            format!(
                "{}\n",
                json!({
                    "type": "user",
                    "sessionId": "zz-valid-after-bad",
                    "uuid": "valid-after-bad-message",
                    "timestamp": "2026-07-15T00:00:00Z",
                    "cwd": fixture.temp.path(),
                    "message": {"role": "user", "content": "valid after bad source"}
                })
            ),
        )
        .unwrap();
        let source = ClaudeSource::with_home(&fixture.home).for_user_scope(None, Vec::new());

        let error = fixture
            .ingest(&source, None, ObservationCancellation::default())
            .await
            .expect_err("the isolated source failure remains visible");
        assert!(matches!(
            error,
            ClaudeObservationIngestError::SourceFailures {
                failed_sources: 1,
                first_reason_code: "observation_domain_invalid",
                first_retryable: false,
            }
        ));
        let state = ingest_state_counts(&fixture).await;
        assert_eq!(
            state[4], 0,
            "projection queue must drain despite source error"
        );
        assert_eq!(
            state[8], 2,
            "later valid source and queued seed must project"
        );
    }

    #[tokio::test]
    async fn recovery_advances_large_backlog_in_bounded_batches() {
        const FRAME_BYTES: usize = 128 * 1024;
        const FRAMES: u64 = 20;

        let fixture = Fixture::new("bounded-recovery-session").await;
        let frame = " ".repeat(FRAME_BYTES);
        let mut transcript = Vec::new();
        for _ in 0..FRAMES {
            transcript.extend_from_slice(frame.as_bytes());
            transcript.push(b'\n');
        }
        assert!(transcript.len() as u64 > CLAUDE_HOOK_MAX_NEW_BYTES);
        let transcript_len = transcript.len() as u64;
        fs::write(&fixture.transcript, transcript).unwrap();

        let source_adapter = fixture.source("bounded-recovery-session");
        let source = observation_source(&fixture.transcript);
        let store = fixture
            .runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();

        let first = fixture
            .ingest(&source_adapter, None, ObservationCancellation::default())
            .await
            .unwrap();
        assert_eq!(first.observations_committed, 0);
        assert!(first.cursor_advances > 0);
        assert!(first.cursor_advances < FRAMES);
        assert_eq!(first.transcript, TranscriptIngestStats::default());
        assert_eq!(first.deferred_sources, 1);
        let first_cursor = store
            .get_source_cursor(&source, &ObservationScopeV1::Profile)
            .await
            .unwrap()
            .unwrap();
        assert!(first_cursor.byte_offset() > 0);
        assert!(first_cursor.byte_offset() < transcript_len);

        let second = fixture
            .ingest(&source_adapter, None, ObservationCancellation::default())
            .await
            .unwrap();
        assert_eq!(second.observations_committed, 0);
        assert!(second.cursor_advances > 0);
        assert_eq!(second.transcript, TranscriptIngestStats::default());
        assert_eq!(second.deferred_sources, 0);
        let final_cursor = store
            .get_source_cursor(&source, &ObservationScopeV1::Profile)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(final_cursor.byte_offset(), transcript_len);
    }

    #[tokio::test]
    async fn commit_before_ack_retry_projects_without_rescan_or_duplicate() {
        let fixture = Fixture::new("retry-session").await;
        fixture.write_record("retry backfill searchable", "retry-secret");
        let source_adapter = fixture.source("retry-session");
        let identity = identify_claude_source(&fixture.transcript).unwrap();
        let mut scan = scan_claude_source_frames(identity.clone(), StoredCursor::default(), None)
            .expect("scan complete retry frame");
        source_adapter
            .retain_scoped_frames(&mut scan, &fixture.profile)
            .expect("retain profile-scoped retry frame");
        let source = ClaudeSourceIdentityV1::for_source(
            SessionId::new(identity.session_id).unwrap(),
            SessionId::new(identity.source_id).unwrap(),
        )
        .unwrap();
        let store = fixture
            .runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let application = ObservationApplication::new(
            store,
            ClaudeRecordSanitizerV1::claude_v1().expect("Claude V1 sanitizer"),
        );
        let admission = fixture.runtime.facade();
        let capture = capture_frame(
            &admission,
            scan.frames.first_mut().expect("retry frame"),
            None,
            &FrameCaptureContext {
                source,
                scope: ObservationScopeV1::Profile,
                generation: ClaudeFileGenerationV1::new(scan.file_generation).unwrap(),
                file_identity: scan.file_identity,
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
        assert_eq!(stats.observation_duplicates, 0);
        assert_eq!(stats.source_bytes_scanned, 0);
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
    async fn protected_source_identity_reuses_cursor_after_restart() {
        let raw_session_id = ["AKIA", "SYNTHETIC", "CANARY", "2"].concat();
        let fixture = Fixture::new(&raw_session_id).await;
        fixture.write_record("protected cursor restart", "restart-secret");
        let source_adapter = fixture.source(&raw_session_id);
        let identity = identify_claude_source(&fixture.transcript).unwrap();
        assert!(identity.session_id.starts_with("privacy.structural-id.v1."));
        assert!(!identity.session_id.contains(&raw_session_id));

        let first = fixture
            .ingest(&source_adapter, None, ObservationCancellation::default())
            .await
            .unwrap();
        assert_eq!(first.observations_committed, 1);

        let Fixture {
            temp: _temp,
            home,
            profile,
            transcript,
            runtime,
        } = fixture;
        drop(runtime);
        let restarted_runtime = HostAdmissionTestRuntimeV1::profile(&profile).await.unwrap();
        let restarted_source =
            ClaudeSource::with_home(&home).for_user_scope(Some(raw_session_id.clone()), Vec::new());
        let admission = restarted_runtime.facade();
        let second = ingest_source_with_observations_with_admission(
            &restarted_source,
            &profile,
            ObservationScopeV1::Profile,
            &admission,
            None,
            ObservationCancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(second.observations_committed, 0);
        assert_eq!(second.source_bytes_scanned, 0);

        let source = observation_source(&transcript);
        let snapshot = restarted_runtime
            .read_snapshot(HostAdmissionScope::Profile)
            .await
            .unwrap();
        let mut rows = snapshot
            .query(
                "SELECT cursor_json FROM source_cursors
                 WHERE source_json = ?1 AND scope_json = ?2",
                (
                    serde_json::to_string(&source).unwrap(),
                    serde_json::to_string(&ObservationScopeV1::Profile).unwrap(),
                ),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let cursor: ClaudeSourceCursorV1 =
            serde_json::from_str(&row.get::<String>(0).unwrap()).unwrap();
        assert!(cursor.byte_offset() > 0);
        let durable = serde_json::to_string(cursor.source()).unwrap();
        assert!(!durable.contains(&raw_session_id));
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
        let observations = fixture
            .runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap()
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap();
        assert!(observations.is_empty());
    }

    #[tokio::test]
    async fn malformed_partial_and_oversized_frames_preserve_all_observation_state() {
        let oversized = format!(
            "{{\"type\":\"user\",\"payload\":\"{}\"}}\n",
            "x".repeat(tracedecay_runtime_core::privacy::MAX_OBSERVATION_RECORD_BYTES)
        );
        for (session_id, frame) in [
            (
                "invalid-malformed",
                br#"{"type":"user",malformed}
"#
                .as_slice(),
            ),
            ("invalid-partial", br#"{"type":"user""#.as_slice()),
            ("invalid-oversized", oversized.as_bytes()),
        ] {
            assert_invalid_frame_preserves_observation_state(session_id, frame).await;
        }
    }

    #[tokio::test]
    async fn valid_prefix_commits_once_before_invalid_suffix_without_cursor_drift() {
        let oversized = format!(
            "{{\"type\":\"user\",\"payload\":\"{}\"}}\n",
            "x".repeat(tracedecay_runtime_core::privacy::MAX_OBSERVATION_RECORD_BYTES)
        );
        for (session_id, suffix) in [
            (
                "prefix-malformed",
                br#"{"type":"user",malformed}
"#
                .as_slice(),
            ),
            ("prefix-partial", br#"{"type":"user""#.as_slice()),
            ("prefix-oversized", oversized.as_bytes()),
        ] {
            assert_invalid_suffix_preserves_valid_prefix(session_id, suffix).await;
        }
    }
}
