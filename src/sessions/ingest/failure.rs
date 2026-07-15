use serde::Serialize;

use crate::sessions::{claude_observation, source};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClaudeObservationFailureClass {
    pub reason_code: &'static str,
    pub retryable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct TranscriptCatchUpFailure {
    pub provider: &'static str,
    pub source: &'static str,
    pub reason_code: &'static str,
    pub retryable: bool,
}

impl TranscriptCatchUpFailure {
    pub(super) const fn new(
        provider: &'static str,
        source: &'static str,
        reason_code: &'static str,
        retryable: bool,
    ) -> Self {
        Self {
            provider,
            source,
            reason_code,
            retryable,
        }
    }
}

pub(crate) fn classify_transcript_ingest_failure(
    provider: &'static str,
    source: &'static str,
    error: &source::TranscriptIngestError,
) -> TranscriptCatchUpFailure {
    use tracedecay_store::TranscriptStoreError;

    let (reason_code, retryable) = match error {
        source::TranscriptIngestError::Store(TranscriptStoreError::Conflict { .. }) => {
            ("transcript_cursor_conflict", true)
        }
        source::TranscriptIngestError::Store(TranscriptStoreError::Storage { .. }) => {
            ("transcript_storage_failed", true)
        }
        source::TranscriptIngestError::Store(TranscriptStoreError::InvalidCursorPath) => {
            ("transcript_cursor_path_invalid", false)
        }
        source::TranscriptIngestError::Store(TranscriptStoreError::InvalidTranscriptPath) => {
            ("transcript_path_invalid", false)
        }
        source::TranscriptIngestError::Store(TranscriptStoreError::MissingTranscriptPath {
            ..
        }) => ("transcript_path_missing", false),
        source::TranscriptIngestError::Store(TranscriptStoreError::MessageIdentityMismatch {
            ..
        }) => ("transcript_message_identity_mismatch", false),
        source::TranscriptIngestError::CursorKeyMismatch { .. } => {
            ("transcript_cursor_key_mismatch", false)
        }
        source::TranscriptIngestError::ScanIo { .. } => ("transcript_source_io_failed", true),
        source::TranscriptIngestError::ScanGenerationChanged { .. } => {
            ("transcript_source_generation_changed", true)
        }
        source::TranscriptIngestError::Privacy(_) => ("transcript_privacy_rejected", false),
        source::TranscriptIngestError::NonDurableRecord { .. } => {
            ("transcript_record_non_durable", false)
        }
        source::TranscriptIngestError::Domain(_)
        | source::TranscriptIngestError::ObservationContract(_)
        | source::TranscriptIngestError::InvalidFrameState { .. }
        | source::TranscriptIngestError::InvalidSourceIdentity { .. } => {
            ("transcript_source_contract_invalid", false)
        }
    };
    TranscriptCatchUpFailure::new(provider, source, reason_code, retryable)
}

pub(super) fn claude_catch_up_failure(
    source: &'static str,
    error: &claude_observation::ClaudeObservationIngestError,
) -> TranscriptCatchUpFailure {
    let failure = classify_claude_observation_failure(error);
    TranscriptCatchUpFailure::new("claude", source, failure.reason_code, failure.retryable)
}

pub(crate) fn classify_claude_observation_failure(
    error: &claude_observation::ClaudeObservationIngestError,
) -> ClaudeObservationFailureClass {
    use claude_observation::ClaudeObservationIngestError as Ingest;
    use tracedecay_store::{ObservationStoreError as Store, ProjectionStoreError as Projection};

    let permanent = |reason_code| ClaudeObservationFailureClass {
        reason_code,
        retryable: false,
    };
    let retryable = |reason_code| ClaudeObservationFailureClass {
        reason_code,
        retryable: true,
    };
    let store = |error: &Store| match error {
        Store::CursorConflict { .. } => retryable("observation_cursor_conflict"),
        Store::CursorAdvanceCollision => permanent("observation_cursor_advance_collision"),
        Store::ObservationCollision { .. } => permanent("observation_identity_collision"),
        Store::SanitizationReceiptCollision => permanent("sanitization_receipt_collision"),
        Store::CursorObservationMismatch => permanent("observation_cursor_mismatch"),
        Store::CursorCoverageMismatch => permanent("observation_cursor_coverage_gap"),
        Store::InvalidReplayLimit { .. } => permanent("observation_replay_limit_invalid"),
        Store::Contract(_) => permanent("observation_contract_invalid"),
        _ => retryable("observation_storage_failed"),
    };
    let projection = |error: &Projection| match error {
        Projection::Storage { .. } => retryable("observation_projection_storage_failed"),
        Projection::Gap { .. } => permanent("observation_projection_checkpoint_gap"),
        Projection::OutputCollision { .. } => permanent("observation_projection_output_collision"),
        Projection::ProvenanceCollision => permanent("observation_projection_provenance_collision"),
        Projection::Contract(_) => permanent("observation_projection_contract_invalid"),
        Projection::SequenceOverflow(_) => permanent("observation_projection_sequence_overflow"),
        Projection::NotQueued => permanent("observation_projection_not_queued"),
        Projection::ObservationNotFound => permanent("observation_projection_source_missing"),
        Projection::UnsupportedProvider(_) => {
            permanent("observation_projection_provider_unsupported")
        }
        Projection::InvalidRebuildFrontier { .. } => {
            permanent("observation_projection_frontier_invalid")
        }
    };
    let transcript = |error: &source::TranscriptIngestError| {
        let failure = classify_transcript_ingest_failure("claude", "transcript", error);
        ClaudeObservationFailureClass {
            reason_code: failure.reason_code,
            retryable: failure.retryable,
        }
    };

    match error {
        Ingest::Domain(_) => permanent("observation_domain_invalid"),
        Ingest::Contract(_) => permanent("observation_contract_invalid"),
        Ingest::Request(_) => permanent("observation_request_invalid"),
        Ingest::Privacy(_) => permanent("observation_privacy_rejected"),
        Ingest::Store(error) => store(error),
        Ingest::Projection(error) => projection(error),
        Ingest::Transcript(error) => transcript(error),
        Ingest::Application(error) => match error {
            crate::application::observation::ObservationApplicationError::Store(error) => {
                store(error)
            }
            crate::application::observation::ObservationApplicationError::Cancelled => {
                retryable("observation_cancelled")
            }
            crate::application::observation::ObservationApplicationError::PersistedObservationUnavailable => {
                retryable("observation_persisted_value_unavailable")
            }
            crate::application::observation::ObservationApplicationError::Contract(_) => {
                permanent("observation_contract_invalid")
            }
            crate::application::observation::ObservationApplicationError::Privacy(_) => {
                permanent("observation_privacy_rejected")
            }
        },
        Ingest::MissingParsedRecord => permanent("observation_parsed_record_missing"),
        Ingest::InvalidFrameState => permanent("observation_frame_state_invalid"),
        Ingest::NonContiguousCoverage => permanent("observation_scanner_coverage_gap"),
        Ingest::SourceFailures {
            first_reason_code,
            first_retryable,
            ..
        } => ClaudeObservationFailureClass {
            reason_code: first_reason_code,
            retryable: *first_retryable,
        },
    }
}
