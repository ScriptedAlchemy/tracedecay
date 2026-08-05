use crate::{
    ProviderRunFailure, ProviderRunFold as GenericProviderRunFold,
    ProviderRunOutcome as GenericProviderRunOutcome,
};
use serde::Serialize;
use tracedecay_domain::ObservationSourceRangeV1;

use crate::runtime::shared::TranscriptIngestStats;
use crate::runtime::{claude_observation, source};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaudeObservationFailureClass {
    pub reason_code: &'static str,
    pub retryable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TranscriptCatchUpFailure {
    pub provider: &'static str,
    pub source: &'static str,
    pub reason_code: &'static str,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_locator: Option<ObservationSourceRangeV1>,
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
            source_locator: None,
        }
    }

    const fn with_source_locator(
        mut self,
        source_locator: Option<ObservationSourceRangeV1>,
    ) -> Self {
        self.source_locator = source_locator;
        self
    }

    /// Typed overload when the bounded multi-source pass cannot admit more work.
    pub(super) const fn pass_backpressured() -> Self {
        Self::new("scheduler", "pass", "ingest_pass_backpressured", true)
    }

    /// Typed cancellation before the pass finished covering admitted work.
    pub(super) const fn pass_cancelled() -> Self {
        Self::new("scheduler", "pass", "ingest_pass_cancelled", true)
    }

    pub(super) const fn pass_frontier_unavailable() -> Self {
        Self::new("scheduler", "frontier", "ingest_frontier_unavailable", true)
    }

    /// Mutable session ingestion requires a retained daemon registry mount.
    /// Compatibility callers without that mount must fail before touching the
    /// legacy database or any provider source.
    #[cfg(any(test, feature = "test-helpers"))]
    pub(super) const fn registered_authority_unavailable(provider: &'static str) -> Self {
        Self::new(
            provider,
            "host_admission",
            "registered_authority_unavailable",
            true,
        )
    }
}

impl ProviderRunFailure for TranscriptCatchUpFailure {
    fn retryable(&self) -> bool {
        self.retryable
    }
}

pub(super) type ProviderRunOutcome = GenericProviderRunOutcome<TranscriptCatchUpFailure>;
pub(super) type ProviderRunFold = GenericProviderRunFold<TranscriptCatchUpFailure>;

/// Hard limits for one multi-source ingest pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IngestPassBounds {
    /// Maximum work units discovered before discovery itself is truncated.
    pub discovered_units: usize,
    /// Maximum work units admitted into one pass after fair rotation.
    pub units_per_pass: usize,
    /// Maximum pending/admitted units attributed to any single source.
    pub units_per_source: usize,
    /// Maximum pending queue depth across all sources during admission.
    pub queue_depth: usize,
    /// Maximum newly-read source bytes charged to one admitted unit.
    pub bytes_per_unit: u64,
    /// Maximum newly-read source bytes charged across the whole pass.
    pub bytes_per_pass: u64,
    /// Maximum in-pass retry attempts for a failed unit (0 = isolate and continue).
    pub retries: usize,
}

/// Typed coverage / overload outcome for one bounded ingest pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestPassCoverage {
    /// Every discovered unit was admitted and dispositioned in this pass.
    Complete,
    /// Some discovered work remains; a durable scheduling frontier may advance.
    Partial { deferred_units: u64 },
    /// Admission hit a hard queue/source/pass bound before the discovered set fit.
    Backpressured {
        admitted_units: u64,
        rejected_units: u64,
    },
}

impl IngestPassCoverage {
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// Narrow additive pass result required by PR6 bounded multi-source scheduling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngestPassOutcome {
    pub stats: TranscriptIngestStats,
    pub failures: Vec<TranscriptCatchUpFailure>,
    pub coverage: IngestPassCoverage,
    /// True only when a durable scheduling frontier write was performed.
    pub scheduling_state_written: bool,
    pub units_admitted: u64,
    pub units_completed: u64,
    pub units_failed: u64,
    /// False when an admitted provider API performs an internally unbounded
    /// sweep and therefore cannot honor the pass byte budget end-to-end.
    pub byte_bounds_enforced: bool,
}

