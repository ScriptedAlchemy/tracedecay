use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

use tracedecay_domain::{
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    RetentionClass, SanitizationReceiptV1,
};
use tracedecay_store::observation::{ObservationCoverageReason, ObservationCursorAdvance};

use crate::admission::{HostAdmission, HostAdmissionOutcome, is_admission_cancellation};
use crate::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::runtime::SessionMessageRecord;
use crate::runtime::shared::StoredCursor;
use crate::runtime::snapshot_observation::host_admission_error;
use crate::runtime::source::{
    JsonlResumeState, MAX_JSONL_RECORD_BYTES, ParsedTranscript, RawJsonlSkippedReason,
    TranscriptIngestError, TranscriptIngestResult, preflight_strict_jsonl,
    try_stream_new_jsonl_raw_strict_with_resume,
};
use tracedecay_runtime_core::privacy::ParsedObservationRecordV1;

#[derive(Clone, Copy)]
pub(super) enum PersistedCursorUpdate {
    Replace,
    Monotonic,
}

/// How one flush persists: the retention class the frames are captured under
/// and whether the durable cursor may move backwards. Both are decided once per
/// admission and always travel together, so they pass as one value.
#[derive(Clone, Copy)]
struct FlushPolicy<'policy> {
    retention_class: &'policy RetentionClass,
    persisted_cursor_update: PersistedCursorUpdate,
}

pub(super) struct JsonlObservationAdmissionRequest<'request> {
    provider: &'static str,
    path: &'request Path,
    admission: &'request dyn HostAdmission,
    source: ObservationSourceIdentityV1,
    scope: ObservationScopeV1,
    retention_class: RetentionClass,
    max_new_bytes: Option<u64>,
    persisted_cursor_update: PersistedCursorUpdate,
    cancellation: ObservationCancellation,
}

impl<'request> JsonlObservationAdmissionRequest<'request> {
    pub(super) fn new(
        provider: &'static str,
        path: &'request Path,
        admission: &'request dyn HostAdmission,
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        retention_class: RetentionClass,
    ) -> Self {
        Self {
            provider,
            path,
            admission,
            source,
            scope,
            retention_class,
            max_new_bytes: None,
            persisted_cursor_update: PersistedCursorUpdate::Monotonic,
            cancellation: ObservationCancellation::default(),
        }
    }

    pub(super) fn with_max_new_bytes(mut self, max_new_bytes: Option<u64>) -> Self {
        self.max_new_bytes = max_new_bytes;
        self
    }

    pub(super) fn with_persisted_cursor_update(
        mut self,
        persisted_cursor_update: PersistedCursorUpdate,
    ) -> Self {
        self.persisted_cursor_update = persisted_cursor_update;
        self
    }

    pub(super) fn with_cancellation(mut self, cancellation: ObservationCancellation) -> Self {
        self.cancellation = cancellation;
        self
    }
}

pub(super) enum JsonlFrameAdmission {
    Durable {
        parsed_record: ParsedObservationRecordV1,
        native_record_id: ObservationId,
    },
    NonDurable {
        reason: ObservationCoverageReason,
        /// The verdict was reached from the frame's raw bytes, without
        /// decoding it. Kept because the cost of a skipped frame is not the
        /// skip: it is whether the frame was parsed first.
        before_decode: bool,
    },
}

impl JsonlFrameAdmission {
    pub(super) fn durable(
        parsed_record: ParsedObservationRecordV1,
        native_record_id: ObservationId,
    ) -> Self {
        Self::Durable {
            parsed_record,
            native_record_id,
        }
    }

    pub(super) fn non_durable(reason: ObservationCoverageReason) -> Self {
        Self::NonDurable {
            reason,
            before_decode: false,
        }
    }

