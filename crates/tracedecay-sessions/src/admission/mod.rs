//! Host-admission disposition values and the session-side admission port.
//!
//! The stateful admission facade (`HostAdmission`, its authorities, and
//! the registered-database bindings it holds) stays in the composition root:
//! it needs the registered global database, the observation store adapters, and
//! anchor resolution, none of which may be depended on from this crate.
//!
//! What lives here is everything the session runtime actually needs to *talk*
//! to that facade:
//!
//! * the disposition values it returns ([`HostAdmissionOutcome`],
//!   [`HostAdmissionStatus`], [`HostProjectionDrainOutcome`]),
//! * the record/spool bounds every provider discovery walk charges against,
//! * and [`HostAdmission`], the dyn-safe port the root facade implements.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use serde::Serialize;
use tracedecay_domain::{
    CanonicalObservationIdV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceIdentityV1, SanitizationReceiptV1,
};
use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};
use tracedecay_store::{ObservationBatchFallbackCause, ParseOffset};

use crate::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};

pub mod bounds;
pub mod disposition;
pub mod ingest;

pub use bounds::{DEFAULT_MAX_RECORD_BYTES, DEFAULT_MAX_RECORDS, DEFAULT_MAX_SPOOL_BYTES};
pub use disposition::{
    HostAdmissionDispositionClass, HostAdmissionStatus, HostAdmissionTelemetryDisposition,
    is_bounded_reason_code,
};
pub use ingest::{SESSION_INGEST_DISABLED_REASON_V1, session_ingest_disabled};

/// Boxed future returned by every [`HostAdmission`] method.
///
/// The port is deliberately dyn-safe: the session runtime threads one admission
/// handle through provider ingest, cursor advancement, and projection drains,
/// and a generic parameter would have to be carried by every intermediate
/// struct. Boxing once per admission call is immaterial next to the store
/// write it guards.
pub type AdmissionFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, HostAdmissionOutcome>> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostDiscoveryQueueEntry {
    pub sequence: u64,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HostAdmissionOutcome {
    pub status: HostAdmissionStatus,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<&'static str>,
    /// Typed recovery instruction for the in-process admission caller.
    ///
    /// This is deliberately omitted from host wire output: reason codes remain
    /// bounded telemetry, while recovery decisions must not be reconstructed
    /// from strings at another layer.
    #[serde(skip)]
    pub recovery: Option<HostAdmissionRecovery>,
    /// Operator-only storage cause for [`ObservationStoreError::Storage`].
    ///
    /// Host wire output stays reason-code-only. Admission callers that already
    /// carry a detail/message slot (MCP hook JSON-RPC `detail`) may copy this
    /// text; it is never reconstructed into a reason code.
    #[serde(skip)]
    pub storage_cause: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostAdmissionRecovery {
    BatchRequiresScalarFallback(ObservationBatchFallbackCause),
    DeterministicContentRefusal,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostProjectionDrainOutcome {
    pub projected: u64,
    pub projected_outputs: u64,
    pub skipped: u64,
    pub exact_duplicates: u64,
    pub deferred: bool,
    pub session_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostAdmissionScope {
    Project,
    Profile,
}

impl HostAdmissionOutcome {
    #[hotpath::skip]
    const fn new(
        status: HostAdmissionStatus,
        retryable: bool,
        reason_code: Option<&'static str>,
    ) -> Self {
        Self {
            status,
            retryable,
            reason_code,
            recovery: None,
            storage_cause: None,
        }
    }

    #[hotpath::skip]
    pub const fn batch_requires_scalar_fallback(cause: ObservationBatchFallbackCause) -> Self {
        Self {
            status: HostAdmissionStatus::Backpressured,
            retryable: true,
            reason_code: Some("batch_requires_scalar_fallback"),
            recovery: Some(HostAdmissionRecovery::BatchRequiresScalarFallback(cause)),
            storage_cause: None,
        }
    }

    #[hotpath::skip]
    pub const fn deterministic_content_refusal(reason_code: &'static str) -> Self {
        Self {
            status: HostAdmissionStatus::Degraded,
            retryable: false,
            reason_code: Some(reason_code),
            recovery: Some(HostAdmissionRecovery::DeterministicContentRefusal),
            storage_cause: None,
        }
    }

    #[hotpath::skip]
    pub const fn supported() -> Self {
        Self::new(HostAdmissionStatus::Supported, false, None)
    }

    #[hotpath::skip]
    pub const fn accepted_for_replay() -> Self {
        Self::new(HostAdmissionStatus::AcceptedForReplay, false, None)
    }

    #[hotpath::skip]
    pub const fn retained_backpressured(reason_code: &'static str) -> Self {
        Self::new(HostAdmissionStatus::Backpressured, true, Some(reason_code))
    }

    #[hotpath::skip]
    pub const fn retained_unavailable(reason_code: &'static str) -> Self {
        Self::new(HostAdmissionStatus::Unavailable, true, Some(reason_code))
    }

    #[hotpath::skip]
    pub const fn degraded(reason_code: &'static str) -> Self {
        Self::new(HostAdmissionStatus::Degraded, false, Some(reason_code))
    }

    #[hotpath::skip]
    pub const fn replay_completed(changed: bool, exact_duplicate: bool) -> Self {
        if changed {
            Self::new(HostAdmissionStatus::Committed, false, None)
        } else if exact_duplicate {
            Self::new(HostAdmissionStatus::ExactDuplicate, false, None)
        } else {
            Self::accepted_for_replay()
        }
    }

    #[hotpath::skip]
    pub const fn spool_overflow() -> Self {
        Self::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("spool_overflow"),
        )
    }

    #[hotpath::skip]
    pub const fn spool_record_too_large() -> Self {
        Self::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("spool_record_too_large"),
        )
    }

    /// Host-event wire or MCP/daemon JSON-RPC frame exceeded its respective
    /// bound ([`tracedecay_framing::MAX_WIRE_MESSAGE_BYTES`] or
    /// [`tracedecay_framing::MAX_MCP_JSONRPC_FRAME_BYTES`])
    /// before durable retention.
    /// Non-retryable; full payload is not retained.
    #[hotpath::skip]
    pub const fn wire_record_too_large() -> Self {
        Self::new(
            HostAdmissionStatus::Degraded,
            false,
            Some(tracedecay_framing::WIRE_RECORD_TOO_LARGE),
        )
    }

    #[hotpath::skip]
    pub const fn spool_source_too_large() -> Self {
        Self::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("spool_source_too_large"),
        )
    }

    #[hotpath::skip]
    pub const fn spool_corrupted() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("spool_corrupted"),
        )
    }

    #[hotpath::skip]
    pub const fn durable_payload_unsupported_version() -> Self {
        Self::retained_unavailable("host_event_payload_unsupported_version")
    }

    #[hotpath::skip]
    pub const fn durable_payload_malformed() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("host_event_payload_malformed"),
        )
    }

    #[hotpath::skip]
    pub const fn spool_ack_conflict() -> Self {
        Self::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("spool_ack_conflict"),
        )
    }

    #[hotpath::skip]
    pub const fn spool_recovery_required() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("spool_recovery_required"),
        )
    }

    #[hotpath::skip]
    pub const fn quarantine_full() -> Self {
        Self::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("spool_quarantine_full"),
        )
    }

    #[hotpath::skip]
    pub const fn quarantine_corrupted() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("spool_quarantine_corrupted"),
        )
    }

    #[hotpath::skip]
    pub const fn quarantine_recovery_required() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("spool_quarantine_recovery_required"),
        )
    }

    #[hotpath::skip]
    pub const fn project_authority_unbound() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("project_authority_unbound"),
        )
    }

    #[hotpath::skip]
    pub const fn project_authority_mismatch() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("project_authority_mismatch"),
        )
    }

    #[hotpath::skip]
    pub const fn registered_authority_unavailable() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("registered_authority_unavailable"),
        )
    }

    #[hotpath::skip]
    pub const fn parse_offset_conflict() -> Self {
        Self::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("parse_offset_conflict"),
        )
    }

    #[hotpath::skip]
    pub const fn observation_point_read_unavailable() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("observation_point_read_unavailable"),
        )
    }
}

