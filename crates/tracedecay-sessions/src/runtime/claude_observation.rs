//! Observation-first Claude transcript ingestion.
//!
//! The provider owns framing and scope. This coordinator routes framed records
//! through the host admission authority, then drains projection work in source
//! order. The projector is the only writer of V1 session and message rows.

use std::collections::BTreeSet;
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

use crate::admission::{HostAdmission, is_admission_cancellation};
use crate::observation::{
    CaptureClaudeObservationOutcome, CaptureClaudeObservationRequest,
    CaptureClaudeObservationRequestError, ObservationApplicationError, ObservationCancellation,
};
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
use tracedecay_runtime_core::privacy::PrivacySanitizerError;

pub const CLAUDE_TRANSCRIPT_RETENTION_CLASS: &str = "transcript.claude.v1";
/// Every pass, including startup recovery, bounds its raw and parsed backlog.
pub const CLAUDE_HOOK_MAX_NEW_BYTES: u64 = STRICT_JSONL_BATCH_BYTES;
const CLAUDE_RECOVERY_MAX_NEW_BYTES: u64 = STRICT_JSONL_BATCH_BYTES * 8;
const MAX_CLAUDE_SOURCES_PER_PASS: usize = 64;
const CLAUDE_SOURCE_FRONTIER_KEY: &str =
    "tracedecay-internal:claude-observation-source-frontier:v1";
const MAX_PROJECTIONS_PER_PASS: usize = 256;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
    projected_session_ids: BTreeSet<String>,
}

impl ClaudeObservationIngestStats {
    #[must_use]
    fn merge(mut self, other: Self) -> Self {
        self.transcript = self.transcript.merge(other.transcript);
        self.projected_session_ids
            .extend(other.projected_session_ids);
        self.transcript.sessions_upserted = self
            .projected_session_ids
            .iter()
            .fold(0_u64, |count, _| count.saturating_add(1));
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

    pub(crate) fn projected_session_ids(&self) -> &BTreeSet<String> {
        &self.projected_session_ids
    }

    pub(crate) fn deduplicated_transcript_stats(
        &self,
        projected_session_ids: &mut BTreeSet<String>,
    ) -> TranscriptIngestStats {
        let mut stats = self.transcript;
        stats.sessions_upserted = self
            .projected_session_ids
            .iter()
            .filter(|session_id| projected_session_ids.insert((*session_id).clone()))
            .fold(0_u64, |count, _| count.saturating_add(1));
        stats
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
    #[error("Claude observation ingestion terminated after durable progress")]
    Terminated {
        stats: Box<ClaudeObservationIngestStats>,
        #[source]
        error: Box<Self>,
    },
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

impl ClaudeObservationIngestError {
    pub(crate) fn accumulated_stats(&self) -> Option<&ClaudeObservationIngestStats> {
        match self {
            Self::Terminated { stats, .. } => Some(stats),
            _ => None,
        }
    }

    pub(crate) fn is_typed_cancellation(&self) -> bool {
        match self {
            Self::Application(ObservationApplicationError::Cancelled) => true,
            Self::Transcript(error) => error.is_cancelled(),
            Self::Terminated { error, .. } => error.is_typed_cancellation(),
            _ => false,
        }
    }
}

fn terminal_error_after_progress(
    stats: ClaudeObservationIngestStats,
    error: ClaudeObservationIngestError,
) -> ClaudeObservationIngestError {
    let has_durable_progress = stats.observations_committed > 0
        || stats.observation_duplicates > 0
        || stats.cursor_advances > 0
        || stats.cursor_duplicates > 0
        || stats.records_rejected > 0
        || stats.records_quarantined > 0
        || stats.projections_completed > 0
        || stats.projection_outputs > 0
        || stats.projections_skipped > 0
        || stats.projection_duplicates > 0;
    if has_durable_progress {
        ClaudeObservationIngestError::Terminated {
            stats: Box::new(stats),
            error: Box::new(error),
        }
    } else {
        error
    }
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
        }
        | CaptureClaudeObservationOutcome::AcceptedForReplay {
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

struct SourceProcessingContext<'a, A: ?Sized> {
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
    let observation_cursor = context
        .admission
        .get_source_cursor(&source, context.scope)
        .await
        .map_err(|outcome| host_admission_error("claude", outcome))?;
    let previous = scanner_cursor(observation_cursor.as_ref());
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
            return Err(terminal_error_after_progress(
                stats,
                ObservationApplicationError::Cancelled.into(),
            ));
        }
        let applied = match apply_scanned_segment(
            admission,
            &capture_context,
            &mut observation_cursor,
            segment,
            &mut stats,
        )
        .await
        {
            Ok(applied) => applied,
            Err(error) => return Err(terminal_error_after_progress(stats, error)),
        };
        if !applied {
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
    if cancellation.is_cancelled() {
        return Err(TranscriptIngestError::Cancelled { provider: "claude" }.into());
    }
    let outcome = admission
        .drain_projection_queue("claude", scope, cancellation, MAX_PROJECTIONS_PER_PASS)
        .await
        .map_err(|outcome| {
            if is_admission_cancellation(&outcome, cancellation) {
                TranscriptIngestError::Cancelled { provider: "claude" }
            } else {
                host_admission_error("claude", outcome)
            }
        })?;
    let projected_session_ids = outcome.session_ids.into_iter().collect::<BTreeSet<_>>();
    let projected_sessions = projected_session_ids
        .iter()
        .fold(0_u64, |count, _| count.saturating_add(1));
    Ok(ClaudeObservationIngestStats {
        transcript: TranscriptIngestStats {
            sessions_upserted: projected_sessions,
            messages_upserted: outcome.projected,
        },
        projections_completed: outcome.projected,
        projection_outputs: outcome.projected_outputs,
        projections_skipped: outcome.skipped,
        projection_duplicates: outcome.exact_duplicates,
        deferred_sources: u64::from(outcome.deferred),
        projected_session_ids,
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
                if let Some(progress) = error.accumulated_stats() {
                    stats = stats.merge(progress.clone());
                }
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
    if (deferred > 0 || attempted_sources < scheduled_source_count)
        && let Err(error) = advance_source_frontier(admission, &scope, attempted_sources).await
    {
        return Err(terminal_error_after_progress(stats, error));
    }
    let projection_stats = match drain_projection_queue(admission, &scope, &cancellation).await {
        Ok(stats) => stats,
        Err(error) => {
            if error.is_typed_cancellation()
                && let Some((failed_sources, first_reason_code, first_retryable)) = source_failures
            {
                return Err(terminal_error_after_progress(
                    stats,
                    ClaudeObservationIngestError::SourceFailures {
                        failed_sources,
                        first_reason_code,
                        first_retryable,
                    },
                ));
            }
            return Err(terminal_error_after_progress(stats, error));
        }
    };
    if let Some((failed_sources, first_reason_code, first_retryable)) = source_failures {
        return Err(terminal_error_after_progress(
            stats.merge(projection_stats),
            ClaudeObservationIngestError::SourceFailures {
                failed_sources,
                first_reason_code,
                first_retryable,
            },
        ));
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
#[path = "claude_observation/tests.rs"]
mod tests;