    /// A frame refused from its raw bytes alone.
    ///
    /// A normalizer may only answer this when it can prove the decoded frame
    /// would have carried the same reason, because the coverage row this
    /// writes is indistinguishable from the one the decoded path writes.
    pub(super) fn non_durable_before_decode(reason: ObservationCoverageReason) -> Self {
        Self::NonDurable {
            reason,
            before_decode: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct JsonlObservationAdmissionProgress {
    pub bytes_consumed: u64,
    pub source_deferred: bool,
    pub frames_decoded: u64,
    pub frames_accepted: u64,
    pub frames_skipped: u64,
    /// Of `frames_skipped`, how many were refused from their raw bytes before
    /// being decoded. The aggregate cannot separate a skip that cost a decode
    /// and two structural walks from one that cost a tokenize.
    pub frames_rejected_before_decode: u64,
    pub frames_refused: u64,
    pub frames_persisted: u64,
    pub io: crate::runtime::source::JsonlIoAccounting,
}

#[derive(Clone, Copy)]
pub(super) struct JsonlObservationScan {
    pub resumed: bool,
    /// True when a prior cursor existed but the scan restarted at offset 0
    /// (truncate/rename replacement). Callers use this to keep projected
    /// message ids distinct across file generations.
    pub replacement_rescan: bool,
    pub start_offset: u64,
    pub generation: u64,
}

#[derive(Clone, Copy)]
struct JsonlCheckpoint {
    offset: u64,
    end_offset: u64,
    resume_fingerprint: u64,
}

impl JsonlCheckpoint {
    const fn new(offset: u64, end_offset: u64, resume_fingerprint: u64) -> Self {
        Self {
            offset,
            end_offset,
            resume_fingerprint,
        }
    }
}

/// Bounded persist window: flush consecutive durables before this many
/// frames so one `persist_observations` call stays a scan-sized batch.
const MAX_CAPTURE_WINDOW: usize = 256;

struct DurableJsonlFrame {
    checkpoint: JsonlCheckpoint,
    range: tracedecay_domain::ObservationSourceRangeV1,
    parsed_record: ParsedObservationRecordV1,
    native_record_id: ObservationId,
    bytes: Arc<[u8]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DurableFrameDisposition {
    Persisted,
    Refused,
}

struct ActiveAdmission<'request> {
    provider: &'static str,
    admission: &'request dyn HostAdmission,
    source: ObservationSourceIdentityV1,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    file_identity: u64,
    cancellation: ObservationCancellation,
}

impl ActiveAdmission<'_> {
    fn cursor_at(
        &self,
        end_offset: u64,
        resume_fingerprint: u64,
    ) -> TranscriptIngestResult<ObservationSourceCursorV1> {
        Ok(ObservationSourceCursorV1::for_ordering(
            self.source.clone(),
            self.scope.clone(),
            self.generation,
            ObservationOrderingDomainV1::FileBytes,
            end_offset,
        )?
        .with_resume_checkpoint(self.file_identity, resume_fingerprint))
    }

    async fn advance_coverage(
        &self,
        expected_cursor: &mut Option<ObservationSourceCursorV1>,
        checkpoint: JsonlCheckpoint,
        reason: ObservationCoverageReason,
        receipt: Option<SanitizationReceiptV1>,
    ) -> TranscriptIngestResult<()> {
        let range = tracedecay_domain::ObservationSourceRangeV1::new(
            checkpoint.offset,
            checkpoint.end_offset,
        )?;
        let advance = match receipt {
            Some(receipt) => ObservationCursorAdvance::for_ordering_with_sanitization_receipt(
                self.source.clone(),
                self.scope.clone(),
                self.generation,
                ObservationOrderingDomainV1::FileBytes,
                expected_cursor.clone(),
                range,
                reason,
                receipt,
            ),
            None => ObservationCursorAdvance::for_ordering(
                self.source.clone(),
                self.scope.clone(),
                self.generation,
                ObservationOrderingDomainV1::FileBytes,
                expected_cursor.clone(),
                range,
                reason,
            ),
        }
        .map_err(|_| TranscriptIngestError::InvalidFrameState {
            provider: self.provider,
        })?
        .with_resume_checkpoint(self.file_identity, checkpoint.resume_fingerprint);
        self.admission
            .advance_non_durable_source_cursor(advance, self.cancellation.clone())
            .await
            .map_err(|outcome| {
                if is_admission_cancellation(&outcome, &self.cancellation) {
                    TranscriptIngestError::Cancelled {
                        provider: self.provider,
                    }
                } else if outcome.retryable {
                    // A retryable advance failure — a cursor CAS lost to a
                    // peer ingestor, a still-mounting write authority — says
                    // nothing about the record; wrapping it as NonDurable
                    // laundered the admission's own verdict into a terminal
                    // non-retryable disposition.
                    host_admission_error(self.provider, outcome)
                } else {
                    TranscriptIngestError::NonDurableRecord {
                        provider: self.provider,
                        offset: checkpoint.offset,
                        end_offset: checkpoint.end_offset,
                        reason: outcome
                            .reason_code
                            .unwrap_or("non_durable_cursor_advance_failed"),
                    }
                }
            })?;
        *expected_cursor =
            Some(self.cursor_at(checkpoint.end_offset, checkpoint.resume_fingerprint)?);
        Ok(())
    }

    fn capture_request(
        &self,
        expected_cursor: Option<ObservationSourceCursorV1>,
        frame: DurableJsonlFrame,
        retention_class: &RetentionClass,
    ) -> TranscriptIngestResult<CaptureObservationRequest> {
        let identity = ObservationIdentityMaterialV1::for_native_record(
            self.source.clone(),
            self.scope.clone(),
            self.generation,
            frame.range,
            ObservationOrderingDomainV1::FileBytes,
            frame.native_record_id,
        )?;
        CaptureObservationRequest::new(
            frame.parsed_record,
            identity,
            expected_cursor,
            retention_class.clone(),
            self.cancellation.clone(),
        )
        .map_err(|_| TranscriptIngestError::InvalidFrameState {
            provider: self.provider,
        })
        .map(|request| {
            request.with_resume_checkpoint(self.file_identity, frame.checkpoint.resume_fingerprint)
        })
    }

    async fn apply_capture_result(
        &self,
        expected_cursor: &mut Option<ObservationSourceCursorV1>,
        checkpoint: JsonlCheckpoint,
        result: Result<CaptureObservationOutcome, HostAdmissionOutcome>,
        persisted_cursor_update: PersistedCursorUpdate,
    ) -> TranscriptIngestResult<DurableFrameDisposition> {
        match result {
            Ok(CaptureObservationOutcome::Persisted { .. })
            | Ok(CaptureObservationOutcome::AcceptedForReplay { .. }) => {
                let should_update = match persisted_cursor_update {
                    PersistedCursorUpdate::Replace => true,
                    PersistedCursorUpdate::Monotonic => {
                        expected_cursor.as_ref().is_none_or(|cursor| {
                            cursor.generation() != self.generation
                                || cursor.position() < checkpoint.end_offset
                        })
                    }
                };
                if should_update {
                    *expected_cursor =
                        Some(self.cursor_at(checkpoint.end_offset, checkpoint.resume_fingerprint)?);
                }
                Ok(DurableFrameDisposition::Persisted)
            }
            Ok(CaptureObservationOutcome::Rejected { receipt, .. }) => {
                self.advance_coverage(
                    expected_cursor,
                    checkpoint,
                    ObservationCoverageReason::SanitizerRejected,
                    Some(receipt),
                )
                .await?;
                Ok(DurableFrameDisposition::Refused)
            }
            Ok(CaptureObservationOutcome::Quarantined { receipt, .. }) => {
                self.advance_coverage(
                    expected_cursor,
                    checkpoint,
                    ObservationCoverageReason::SanitizerQuarantined,
                    Some(receipt),
                )
                .await?;
                Ok(DurableFrameDisposition::Refused)
            }
            // Deterministic content refusals re-fail identically forever;
            // advance coverage with a durable typed reason so the stream
            // converges instead of re-reporting the same records every sweep.
            Err(outcome)
                if is_deterministic_content_refusal(&outcome)
                    && !self.cancellation.is_cancelled() =>
            {
                tracing::warn!(
                    provider = self.provider,
                    offset = checkpoint.offset,
                    reason = outcome.reason_code.unwrap_or("host_admission_refused"),
                    "admission refused a record; covering past it"
                );
                self.advance_coverage(
                    expected_cursor,
                    checkpoint,
                    ObservationCoverageReason::AdmissionRefused,
                    None,
                )
                .await?;
                Ok(DurableFrameDisposition::Refused)
            }
            Err(outcome) => {
                if is_admission_cancellation(&outcome, &self.cancellation) {
                    Err(TranscriptIngestError::Cancelled {
                        provider: self.provider,
                    })
                } else {
                    // Everything else says nothing about the record's
                    // content: commit/read-back failures
                    // (`observation_commit_failed`,
                    // `authority_write_failed`,
                    // `observation_persisted_value_unavailable`), unbound
                    // authorities, and retryable races keep the admission
                    // authority's own verdict as a typed block. The frontier
                    // must not advance over a record whose durable fate is
                    // unknown — the persist may already have committed and
                    // advanced the source cursor, so a cover-past write here
                    // would stack a second, conflicting cursor advance on
                    // every frame.
                    Err(host_admission_error(self.provider, outcome))
                }
            }
        }
    }

    async fn capture(
        &self,
        expected_cursor: &mut Option<ObservationSourceCursorV1>,
        frame: DurableJsonlFrame,
        retention_class: &RetentionClass,
        persisted_cursor_update: PersistedCursorUpdate,
    ) -> TranscriptIngestResult<DurableFrameDisposition> {
        let checkpoint = frame.checkpoint;
        crate::runtime::pipeline_metrics::record_capture_single();
        let request = self.capture_request(expected_cursor.clone(), frame, retention_class)?;
        let result = self.admission.capture_observation(request).await;
        self.apply_capture_result(expected_cursor, checkpoint, result, persisted_cursor_update)
            .await
    }

    async fn capture_window(
        &self,
        expected_cursor: &mut Option<ObservationSourceCursorV1>,
        frames: Vec<DurableJsonlFrame>,
        retention_class: &RetentionClass,
        persisted_cursor_update: PersistedCursorUpdate,
        progress: &mut JsonlObservationAdmissionProgress,
    ) -> TranscriptIngestResult<()> {
        if frames.is_empty() {
            return Ok(());
        }
        crate::runtime::pipeline_metrics::record_capture_window(frames.len());
        let mut batch_expected = expected_cursor.clone();
        let mut requests = Vec::with_capacity(frames.len());
        let mut checkpoints = Vec::with_capacity(frames.len());
        for frame in frames {
            checkpoints.push(frame.checkpoint);
            let next_expected = Some(self.cursor_at(
                frame.checkpoint.end_offset,
                frame.checkpoint.resume_fingerprint,
            )?);
            requests.push(self.capture_request(batch_expected, frame, retention_class)?);
            batch_expected = next_expected;
        }
        match self.admission.capture_observations(requests).await {
            Ok(outcomes) => {
                if outcomes.len() != checkpoints.len() {
                    return Err(TranscriptIngestError::InvalidFrameState {
                        provider: self.provider,
                    });
                }
                for (checkpoint, outcome) in checkpoints.into_iter().zip(outcomes) {
                    match self
                        .apply_capture_result(
                            expected_cursor,
                            checkpoint,
                            Ok(outcome),
                            persisted_cursor_update,
                        )
                        .await?
                    {
                        DurableFrameDisposition::Persisted => {
                            progress.frames_accepted = progress.frames_accepted.saturating_add(1);
                            progress.frames_persisted = progress.frames_persisted.saturating_add(1);
                        }
                        DurableFrameDisposition::Refused => {
                            progress.frames_refused = progress.frames_refused.saturating_add(1);
                        }
                    }
                }
                Ok(())
            }
            Err(outcome) => {
                if is_admission_cancellation(&outcome, &self.cancellation) {
                    Err(TranscriptIngestError::Cancelled {
                        provider: self.provider,
                    })
                } else {
                    Err(host_admission_error(self.provider, outcome))
                }
            }
        }
    }
}

#[hotpath::measure]
pub(super) async fn admit_jsonl_observations<State>(
    request: JsonlObservationAdmissionRequest<'_>,
    initialize: impl FnOnce(JsonlObservationScan) -> State,
    mut normalize: impl FnMut(
        &mut State,
        &[u8],
        tracedecay_domain::ObservationSourceRangeV1,
        u64,
    ) -> TranscriptIngestResult<JsonlFrameAdmission>,
) -> TranscriptIngestResult<JsonlObservationAdmissionProgress> {
    let JsonlObservationAdmissionRequest {
        provider,
        path,
        admission,
        source,
        scope,
        retention_class,
        max_new_bytes,
        persisted_cursor_update,
        cancellation,
    } = request;
    if cancellation.is_cancelled() {
        return Err(TranscriptIngestError::Cancelled { provider });
    }
    let mut expected_cursor =
        admission
            .get_source_cursor(&source, &scope)
            .await
            .map_err(|outcome| {
                if is_admission_cancellation(&outcome, &cancellation) {
                    TranscriptIngestError::Cancelled { provider }
                } else {
                    TranscriptIngestError::InvalidFrameState { provider }
                }
            })?;
    if cancellation.is_cancelled() {
        return Err(TranscriptIngestError::Cancelled { provider });
    }
    let previous = expected_cursor
        .as_ref()
        .map_or(StoredCursor::default(), |cursor| StoredCursor {
            position: cursor.position(),
            mtime: 0,
            file_id: cursor.generation().generation_id(),
        });
    let resume_state = expected_cursor.as_ref().and_then(|cursor| {
        Some(JsonlResumeState {
            generation: cursor.generation().generation_id(),
            file_identity: cursor.file_identity()?,
            fingerprint: cursor.resume_fingerprint()?,
        })
    });
    let had_expected_cursor = expected_cursor.is_some();
    let raw = try_stream_new_jsonl_raw_strict_with_resume(
        path,
        previous,
        max_new_bytes,
        MAX_JSONL_RECORD_BYTES,
        resume_state,
    )?;
    let mut progress = JsonlObservationAdmissionProgress {
        bytes_consumed: raw.read_through.saturating_sub(raw.start_offset),
        source_deferred: raw.deferred.is_some(),
        frames_decoded: u64::try_from(raw.frames.len()).unwrap_or(u64::MAX),
        io: raw.io,
        ..JsonlObservationAdmissionProgress::default()
    };
    let total_frames = raw.frames.len();
    let retained_frame_bytes = raw.frames.iter().fold(0_u64, |total, frame| {
        total.saturating_add(u64::try_from(frame.bytes.len()).unwrap_or(u64::MAX))
    });
    tracing::debug!(
        event = "transcript_admission_batch",
        phase = "capturing",
        provider,
        transcript = %transcript_log_identity(path),
        total_frames,
        retained_frame_bytes,
        bytes_consumed = progress.bytes_consumed,
        source_deferred = progress.source_deferred,
        "transcript admission batch started"
    );
    if cancellation.is_cancelled() {
        return Err(TranscriptIngestError::Cancelled { provider });
    }
    let generation = ObservationSourceGenerationV1::new(raw.new_cursor.file_id)?;
    let mut state = initialize(JsonlObservationScan {
        resumed: had_expected_cursor && raw.start_offset > 0,
        // Derived from the scanned generation rather than this batch's first
        // offset, so a rewrite that spans several batches keeps namespacing
        // ids past the batch that started at the file head.
        replacement_rescan: raw.replacement_generation,
        start_offset: raw.start_offset,
        generation: raw.new_cursor.file_id,
    });
    let active = ActiveAdmission {
        provider,
        admission,
        source,
        scope,
        generation,
        file_identity: raw.file_identity,
        cancellation,
    };
    let mut skipped = raw.skipped.into_iter().peekable();
    let mut pending: Vec<DurableJsonlFrame> = Vec::new();

    async fn flush_pending<State>(
        active: &ActiveAdmission<'_>,
        expected_cursor: &mut Option<ObservationSourceCursorV1>,
        pending: &mut Vec<DurableJsonlFrame>,
        policy: FlushPolicy<'_>,
        progress: &mut JsonlObservationAdmissionProgress,
        state: &mut State,
        mut normalize: impl FnMut(
            &mut State,
            &[u8],
            tracedecay_domain::ObservationSourceRangeV1,
            u64,
        ) -> TranscriptIngestResult<JsonlFrameAdmission>,
    ) -> TranscriptIngestResult<()> {
        let frames = std::mem::take(pending);
        if frames.is_empty() {
            return Ok(());
        }
        let backups = frames
            .iter()
            .map(|frame| (frame.checkpoint, frame.range, Arc::clone(&frame.bytes)))
            .collect::<Vec<_>>();
        match active
            .capture_window(
                expected_cursor,
                frames,
                policy.retention_class,
                policy.persisted_cursor_update,
                progress,
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if is_content_refusal_ingest_error(&error) => {
                for (checkpoint, range, bytes) in backups {
                    if active.cancellation.is_cancelled() {
                        return Err(TranscriptIngestError::Cancelled {
                            provider: active.provider,
                        });
                    }
                    match normalize(state, bytes.as_ref(), range, checkpoint.offset)? {
                        JsonlFrameAdmission::Durable {
                            parsed_record,
                            native_record_id,
                        } => {
                            match active
                                .capture(
                                    expected_cursor,
                                    DurableJsonlFrame {
                                        checkpoint,
                                        range,
                                        parsed_record,
                                        native_record_id,
                                        bytes,
                                    },
                                    policy.retention_class,
                                    policy.persisted_cursor_update,
                                )
                                .await?
                            {
                                DurableFrameDisposition::Persisted => {
                                    progress.frames_accepted =
                                        progress.frames_accepted.saturating_add(1);
                                    progress.frames_persisted =
                                        progress.frames_persisted.saturating_add(1);
                                }
                                DurableFrameDisposition::Refused => {
                                    progress.frames_refused =
                                        progress.frames_refused.saturating_add(1);
                                }
                            }
                        }
                        JsonlFrameAdmission::NonDurable {
                            reason,
                            before_decode,
                        } => {
                            active
                                .advance_coverage(expected_cursor, checkpoint, reason, None)
                                .await?;
                            crate::runtime::pipeline_metrics::record_frame_skipped(reason);
                            progress.frames_skipped = progress.frames_skipped.saturating_add(1);
                            if before_decode {
                                progress.frames_rejected_before_decode =
                                    progress.frames_rejected_before_decode.saturating_add(1);
                            }
                        }
                    }
                }
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    for (frame_index, frame) in raw.frames.into_iter().enumerate() {
        if active.cancellation.is_cancelled() {
            return Err(TranscriptIngestError::Cancelled { provider });
        }
        if frame_index % 256 == 0 {
            tracing::trace!(
                event = "transcript_admission_batch",
                phase = "capturing",
                provider,
                transcript = %transcript_log_identity(path),
                completed_frames = frame_index,
                total_frames,
                source_offset = frame.offset,
                "transcript admission batch progress"
            );
        }
        while skipped
            .peek()
            .is_some_and(|skipped| skipped.offset < frame.offset)
        {
            if active.cancellation.is_cancelled() {
                return Err(TranscriptIngestError::Cancelled { provider });
            }
            flush_pending(
                &active,
                &mut expected_cursor,
                &mut pending,
                FlushPolicy {
                    retention_class: &retention_class,
                    persisted_cursor_update,
                },
                &mut progress,
                &mut state,
                &mut normalize,
            )
            .await?;
            let skipped = skipped
                .next()
                .ok_or(TranscriptIngestError::InvalidFrameState { provider })?;
            active
                .advance_coverage(
                    &mut expected_cursor,
                    JsonlCheckpoint::new(
                        skipped.offset,
                        skipped.end_offset,
                        skipped.resume_fingerprint,
                    ),
                    skipped_reason(skipped.reason),
                    None,
                )
                .await?;
            crate::runtime::pipeline_metrics::record_frame_skipped(skipped_reason(skipped.reason));
            progress.frames_skipped = progress.frames_skipped.saturating_add(1);
        }
        if active.cancellation.is_cancelled() {
            return Err(TranscriptIngestError::Cancelled { provider });
        }

        let range =
            tracedecay_domain::ObservationSourceRangeV1::new(frame.offset, frame.end_offset)?;
        let checkpoint =
            JsonlCheckpoint::new(frame.offset, frame.end_offset, frame.resume_fingerprint);
        let (parsed_record, native_record_id) =
            match normalize(&mut state, &frame.bytes, range, frame.offset)? {
                JsonlFrameAdmission::Durable {
                    parsed_record,
                    native_record_id,
                } => (parsed_record, native_record_id),
                JsonlFrameAdmission::NonDurable {
                    reason,
                    before_decode,
                } => {
                    flush_pending(
                        &active,
                        &mut expected_cursor,
                        &mut pending,
                        FlushPolicy {
                            retention_class: &retention_class,
                            persisted_cursor_update,
                        },
                        &mut progress,
                        &mut state,
                        &mut normalize,
                    )
                    .await?;
                    active
                        .advance_coverage(&mut expected_cursor, checkpoint, reason, None)
                        .await?;
                    crate::runtime::pipeline_metrics::record_frame_skipped(reason);
                    progress.frames_skipped = progress.frames_skipped.saturating_add(1);
                    if before_decode {
                        progress.frames_rejected_before_decode =
                            progress.frames_rejected_before_decode.saturating_add(1);
                    }
                    continue;
                }
            };
        pending.push(DurableJsonlFrame {
            checkpoint,
            range,
            parsed_record,
            native_record_id,
            bytes: frame.bytes.into(),
        });
        if pending.len() >= MAX_CAPTURE_WINDOW {
            flush_pending(
                &active,
                &mut expected_cursor,
                &mut pending,
                FlushPolicy {
                    retention_class: &retention_class,
                    persisted_cursor_update,
                },
                &mut progress,
                &mut state,
                &mut normalize,
            )
            .await?;
        }
    }

    flush_pending(
        &active,
        &mut expected_cursor,
        &mut pending,
        FlushPolicy {
            retention_class: &retention_class,
            persisted_cursor_update,
        },
        &mut progress,
        &mut state,
        &mut normalize,
    )
    .await?;

    if !active.cancellation.is_cancelled() {
        for skipped in skipped {
            active
                .advance_coverage(
                    &mut expected_cursor,
                    JsonlCheckpoint::new(
                        skipped.offset,
                        skipped.end_offset,
                        skipped.resume_fingerprint,
                    ),
                    skipped_reason(skipped.reason),
                    None,
                )
                .await?;
            crate::runtime::pipeline_metrics::record_frame_skipped(skipped_reason(skipped.reason));
            progress.frames_skipped = progress.frames_skipped.saturating_add(1);
        }
    } else {
        return Err(TranscriptIngestError::Cancelled { provider });
    }
    tracing::debug!(
        event = "transcript_admission_batch",
        phase = "complete",
        provider,
        transcript = %transcript_log_identity(path),
        total_frames,
        retained_frame_bytes,
        bytes_consumed = progress.bytes_consumed,
        source_deferred = progress.source_deferred,
        "transcript admission batch finished"
    );
    crate::runtime::pipeline_metrics::record_admission_progress(
        progress.frames_decoded,
        progress.frames_accepted,
        progress.frames_skipped,
        progress.frames_rejected_before_decode,
        progress.frames_refused,
        progress.frames_persisted,
    );
    Ok(progress)
}

/// Non-retryable admission failures that are verdicts about the record's
/// content. Only these may be covered past: they re-fail identically on every
/// sweep, so a durable `AdmissionRefused` coverage row is what lets the
/// stream converge. Every other failure — store commit/read-back failures,
/// unbound authorities, retryable races — says nothing about the record and
/// must surface as a typed block instead of writing coverage over a commit
/// that never landed (or one that already landed and advanced the cursor).
fn is_deterministic_content_refusal(outcome: &HostAdmissionOutcome) -> bool {
    !outcome.retryable
        && matches!(
            outcome.reason_code,
            Some("invalid_observation_contract" | "privacy_boundary_failed")
        )
}

fn is_content_refusal_ingest_error(error: &TranscriptIngestError) -> bool {
    matches!(
        error,
        TranscriptIngestError::HostAdmission {
            reason: "invalid_observation_contract" | "privacy_boundary_failed",
            retryable: false,
            ..
        }
    )
}

/// Log identity for a transcript file. Transcript paths sit under the
/// operator's home directory and name real sessions, so ingest logs carry the
/// basename only rather than persisting an absolute path into the daemon log.
fn transcript_log_identity(path: &Path) -> Cow<'_, str> {
    path.file_name()
        .map_or(Cow::Borrowed("<unnamed>"), |name| name.to_string_lossy())
}

fn skipped_reason(reason: RawJsonlSkippedReason) -> ObservationCoverageReason {
    match reason {
        RawJsonlSkippedReason::Whitespace => ObservationCoverageReason::BlankFrame,
        RawJsonlSkippedReason::Oversized => ObservationCoverageReason::OversizedFrame,
    }
}

pub(super) fn namespace_replacement_message_ids(
    messages: &mut [SessionMessageRecord],
    generation: u64,
) {
    for message in messages {
        message.message_id = format!("{}:generation:{generation}", message.message_id);
    }
}

pub(super) fn preflight_and_parse_new(
    provider: &'static str,
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    parse_new: impl FnOnce() -> Option<ParsedTranscript>,
) -> TranscriptIngestResult<Option<ParsedTranscript>> {
    preflight_strict_jsonl(provider, path, prev, max_new_bytes)?;
    Ok(parse_new())
}

#[cfg(test)]
mod tests;