impl IngestPassOutcome {
    pub(super) fn failed(failure: TranscriptCatchUpFailure) -> Self {
        Self {
            stats: TranscriptIngestStats::default(),
            failures: vec![failure],
            coverage: IngestPassCoverage::Complete,
            scheduling_state_written: false,
            units_admitted: 0,
            units_completed: 0,
            units_failed: 0,
            byte_bounds_enforced: true,
        }
    }

    pub(super) fn into_transcript_outcome(self) -> super::startup::TranscriptIngestOutcome {
        super::startup::TranscriptIngestOutcome::from_pass(self)
    }
}

/// Pure round-robin admission over a discovered unit count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RoundRobinAdmission {
    pub admitted_indices: Vec<usize>,
    pub coverage: IngestPassCoverage,
}

/// Rotate `[0, discovered)` by `frontier_offset`, then admit at most `max_units`.
///
/// A fully covered pass (`discovered <= max_units`) reports complete coverage;
/// callers persist rotation only for bounded partial passes.
pub(super) fn plan_round_robin_admission(
    discovered: usize,
    frontier_offset: u64,
    max_units: usize,
) -> RoundRobinAdmission {
    if discovered == 0 {
        return RoundRobinAdmission {
            admitted_indices: Vec::new(),
            coverage: IngestPassCoverage::Complete,
        };
    }
    if max_units == 0 {
        return RoundRobinAdmission {
            admitted_indices: Vec::new(),
            coverage: IngestPassCoverage::Backpressured {
                admitted_units: 0,
                rejected_units: u64::try_from(discovered).unwrap_or(u64::MAX),
            },
        };
    }
    let start = usize::try_from(frontier_offset).unwrap_or(usize::MAX) % discovered;
    let admitted = discovered.min(max_units);
    let order = (start..discovered).chain(0..start).take(admitted).collect();
    let deferred = discovered - admitted;
    let coverage = if deferred == 0 {
        IngestPassCoverage::Complete
    } else {
        IngestPassCoverage::Partial {
            deferred_units: u64::try_from(deferred).unwrap_or(u64::MAX),
        }
    };
    RoundRobinAdmission {
        admitted_indices: order,
        coverage,
    }
}

pub(super) fn allocate_pass_byte_budgets(unit_count: usize, bounds: IngestPassBounds) -> Vec<u64> {
    let mut remaining = bounds.bytes_per_pass;
    let mut budgets = Vec::with_capacity(unit_count.min(bounds.units_per_pass));
    while budgets.len() < unit_count && remaining > 0 && bounds.bytes_per_unit > 0 {
        let grant = remaining.min(bounds.bytes_per_unit);
        budgets.push(grant);
        remaining = remaining.saturating_sub(grant);
    }
    budgets
}

/// Decide whether a completed pass should persist a scheduling frontier.
///
/// Cancellation and full coverage never write. Partial / backpressured passes
/// write only when at least one unit was attempted so rotation can continue.
pub(super) fn scheduling_write_required(
    coverage: IngestPassCoverage,
    attempted_units: usize,
    cancelled: bool,
) -> bool {
    if cancelled || attempted_units == 0 || coverage.is_complete() {
        return false;
    }
    matches!(
        coverage,
        IngestPassCoverage::Partial { .. } | IngestPassCoverage::Backpressured { .. }
    )
}

pub fn classify_transcript_ingest_failure(
    provider: &'static str,
    source: &'static str,
    error: &source::TranscriptIngestError,
) -> TranscriptCatchUpFailure {
    use tracedecay_store::TranscriptStoreError;

    if let source::TranscriptIngestError::NonDurableRecord {
        offset,
        end_offset,
        reason,
        ..
    } = error
    {
        return TranscriptCatchUpFailure::new(
            provider,
            source,
            non_durable_reason_code(reason),
            false,
        )
        .with_source_locator(ObservationSourceRangeV1::new(*offset, *end_offset).ok());
    }

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
        source::TranscriptIngestError::NonDurableRecord { .. } => unreachable!(),
        source::TranscriptIngestError::Domain(_)
        | source::TranscriptIngestError::ObservationContract(_)
        | source::TranscriptIngestError::InvalidFrameState { .. }
        | source::TranscriptIngestError::InvalidSourceIdentity { .. } => {
            ("transcript_source_contract_invalid", false)
        }
    };
    TranscriptCatchUpFailure::new(provider, source, reason_code, retryable)
}