pub(crate) fn is_admission_cancellation(
    outcome: &HostAdmissionOutcome,
    cancellation: &ObservationCancellation,
) -> bool {
    cancellation.is_cancelled()
        && outcome.status == HostAdmissionStatus::Backpressured
        && outcome.retryable
        && outcome.reason_code == Some("admission_cancelled")
}

/// Everything the session runtime asks of the host-admission facade.
///
/// This is the inverted seam for the former
/// `crate::admission::{ObservationCaptureAdmissionPort,
/// TranscriptCursorAdmissionPort}` pair: the traits were defined next to the
/// facade in the root crate, which put the whole session runtime downstream of
/// the composition root. Provider ingest now depends on this trait only.
pub trait HostAdmission: Send + Sync {
    /// Sanitizes and, when the authority permits it, durably persists one
    /// bounded provider record.
    fn capture_observation<'a>(
        &'a self,
        request: CaptureObservationRequest,
    ) -> AdmissionFuture<'a, CaptureObservationOutcome>;

    /// Sanitizes then persists a bounded window through one store-owned
    /// `persist_observations` call when the implementor owns that authority.
    ///
    /// The default walks [`Self::capture_observation`] so composition-root
    /// façades keep compiling until they override. An empty window returns
    /// empty without minting a skipped-authority success.
    fn capture_observations<'a>(
        &'a self,
        requests: Vec<CaptureObservationRequest>,
    ) -> AdmissionFuture<'a, Vec<CaptureObservationOutcome>> {
        Box::pin(async move {
            let mut outcomes = Vec::with_capacity(requests.len());
            for request in requests {
                outcomes.push(self.capture_observation(request).await?);
            }
            Ok(outcomes)
        })
    }

    /// Advances a non-durable frame cursor without persisting a record.
    fn advance_non_durable_source_cursor<'a>(
        &'a self,
        advance: ObservationCursorAdvance,
        cancellation: ObservationCancellation,
    ) -> AdmissionFuture<'a, CursorAdvanceOutcome>;

    /// Reads the admitted cursor for one observation source.
    fn get_source_cursor<'a>(
        &'a self,
        source: &'a ObservationSourceIdentityV1,
        scope: &'a ObservationScopeV1,
    ) -> AdmissionFuture<'a, Option<ObservationSourceCursorV1>>;

    /// Content-free point existence check for idempotent recovery. The
    /// default is a typed unavailable state; production composition overrides
    /// it with the canonical observation authority.
    fn observation_receipt<'a>(
        &'a self,
        _provider: &'a str,
        _scope: &'a ObservationScopeV1,
        _observation_id: &'a CanonicalObservationIdV1,
        _cancellation: &'a ObservationCancellation,
    ) -> AdmissionFuture<'a, Option<SanitizationReceiptV1>> {
        Box::pin(async { Err(HostAdmissionOutcome::observation_point_read_unavailable()) })
    }

    /// Drains up to `max` queued projections for one provider.
    fn drain_projection_queue<'a>(
        &'a self,
        provider: &'a str,
        scope: &'a ObservationScopeV1,
        cancellation: &'a ObservationCancellation,
        max: usize,
    ) -> AdmissionFuture<'a, HostProjectionDrainOutcome>;

    /// Whether one provider message is already durable under `scope`.
    ///
    /// Composer sweeps ask before re-reading a bubble: an already-admitted
    /// message must not be re-parsed out of the host's own store.
    fn has_session_message<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        provider: &'a str,
        message_id: &'a str,
    ) -> AdmissionFuture<'a, bool>;

    /// Reads the durable subset of one bounded provider message-id window.
    /// The default preserves compatibility for narrow test seams; production
    /// authorities override it with one indexed batch query.
    fn existing_session_message_ids<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        provider: &'a str,
        message_ids: Vec<String>,
    ) -> AdmissionFuture<'a, Vec<String>> {
        Box::pin(async move {
            let mut existing = Vec::new();
            for message_id in message_ids {
                if self
                    .has_session_message(scope, provider, &message_id)
                    .await?
                {
                    existing.push(message_id);
                }
            }
            Ok(existing)
        })
    }

    /// Reads one provider-owned value from the canonical session-backfill
    /// authority selected by `scope`.
    fn read_session_backfill_state<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _key: &'a str,
    ) -> AdmissionFuture<'a, Option<String>> {
        Box::pin(async { Err(HostAdmissionOutcome::registered_authority_unavailable()) })
    }

    /// Reads one bounded keyset page from the canonical session-backfill
    /// authority, excluding entries whose JSON status is `complete`.
    fn list_session_backfill_state_page<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _key_prefix: &'a str,
        _after_key: Option<&'a str>,
        _through_key: &'a str,
    ) -> AdmissionFuture<'a, Vec<(String, String)>> {
        Box::pin(async { Err(HostAdmissionOutcome::registered_authority_unavailable()) })
    }

    /// Returns the greatest incomplete key currently present under a prefix.
    fn session_backfill_state_high_water<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _key_prefix: &'a str,
    ) -> AdmissionFuture<'a, Option<String>> {
        Box::pin(async { Err(HostAdmissionOutcome::registered_authority_unavailable()) })
    }

    /// Inserts or atomically replaces one canonical session-backfill value.
    /// `expected = None` means the key must still be absent.
    fn compare_and_swap_session_backfill_state<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _key: &'a str,
        _expected: Option<&'a str>,
        _replacement: &'a str,
    ) -> AdmissionFuture<'a, bool> {
        Box::pin(async { Err(HostAdmissionOutcome::registered_authority_unavailable()) })
    }

    /// Deletes one canonical session-backfill value only if it is unchanged.
    fn compare_and_delete_session_backfill_state<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _key: &'a str,
        _expected: &'a str,
    ) -> AdmissionFuture<'a, bool> {
        Box::pin(async { Err(HostAdmissionOutcome::registered_authority_unavailable()) })
    }

    /// Reads the durable parse offset recorded for one transcript path.
    fn get_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
    ) -> AdmissionFuture<'a, Option<ParseOffset>>;

    /// Advances the durable parse offset for one transcript path.
    fn advance_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
        offset: ParseOffset,
    ) -> AdmissionFuture<'a, ()>;

    /// Replaces one typed parse-offset authority only when its exact prior
    /// value still matches. This is intentionally separate from monotonic
    /// transcript cursors: versioned state machines may move numeric fields in
    /// either direction without weakening ordinary cursor ordering.
    fn replace_parse_offset<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _path: &'a str,
        _expected: ParseOffset,
        _next: ParseOffset,
    ) -> AdmissionFuture<'a, ()> {
        Box::pin(async { Err(HostAdmissionOutcome::registered_authority_unavailable()) })
    }

    /// Atomically replaces two exact parse-offset authorities. This is the
    /// only supported write for state whose validity spans both keys.
    fn replace_parse_offset_pair<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _first: (&'a str, ParseOffset, ParseOffset),
        _second: (&'a str, ParseOffset, ParseOffset),
    ) -> AdmissionFuture<'a, ()> {
        Box::pin(async { Err(HostAdmissionOutcome::registered_authority_unavailable()) })
    }

    /// Adds provider paths to the durable discovery queue and returns the
    /// stable identity of the final input path.
    fn enqueue_discovery_paths<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _provider: &'a str,
        _paths: Vec<PathBuf>,
    ) -> AdmissionFuture<'a, Option<HostDiscoveryQueueEntry>> {
        Box::pin(async { Err(HostAdmissionOutcome::registered_authority_unavailable()) })
    }

    /// Reads a bounded queue window in stable insertion order.
    fn discovery_paths_after<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _provider: &'a str,
        _after_sequence: u64,
        _limit: usize,
    ) -> AdmissionFuture<'a, Vec<HostDiscoveryQueueEntry>> {
        Box::pin(async { Err(HostAdmissionOutcome::registered_authority_unavailable()) })
    }

    /// Resolves a stable queue identity back to its provider path.
    fn discovery_path<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _provider: &'a str,
        _sequence: u64,
    ) -> AdmissionFuture<'a, Option<HostDiscoveryQueueEntry>> {
        Box::pin(async { Err(HostAdmissionOutcome::registered_authority_unavailable()) })
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    use tracedecay_domain::{CanonicalObservationEnvelopeV1, CanonicalObservationIdV1};
    use tracedecay_runtime_core::privacy::RecordSanitizerV1;
    use tracedecay_store::observation::{
        ObservationPersistOutcome, ObservationStoreError, ObservationStoreResult,
    };
    use tracedecay_store::{
        AnchoredObservationWrite, ObservationBatchPersistOutcome, ObservationProjectionStatus,
        ObservationReplayRequest, ObservationStore, StoredObservation,
    };

    type SessionBackfillPagePause = (Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>);

    use crate::observation::{
        AdvanceNonDurableSourceCursorRequest, GetObservationRequest, ObservationApplication,
        ObservationApplicationError,
    };

    use super::*;

    /// Admission port for pre-cancellation tests that must fail if any host
    /// storage call is attempted.
    pub(crate) struct PanicHostAdmission;

    impl HostAdmission for PanicHostAdmission {
        fn capture_observation<'a>(
            &'a self,
            _request: CaptureObservationRequest,
        ) -> AdmissionFuture<'a, CaptureObservationOutcome> {
            panic!("pre-cancelled ingest attempted observation admission")
        }

        fn advance_non_durable_source_cursor<'a>(
            &'a self,
            _advance: ObservationCursorAdvance,
            _cancellation: ObservationCancellation,
        ) -> AdmissionFuture<'a, CursorAdvanceOutcome> {
            panic!("pre-cancelled ingest attempted cursor admission")
        }

        fn get_source_cursor<'a>(
            &'a self,
            _source: &'a ObservationSourceIdentityV1,
            _scope: &'a ObservationScopeV1,
        ) -> AdmissionFuture<'a, Option<ObservationSourceCursorV1>> {
            panic!("pre-cancelled ingest attempted cursor read")
        }

        fn drain_projection_queue<'a>(
            &'a self,
            _provider: &'a str,
            _scope: &'a ObservationScopeV1,
            _cancellation: &'a ObservationCancellation,
            _max: usize,
        ) -> AdmissionFuture<'a, HostProjectionDrainOutcome> {
            panic!("pre-cancelled ingest attempted projection drain")
        }

        fn has_session_message<'a>(
            &'a self,
            _scope: &'a ObservationScopeV1,
            _provider: &'a str,
            _message_id: &'a str,
        ) -> AdmissionFuture<'a, bool> {
            panic!("pre-cancelled ingest attempted session-message read")
        }

        fn get_parse_offset<'a>(
            &'a self,
            _scope: &'a ObservationScopeV1,
            _path: &'a str,
        ) -> AdmissionFuture<'a, Option<ParseOffset>> {
            panic!("pre-cancelled ingest attempted parse-offset read")
        }

        fn advance_parse_offset<'a>(
            &'a self,
            _scope: &'a ObservationScopeV1,
            _path: &'a str,
            _offset: ParseOffset,
        ) -> AdmissionFuture<'a, ()> {
            panic!("pre-cancelled ingest attempted parse-offset write")
        }
    }

    #[derive(Clone, Default)]
    struct MemoryObservationState {
        observations: Vec<StoredObservation>,
        cursors: Vec<ObservationSourceCursorV1>,
        projected_sequences: Vec<u64>,
        parse_offsets: Vec<(ObservationScopeV1, String, ParseOffset)>,
        discovery_paths: Vec<(ObservationScopeV1, String, HostDiscoveryQueueEntry)>,
        next_discovery_sequence: u64,
        capture_failures_remaining: usize,
        scalar_capture_calls: usize,
        batch_capture_calls: usize,
        session_message_failures_remaining: usize,
        session_message_reads: usize,
        session_backfill_state: Vec<(ObservationScopeV1, String, String)>,
        non_durable_advances: Vec<ObservationCursorAdvance>,
    }

    #[derive(Clone, Default)]
    struct MemoryObservationStore {
        state: Arc<Mutex<MemoryObservationState>>,
    }

    impl MemoryObservationStore {
        fn state(&self) -> std::sync::MutexGuard<'_, MemoryObservationState> {
            self.state
                .lock()
                .expect("memory observation store poisoned")
        }

        fn current_cursor(
            state: &MemoryObservationState,
            source: &ObservationSourceIdentityV1,
            scope: &ObservationScopeV1,
        ) -> Option<ObservationSourceCursorV1> {
            state
                .cursors
                .iter()
                .find(|cursor| cursor.source() == source && cursor.scope() == scope)
                .cloned()
        }

        fn replace_cursor(
            state: &mut MemoryObservationState,
            next_cursor: ObservationSourceCursorV1,
        ) {
            state.cursors.retain(|cursor| {
                cursor.source() != next_cursor.source() || cursor.scope() != next_cursor.scope()
            });
            state.cursors.push(next_cursor);
        }
    }

    impl MemoryObservationStore {
        fn persist_one(
            state: &mut MemoryObservationState,
            write: AnchoredObservationWrite,
        ) -> ObservationStoreResult<ObservationPersistOutcome> {
            if let Some(stored) = state.observations.iter().find(|stored| {
                stored.observation().observation_id() == write.observation().observation_id()
            }) {
                return Ok(ObservationPersistOutcome::ExactDuplicate(
                    stored.commit_receipt().clone(),
                ));
            }
            let actual = Self::current_cursor(
                state,
                write.next_cursor().source(),
                write.next_cursor().scope(),
            );
            if actual.as_ref() != write.expected_cursor() {
                return Err(ObservationStoreError::CursorConflict {
                    expected: Box::new(write.expected_cursor().cloned()),
                    actual: Box::new(actual),
                });
            }
            let sequence = u64::try_from(state.observations.len())
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            let (write, retrieval_anchor, projection_generation, repository_provenance) =
                write.into_parts();
            let (observation, _expected_cursor, next_cursor) = write.into_parts();
            let receipt = tracedecay_store::ObservationCommitReceipt::new(
                sequence,
                observation,
                next_cursor.clone(),
                retrieval_anchor,
                projection_generation,
            )?
            .with_repository_provenance_attachment(repository_provenance)?;
            state
                .observations
                .push(StoredObservation::from_commit_receipt(
                    receipt.clone(),
                    ObservationProjectionStatus::Queued,
                ));
            Self::replace_cursor(state, next_cursor);
            Ok(ObservationPersistOutcome::Committed(receipt))
        }
    }

    impl ObservationStore for MemoryObservationStore {
        #[hotpath::skip]
        async fn persist_observation(
            &self,
            write: AnchoredObservationWrite,
        ) -> ObservationStoreResult<ObservationPersistOutcome> {
            Self::persist_one(&mut self.state(), write)
        }

        #[hotpath::skip]
        async fn persist_observations(
            &self,
            writes: Vec<AnchoredObservationWrite>,
        ) -> ObservationStoreResult<Vec<ObservationBatchPersistOutcome>> {
            if writes.is_empty() {
                return Ok(Vec::new());
            }
            let mut state = self.state();
            let mut staged = state.clone();
            let mut outcomes = Vec::with_capacity(writes.len());
            for write in writes {
                outcomes.push(Self::persist_one(&mut staged, write)?);
            }
            let outcomes = outcomes
                .into_iter()
                .map(|outcome| {
                    let stored = staged
                        .observations
                        .iter()
                        .find(|stored| {
                            stored.observation().observation_id()
                                == outcome.receipt().observation().observation_id()
                        })
                        .cloned();
                    ObservationBatchPersistOutcome::new(outcome, stored)
                })
                .collect();
            *state = staged;
            Ok(outcomes)
        }

        #[hotpath::skip]
        async fn get_source_cursor(
            &self,
            source: &ObservationSourceIdentityV1,
            scope: &ObservationScopeV1,
        ) -> ObservationStoreResult<Option<ObservationSourceCursorV1>> {
            Ok(Self::current_cursor(&self.state(), source, scope))
        }

        #[hotpath::skip]
        async fn advance_source_cursor(
            &self,
            advance: ObservationCursorAdvance,
        ) -> ObservationStoreResult<CursorAdvanceOutcome> {
            let mut state = self.state();
            let next_cursor = advance.next_cursor();
            let actual = Self::current_cursor(&state, next_cursor.source(), next_cursor.scope());
            if actual.as_ref() == Some(next_cursor) {
                return Ok(CursorAdvanceOutcome::ExactDuplicate);
            }
            if actual.as_ref() != advance.expected_cursor() {
                return Err(ObservationStoreError::CursorConflict {
                    expected: Box::new(advance.expected_cursor().cloned()),
                    actual: Box::new(actual),
                });
            }
            Self::replace_cursor(&mut state, next_cursor.clone());
            Ok(CursorAdvanceOutcome::Committed)
        }

        #[hotpath::skip]
        async fn get_observation(
            &self,
            observation_id: &CanonicalObservationIdV1,
        ) -> ObservationStoreResult<Option<StoredObservation>> {
            Ok(self
                .state()
                .observations
                .iter()
                .find(|stored| stored.observation().observation_id() == observation_id)
                .cloned())
        }

        #[hotpath::skip]
        async fn replay_observations(
            &self,
            request: ObservationReplayRequest,
        ) -> ObservationStoreResult<Vec<StoredObservation>> {
            Ok(self
                .state()
                .observations
                .iter()
                .filter(|stored| stored.sequence() > request.after_sequence())
                .take(request.limit())
                .cloned()
                .collect())
        }
    }

    /// Cloneable admission fixture for provider tests that exercise the
    /// session-side protocol without composing the root database runtime.
    #[derive(Clone, Default)]
    pub(crate) struct MemoryHostAdmission {
        store: MemoryObservationStore,
        cancel_on_cursor_read: Arc<Mutex<Option<ObservationCancellation>>>,
        projection_failure: Arc<Mutex<Option<(HostAdmissionOutcome, ObservationCancellation)>>>,
        cancel_on_discovery_queue_read: Arc<Mutex<Option<ObservationCancellation>>>,
        session_backfill_page_pause: Arc<Mutex<Option<SessionBackfillPagePause>>>,
    }

    impl MemoryHostAdmission {
        pub(crate) fn observations(&self) -> Vec<StoredObservation> {
            self.store.state().observations.clone()
        }

        pub(crate) fn capture_call_counts(&self) -> (usize, usize) {
            let state = self.store.state();
            (state.scalar_capture_calls, state.batch_capture_calls)
        }

        pub(crate) fn fail_next_capture(&self) {
            self.store.state().capture_failures_remaining = 1;
        }

        /// Make the next `count` session-message lookups report the store as
        /// unavailable, the way reader-pool saturation does.
        pub(crate) fn fail_next_session_message_lookups(&self, count: usize) {
            self.store.state().session_message_failures_remaining = count;
        }

        pub(crate) fn pending_projection_count(&self) -> usize {
            let state = self.store.state();
            state
                .observations
                .iter()
                .filter(|stored| !state.projected_sequences.contains(&stored.sequence()))
                .count()
        }

        pub(crate) fn cancel_on_next_cursor_read(&self, cancellation: ObservationCancellation) {
            *self
                .cancel_on_cursor_read
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(cancellation);
        }

        pub(crate) fn fail_next_projection_drain_after_cancelling(
            &self,
            outcome: HostAdmissionOutcome,
            cancellation: ObservationCancellation,
        ) {
            *self
                .projection_failure
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some((outcome, cancellation));
        }

        pub(crate) fn cancel_on_next_discovery_queue_read(
            &self,
            cancellation: ObservationCancellation,
        ) {
            *self
                .cancel_on_discovery_queue_read
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(cancellation);
        }

        pub(crate) fn session_message_read_count(&self) -> usize {
            self.store.state().session_message_reads
        }

        pub(crate) fn session_backfill_state_entries(
            &self,
            key_prefix: &str,
        ) -> Vec<(ObservationScopeV1, String, String)> {
            self.store
                .state()
                .session_backfill_state
                .iter()
                .filter(|(_, key, _)| key.starts_with(key_prefix))
                .cloned()
                .collect()
        }

        pub(crate) fn pause_next_session_backfill_page(
            &self,
            entered: Arc<tokio::sync::Barrier>,
            release: Arc<tokio::sync::Barrier>,
        ) {
            *self
                .session_backfill_page_pause
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some((entered, release));
        }

        pub(crate) fn non_durable_advances(&self) -> Vec<ObservationCursorAdvance> {
            self.store.state().non_durable_advances.clone()
        }

        fn application(
            &self,
        ) -> Result<ObservationApplication<MemoryObservationStore>, HostAdmissionOutcome> {
            let sanitizer = RecordSanitizerV1::observation_v1()
                .map_err(|_| HostAdmissionOutcome::registered_authority_unavailable())?;
            Ok(ObservationApplication::new(self.store.clone(), sanitizer))
        }

        fn application_error(error: ObservationApplicationError) -> HostAdmissionOutcome {
            match error {
                ObservationApplicationError::Cancelled => {
                    HostAdmissionOutcome::retained_backpressured("admission_cancelled")
                }
                ObservationApplicationError::Store(
                    ObservationStoreError::BatchRequiresScalarFallback { cause },
                ) => HostAdmissionOutcome::batch_requires_scalar_fallback(cause),
                _ => HostAdmissionOutcome::registered_authority_unavailable(),
            }
        }
    }

    impl HostAdmission for MemoryHostAdmission {
        fn capture_observation<'a>(
            &'a self,
            request: CaptureObservationRequest,
        ) -> AdmissionFuture<'a, CaptureObservationOutcome> {
            Box::pin(async move {
                {
                    let mut state = self.store.state();
                    state.scalar_capture_calls = state.scalar_capture_calls.saturating_add(1);
                    if state.capture_failures_remaining > 0 {
                        state.capture_failures_remaining -= 1;
                        return Err(HostAdmissionOutcome::registered_authority_unavailable());
                    }
                }
                self.application()?
                    .capture_observation(request)
                    .await
                    .map_err(Self::application_error)
            })
        }

        fn capture_observations<'a>(
            &'a self,
            requests: Vec<CaptureObservationRequest>,
        ) -> AdmissionFuture<'a, Vec<CaptureObservationOutcome>> {
            Box::pin(async move {
                {
                    let mut state = self.store.state();
                    state.batch_capture_calls = state.batch_capture_calls.saturating_add(1);
                    if state.capture_failures_remaining > 0 {
                        state.capture_failures_remaining -= 1;
                        return Err(HostAdmissionOutcome::registered_authority_unavailable());
                    }
                }
                self.application()?
                    .capture_observations(requests)
                    .await
                    .map_err(Self::application_error)
            })
        }

        fn advance_non_durable_source_cursor<'a>(
            &'a self,
            advance: ObservationCursorAdvance,
            cancellation: ObservationCancellation,
        ) -> AdmissionFuture<'a, CursorAdvanceOutcome> {
            Box::pin(async move {
                let recorded = advance.clone();
                let outcome =
                    self.application()?
                        .advance_non_durable_source_cursor(
                            AdvanceNonDurableSourceCursorRequest::new(advance, cancellation),
                        )
                        .await
                        .map_err(Self::application_error)?;
                self.store.state().non_durable_advances.push(recorded);
                Ok(outcome)
            })
        }

        fn get_source_cursor<'a>(
            &'a self,
            source: &'a ObservationSourceIdentityV1,
            scope: &'a ObservationScopeV1,
        ) -> AdmissionFuture<'a, Option<ObservationSourceCursorV1>> {
            Box::pin(async move {
                if let Some(cancellation) = self
                    .cancel_on_cursor_read
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                {
                    cancellation.cancel();
                }
                self.store
                    .get_source_cursor(source, scope)
                    .await
                    .map_err(|_| HostAdmissionOutcome::registered_authority_unavailable())
            })
        }

        fn observation_receipt<'a>(
            &'a self,
            _provider: &'a str,
            _scope: &'a ObservationScopeV1,
            observation_id: &'a CanonicalObservationIdV1,
            cancellation: &'a ObservationCancellation,
        ) -> AdmissionFuture<'a, Option<SanitizationReceiptV1>> {
            Box::pin(async move {
                self.application()?
                    .get_observation(GetObservationRequest::new(
                        observation_id.clone(),
                        cancellation.clone(),
                    ))
                    .await
                    .map(|read| {
                        read.observation()
                            .map(|stored| stored.observation().receipt().clone())
                    })
                    .map_err(Self::application_error)
            })
        }

        fn drain_projection_queue<'a>(
            &'a self,
            provider: &'a str,
            scope: &'a ObservationScopeV1,
            _cancellation: &'a ObservationCancellation,
            max: usize,
        ) -> AdmissionFuture<'a, HostProjectionDrainOutcome> {
            Box::pin(async move {
                if let Some((outcome, cancellation)) = self
                    .projection_failure
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                {
                    cancellation.cancel();
                    return Err(outcome);
                }
                let mut state = self.store.state();
                let mut candidates = state
                    .observations
                    .iter()
                    .filter(|stored| {
                        stored.observation().source().provider().as_str() == provider
                            && stored.observation().scope() == scope
                            && !state.projected_sequences.contains(&stored.sequence())
                    })
                    .take(max.saturating_add(1))
                    .map(StoredObservation::sequence)
                    .collect::<Vec<_>>();
                let deferred = candidates.len() > max;
                candidates.truncate(max);
                let mut session_ids = Vec::new();
                for sequence in &candidates {
                    let Some(stored) = state
                        .observations
                        .iter()
                        .find(|stored| stored.sequence() == *sequence)
                    else {
                        continue;
                    };
                    let Ok(envelope) = serde_json::from_value::<CanonicalObservationEnvelopeV1>(
                        stored.observation().payload().clone(),
                    ) else {
                        continue;
                    };
                    let session_id = envelope.relations().session_id().as_str().to_owned();
                    if !session_ids.contains(&session_id) {
                        session_ids.push(session_id);
                    }
                }
                state.projected_sequences.extend(candidates.iter().copied());
                Ok(HostProjectionDrainOutcome {
                    projected: u64::try_from(candidates.len()).unwrap_or(u64::MAX),
                    projected_outputs: u64::try_from(candidates.len()).unwrap_or(u64::MAX),
                    deferred,
                    session_ids,
                    ..HostProjectionDrainOutcome::default()
                })
            })
        }

        fn has_session_message<'a>(
            &'a self,
            scope: &'a ObservationScopeV1,
            provider: &'a str,
            message_id: &'a str,
        ) -> AdmissionFuture<'a, bool> {
            Box::pin(async move {
                {
                    let mut state = self.store.state();
                    state.session_message_reads = state.session_message_reads.saturating_add(1);
                    if state.session_message_failures_remaining > 0 {
                        state.session_message_failures_remaining -= 1;
                        return Err(HostAdmissionOutcome::registered_authority_unavailable());
                    }
                }
                Ok(self.store.state().observations.iter().any(|stored| {
                    stored.observation().scope() == scope
                        && stored.observation().source().provider().as_str() == provider
                        && stored
                            .observation()
                            .payload()
                            .to_string()
                            .contains(message_id)
                }))
            })
        }

        fn existing_session_message_ids<'a>(
            &'a self,
            scope: &'a ObservationScopeV1,
            provider: &'a str,
            message_ids: Vec<String>,
        ) -> AdmissionFuture<'a, Vec<String>> {
            Box::pin(async move {
                {
                    let mut state = self.store.state();
                    if state.session_message_failures_remaining > 0 {
                        state.session_message_failures_remaining -= 1;
                        return Err(HostAdmissionOutcome::registered_authority_unavailable());
                    }
                }
                let state = self.store.state();
                Ok(message_ids
                    .into_iter()
                    .filter(|message_id| {
                        state.observations.iter().any(|stored| {
                            stored.observation().scope() == scope
                                && stored.observation().source().provider().as_str() == provider
                                && stored
                                    .observation()
                                    .payload()
                                    .to_string()
                                    .contains(message_id)
                        })
                    })
                    .collect())
            })
        }

        fn read_session_backfill_state<'a>(
            &'a self,
            scope: &'a ObservationScopeV1,
            key: &'a str,
        ) -> AdmissionFuture<'a, Option<String>> {
            Box::pin(async move {
                Ok(self
                    .store
                    .state()
                    .session_backfill_state
                    .iter()
                    .find(|(stored_scope, stored_key, _)| {
                        stored_scope == scope && stored_key == key
                    })
                    .map(|(_, _, value)| value.clone()))
            })
        }

        fn list_session_backfill_state_page<'a>(
            &'a self,
            scope: &'a ObservationScopeV1,
            key_prefix: &'a str,
            after_key: Option<&'a str>,
            through_key: &'a str,
        ) -> AdmissionFuture<'a, Vec<(String, String)>> {
            Box::pin(async move {
                let mut entries = {
                    let state = self.store.state();
                    state
                        .session_backfill_state
                        .iter()
                        .filter(|(stored_scope, key, value)| {
                            stored_scope == scope
                                && key.starts_with(key_prefix)
                                && after_key.is_none_or(|after| key.as_str() > after)
                                && key.as_str() <= through_key
                                && !serde_json::from_str::<serde_json::Value>(value)
                                    .ok()
                                    .is_some_and(|value| {
                                        value.get("status").and_then(serde_json::Value::as_str)
                                            == Some("complete")
                                    })
                        })
                        .map(|(_, key, value)| (key.clone(), value.clone()))
                        .collect::<Vec<_>>()
                };
                entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
                entries.truncate(8);
                let pause = self
                    .session_backfill_page_pause
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                if let Some((entered, release)) = pause {
                    entered.wait().await;
                    release.wait().await;
                }
                Ok(entries)
            })
        }

        fn session_backfill_state_high_water<'a>(
            &'a self,
            scope: &'a ObservationScopeV1,
            key_prefix: &'a str,
        ) -> AdmissionFuture<'a, Option<String>> {
            Box::pin(async move {
                Ok(self
                    .store
                    .state()
                    .session_backfill_state
                    .iter()
                    .filter(|(stored_scope, key, value)| {
                        stored_scope == scope
                            && key.starts_with(key_prefix)
                            && !serde_json::from_str::<serde_json::Value>(value)
                                .ok()
                                .is_some_and(|value| {
                                    value.get("status").and_then(serde_json::Value::as_str)
                                        == Some("complete")
                                })
                    })
                    .map(|(_, key, _)| key.clone())
                    .max())
            })
        }

        fn compare_and_swap_session_backfill_state<'a>(
            &'a self,
            scope: &'a ObservationScopeV1,
            key: &'a str,
            expected: Option<&'a str>,
            replacement: &'a str,
        ) -> AdmissionFuture<'a, bool> {
            Box::pin(async move {
                let mut state = self.store.state();
                let current = state.session_backfill_state.iter_mut().find(
                    |(stored_scope, stored_key, _)| stored_scope == scope && stored_key == key,
                );
                match (current, expected) {
                    (None, None) => {
                        state.session_backfill_state.push((
                            scope.clone(),
                            key.to_string(),
                            replacement.to_string(),
                        ));
                        Ok(true)
                    }
                    (Some((_, _, current)), Some(expected)) if current == expected => {
                        *current = replacement.to_string();
                        Ok(true)
                    }
                    _ => Ok(false),
                }
            })
        }

        fn compare_and_delete_session_backfill_state<'a>(
            &'a self,
            scope: &'a ObservationScopeV1,
            key: &'a str,
            expected: &'a str,
        ) -> AdmissionFuture<'a, bool> {
            Box::pin(async move {
                let mut state = self.store.state();
                let Some(index) = state.session_backfill_state.iter().position(
                    |(stored_scope, stored_key, value)| {
                        stored_scope == scope && stored_key == key && value == expected
                    },
                ) else {
                    return Ok(false);
                };
                state.session_backfill_state.remove(index);
                Ok(true)
            })
        }

        fn get_parse_offset<'a>(
            &'a self,
            scope: &'a ObservationScopeV1,
            path: &'a str,
        ) -> AdmissionFuture<'a, Option<ParseOffset>> {
            Box::pin(async move {
                Ok(self
                    .store
                    .state()
                    .parse_offsets
                    .iter()
                    .find(|(stored_scope, stored_path, _)| {
                        stored_scope == scope && stored_path == path
                    })
                    .map(|(_, _, offset)| *offset))
            })
        }

        fn advance_parse_offset<'a>(
            &'a self,
            scope: &'a ObservationScopeV1,
            path: &'a str,
            offset: ParseOffset,
        ) -> AdmissionFuture<'a, ()> {
            Box::pin(async move {
                let mut state = self.store.state();
                state
                    .parse_offsets
                    .retain(|(stored_scope, stored_path, _)| {
                        stored_scope != scope || stored_path != path
                    });
                state
                    .parse_offsets
                    .push((scope.clone(), path.to_owned(), offset));
                Ok(())
            })
        }

        fn replace_parse_offset<'a>(
            &'a self,
            scope: &'a ObservationScopeV1,
            path: &'a str,
            expected: ParseOffset,
            next: ParseOffset,
        ) -> AdmissionFuture<'a, ()> {
            Box::pin(async move {
                let mut state = self.store.state();
                let actual = state
                    .parse_offsets
                    .iter()
                    .find(|(stored_scope, stored_path, _)| {
                        stored_scope == scope && stored_path == path
                    })
                    .map(|(_, _, offset)| *offset)
                    .unwrap_or_default();
                if actual != expected {
                    return Err(HostAdmissionOutcome::parse_offset_conflict());
                }
                state
                    .parse_offsets
                    .retain(|(stored_scope, stored_path, _)| {
                        stored_scope != scope || stored_path != path
                    });
                state
                    .parse_offsets
                    .push((scope.clone(), path.to_owned(), next));
                Ok(())
            })
        }

        fn replace_parse_offset_pair<'a>(
            &'a self,
            scope: &'a ObservationScopeV1,
            first: (&'a str, ParseOffset, ParseOffset),
            second: (&'a str, ParseOffset, ParseOffset),
        ) -> AdmissionFuture<'a, ()> {
            Box::pin(async move {
                let mut state = self.store.state();
                let actual = |path: &str| {
                    state
                        .parse_offsets
                        .iter()
                        .find(|(stored_scope, stored_path, _)| {
                            stored_scope == scope && stored_path == path
                        })
                        .map(|(_, _, offset)| *offset)
                        .unwrap_or_default()
                };
                if actual(first.0) != first.1 || actual(second.0) != second.1 {
                    return Err(HostAdmissionOutcome::parse_offset_conflict());
                }
                state
                    .parse_offsets
                    .retain(|(stored_scope, stored_path, _)| {
                        stored_scope != scope || (stored_path != first.0 && stored_path != second.0)
                    });
                state
                    .parse_offsets
                    .push((scope.clone(), first.0.to_owned(), first.2));
                state
                    .parse_offsets
                    .push((scope.clone(), second.0.to_owned(), second.2));
                Ok(())
            })
        }

        fn enqueue_discovery_paths<'a>(
            &'a self,
            scope: &'a ObservationScopeV1,
            provider: &'a str,
            paths: Vec<PathBuf>,
        ) -> AdmissionFuture<'a, Option<HostDiscoveryQueueEntry>> {
            Box::pin(async move {
                let mut state = self.store.state();
                let mut last_entry = None;
                for path in paths {
                    let existing = state
                        .discovery_paths
                        .iter()
                        .find(|(stored_scope, stored_provider, entry)| {
                            stored_scope == scope
                                && stored_provider == provider
                                && entry.path == path
                        })
                        .map(|(_, _, entry)| entry.clone());
                    let entry = match existing {
                        Some(entry) => entry,
                        None => {
                            state.next_discovery_sequence =
                                state.next_discovery_sequence.saturating_add(1);
                            let entry = HostDiscoveryQueueEntry {
                                sequence: state.next_discovery_sequence,
                                path,
                            };
                            state.discovery_paths.push((
                                scope.clone(),
                                provider.to_owned(),
                                entry.clone(),
                            ));
                            entry
                        }
                    };
                    last_entry = Some(entry);
                }
                Ok(last_entry)
            })
        }

        fn discovery_paths_after<'a>(
            &'a self,
            scope: &'a ObservationScopeV1,
            provider: &'a str,
            after_sequence: u64,
            limit: usize,
        ) -> AdmissionFuture<'a, Vec<HostDiscoveryQueueEntry>> {
            Box::pin(async move {
                if let Some(cancellation) = self
                    .cancel_on_discovery_queue_read
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                {
                    cancellation.cancel();
                }
                let mut entries = self
                    .store
                    .state()
                    .discovery_paths
                    .iter()
                    .filter(|(stored_scope, stored_provider, entry)| {
                        stored_scope == scope
                            && stored_provider == provider
                            && entry.sequence > after_sequence
                    })
                    .map(|(_, _, entry)| entry.clone())
                    .collect::<Vec<_>>();
                entries.sort_by_key(|entry| entry.sequence);
                entries.truncate(limit);
                Ok(entries)
            })
        }

        fn discovery_path<'a>(
            &'a self,
            scope: &'a ObservationScopeV1,
            provider: &'a str,
            sequence: u64,
        ) -> AdmissionFuture<'a, Option<HostDiscoveryQueueEntry>> {
            Box::pin(async move {
                Ok(self
                    .store
                    .state()
                    .discovery_paths
                    .iter()
                    .find(|(stored_scope, stored_provider, entry)| {
                        stored_scope == scope
                            && stored_provider == provider
                            && entry.sequence == sequence
                    })
                    .map(|(_, _, entry)| entry.clone()))
            })
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod wire_disposition_tests {
    use super::{HostAdmissionOutcome, HostAdmissionStatus};
    use tracedecay_framing::WIRE_RECORD_TOO_LARGE;

    #[test]
    fn wire_oversized_maps_to_typed_non_durable_outcome_without_payload() {
        let outcome = HostAdmissionOutcome::wire_record_too_large();
        assert_eq!(outcome.status, HostAdmissionStatus::Degraded);
        assert!(!outcome.retryable);
        assert_eq!(outcome.reason_code, Some(WIRE_RECORD_TOO_LARGE));
        let encoded = serde_json::to_string(&outcome).unwrap();
        assert!(!encoded.contains('x'));
        assert!(encoded.contains(WIRE_RECORD_TOO_LARGE));
    }
}