fn non_durable_reason_code(reason: &'static str) -> &'static str {
    match reason {
        "normalized observation record is not durable" => "normalized_observation_not_durable",
        "malformed snapshot JSON" => "malformed_snapshot_json",
        "unsupported execution index snapshot" => "unsupported_execution_index_snapshot",
        "snapshot contains no durable messages" => "snapshot_no_durable_messages",
        "snapshot exceeds provider byte bound" => "snapshot_byte_bound_exceeded",
        "snapshot message count exceeds provider bound" => "snapshot_message_bound_exceeded",
        "unsupported snapshot message layout" => "unsupported_snapshot_message_layout",
        "unsupported snapshot root" => "unsupported_snapshot_root",
        "malformed usage snapshot JSON" => "malformed_usage_snapshot_json",
        "unsupported usage snapshot root" => "unsupported_usage_snapshot_root",
        "usage event count exceeds provider bound" => "usage_event_bound_exceeded",
        "snapshot input exceeds provider byte bound" => "snapshot_input_byte_bound_exceeded",
        "snapshot metadata exceeds provider byte bound" => "snapshot_metadata_byte_bound_exceeded",
        reason if crate::admission::is_bounded_reason_code(reason) => reason,
        _ => "transcript_record_non_durable",
    }
}

/// Classify an observation drain error under any provider label.
pub(super) fn observation_catch_up_failure(
    provider: &'static str,
    source: &'static str,
    error: &claude_observation::ClaudeObservationIngestError,
) -> TranscriptCatchUpFailure {
    let failure = classify_claude_observation_failure(error);
    TranscriptCatchUpFailure::new(provider, source, failure.reason_code, failure.retryable)
}

pub(super) fn claude_catch_up_failure(
    source: &'static str,
    error: &claude_observation::ClaudeObservationIngestError,
) -> TranscriptCatchUpFailure {
    observation_catch_up_failure("claude", source, error)
}

/// Classify a transcript catch-up error, warn its bounded reason code, and
/// return the typed failure.
pub(super) fn warn_transcript_catch_up_failure(
    provider: &'static str,
    source: &'static str,
    error: &source::TranscriptIngestError,
    message: &'static str,
) -> TranscriptCatchUpFailure {
    let failure = classify_transcript_ingest_failure(provider, source, error);
    tracing::warn!(
        reason_code = failure.reason_code,
        retryable = failure.retryable,
        "{message}"
    );
    failure
}

pub fn classify_claude_observation_failure(
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
        Projection::RetryDeferred { .. } => retryable("observation_projection_retry_deferred"),
        Projection::Gap { .. } => permanent("observation_projection_checkpoint_gap"),
        Projection::OutputCollision { .. } => permanent("observation_projection_output_collision"),
        Projection::ProvenanceCollision => permanent("observation_projection_provenance_collision"),
        Projection::Contract(_) => permanent("observation_projection_contract_invalid"),
        Projection::Anchor(_) => permanent("observation_projection_anchor_invalid"),
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
            crate::observation::ObservationApplicationError::Store(error) => store(error),
            crate::observation::ObservationApplicationError::Cancelled => {
                retryable("observation_cancelled")
            }
            crate::observation::ObservationApplicationError::PersistedObservationUnavailable => {
                retryable("observation_persisted_value_unavailable")
            }
            crate::observation::ObservationApplicationError::Contract(_) => {
                permanent("observation_contract_invalid")
            }
            crate::observation::ObservationApplicationError::Privacy(_) => {
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
