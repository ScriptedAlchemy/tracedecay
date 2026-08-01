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
//! * the bounded-wire helpers shared by every host input reader,
//! * the record/spool bounds every provider discovery walk charges against,
//! * and [`HostAdmission`], the dyn-safe port the root facade implements.
//!
//! Root wiring: `src/application/host_admission.rs` must drop its own copies of
//! these values and re-export them from here, then add
//! `impl HostAdmission for dyn HostAdmission`.

use std::future::Future;
use std::pin::Pin;

use serde::Serialize;
use tracedecay_domain::{
    ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceIdentityV1,
};
use tracedecay_store::ParseOffset;
use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};

use crate::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};

pub mod bounds;
pub mod disposition;
pub mod wire;

pub use bounds::{DEFAULT_MAX_RECORD_BYTES, DEFAULT_MAX_RECORDS, DEFAULT_MAX_SPOOL_BYTES};
pub use disposition::{
    HostAdmissionDispositionClass, HostAdmissionStatus, HostAdmissionTelemetryDisposition,
    is_bounded_reason_code,
};
pub use wire::{
    MAX_MCP_JSONRPC_FRAME_BYTES, MAX_WIRE_MESSAGE_BYTES, MCP_OVERSIZE_ID_INSPECT_BYTES,
    WIRE_RECORD_TOO_LARGE, WireReadOutcome, is_wire_oversized_io_error, read_bounded_mcp_line,
    read_bounded_to_string, wire_oversized_inspect_prefix, wire_oversized_io_error,
    wire_oversized_io_error_with_prefix,
};

/// Boxed future returned by every [`HostAdmission`] method.
///
/// The port is deliberately dyn-safe: the session runtime threads one admission
/// handle through provider ingest, cursor advancement, and projection drains,
/// and a generic parameter would have to be carried by every intermediate
/// struct. Boxing once per admission call is immaterial next to the store
/// write it guards.
pub type AdmissionFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, HostAdmissionOutcome>> + Send + 'a>>;

/// Terminal disposition of one host-admission attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct HostAdmissionOutcome {
    pub status: HostAdmissionStatus,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<&'static str>,
}

/// Counts reported by one bounded projection-queue drain.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostProjectionDrainOutcome {
    pub projected: u64,
    pub projected_outputs: u64,
    pub skipped: u64,
    pub exact_duplicates: u64,
    pub session_ids: Vec<String>,
}

/// Which registered authority an admission call must be bound to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostAdmissionScope {
    Project,
    Profile,
}

impl HostAdmissionOutcome {
    const fn new(
        status: HostAdmissionStatus,
        retryable: bool,
        reason_code: Option<&'static str>,
    ) -> Self {
        Self {
            status,
            retryable,
            reason_code,
        }
    }

    pub const fn supported() -> Self {
        Self::new(HostAdmissionStatus::Supported, false, None)
    }

    pub const fn accepted_for_replay() -> Self {
        Self::new(HostAdmissionStatus::AcceptedForReplay, false, None)
    }

    pub const fn retained_backpressured(reason_code: &'static str) -> Self {
        Self::new(HostAdmissionStatus::Backpressured, true, Some(reason_code))
    }

    pub const fn retained_unavailable(reason_code: &'static str) -> Self {
        Self::new(HostAdmissionStatus::Unavailable, true, Some(reason_code))
    }

    pub const fn degraded(reason_code: &'static str) -> Self {
        Self::new(HostAdmissionStatus::Degraded, false, Some(reason_code))
    }

    pub const fn replay_completed(changed: bool, exact_duplicate: bool) -> Self {
        if changed {
            Self::new(HostAdmissionStatus::Committed, false, None)
        } else if exact_duplicate {
            Self::new(HostAdmissionStatus::ExactDuplicate, false, None)
        } else {
            Self::accepted_for_replay()
        }
    }

    pub const fn spool_overflow() -> Self {
        Self::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("spool_overflow"),
        )
    }

    pub const fn spool_record_too_large() -> Self {
        Self::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("spool_record_too_large"),
        )
    }

    /// Host-event wire or MCP/daemon JSON-RPC frame exceeded its respective
    /// bound ([`wire::MAX_WIRE_MESSAGE_BYTES`] or
    /// [`wire::MAX_MCP_JSONRPC_FRAME_BYTES`]) before durable retention.
    /// Non-retryable; full payload is not retained.
    pub const fn wire_record_too_large() -> Self {
        Self::new(
            HostAdmissionStatus::Degraded,
            false,
            Some(wire::WIRE_RECORD_TOO_LARGE),
        )
    }

    pub const fn spool_source_too_large() -> Self {
        Self::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("spool_source_too_large"),
        )
    }

    pub const fn spool_corrupted() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("spool_corrupted"),
        )
    }

    pub const fn spool_unsupported_version() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("spool_unsupported_version"),
        )
    }

    pub const fn durable_payload_unsupported_version() -> Self {
        Self::retained_unavailable("host_event_payload_unsupported_version")
    }

    pub const fn durable_payload_malformed() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("host_event_payload_malformed"),
        )
    }

    pub const fn spool_ack_conflict() -> Self {
        Self::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("spool_ack_conflict"),
        )
    }

    pub const fn spool_recovery_required() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("spool_recovery_required"),
        )
    }

    pub const fn quarantine_full() -> Self {
        Self::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("spool_quarantine_full"),
        )
    }

    pub const fn quarantine_corrupted() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("spool_quarantine_corrupted"),
        )
    }

    pub const fn quarantine_recovery_required() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("spool_quarantine_recovery_required"),
        )
    }

    pub const fn project_authority_unbound() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("project_authority_unbound"),
        )
    }

    pub const fn project_authority_mismatch() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("project_authority_mismatch"),
        )
    }

    pub const fn registered_authority_unavailable() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("registered_authority_unavailable"),
        )
    }
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
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    use tracedecay_domain::{CanonicalObservationEnvelopeV1, CanonicalObservationIdV1};
    use tracedecay_runtime_core::privacy::RecordSanitizerV1;
    use tracedecay_store::observation::{
        ObservationAdmissionPort, ObservationCaptureSink, ObservationCursorPort,
        ObservationPersistOutcome, ObservationStoreError, ObservationStoreResult,
    };
    use tracedecay_store::{
        AnchoredObservationWrite, ObservationProjectionStatus, ObservationReplayRequest,
        StoredObservation,
    };

    use crate::observation::{
        AdvanceNonDurableSourceCursorRequest, ObservationApplication, ObservationApplicationError,
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

    #[derive(Default)]
    struct MemoryObservationState {
        observations: Vec<StoredObservation>,
        cursors: Vec<ObservationSourceCursorV1>,
        projected_sequences: Vec<u64>,
        parse_offsets: Vec<(ObservationScopeV1, String, ParseOffset)>,
        capture_failures_remaining: usize,
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

    impl ObservationCaptureSink for MemoryObservationStore {
        async fn persist_admitted_observation(
            &self,
            write: AnchoredObservationWrite,
        ) -> ObservationStoreResult<ObservationPersistOutcome> {
            let mut state = self.state();
            if let Some(stored) = state.observations.iter().find(|stored| {
                stored.observation().observation_id() == write.observation().observation_id()
            }) {
                return Ok(ObservationPersistOutcome::ExactDuplicate(
                    stored.commit_receipt().clone(),
                ));
            }
            let actual = Self::current_cursor(
                &state,
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
            Self::replace_cursor(&mut state, next_cursor);
            Ok(ObservationPersistOutcome::Committed(receipt))
        }
    }

    impl ObservationCursorPort for MemoryObservationStore {
        async fn read_source_cursor(
            &self,
            source: &ObservationSourceIdentityV1,
            scope: &ObservationScopeV1,
        ) -> ObservationStoreResult<Option<ObservationSourceCursorV1>> {
            Ok(Self::current_cursor(&self.state(), source, scope))
        }

        async fn advance_admitted_source_cursor(
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
    }

    impl ObservationAdmissionPort for MemoryObservationStore {
        async fn read_admitted_observation(
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

        async fn replay_admitted_observations(
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
    }

    impl MemoryHostAdmission {
        pub(crate) fn observations(&self) -> Vec<StoredObservation> {
            self.store.state().observations.clone()
        }

        pub(crate) fn fail_next_capture(&self) {
            self.store.state().capture_failures_remaining = 1;
        }

        pub(crate) fn pending_projection_count(&self) -> usize {
            let state = self.store.state();
            state
                .observations
                .iter()
                .filter(|stored| !state.projected_sequences.contains(&stored.sequence()))
                .count()
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

        fn advance_non_durable_source_cursor<'a>(
            &'a self,
            advance: ObservationCursorAdvance,
            cancellation: ObservationCancellation,
        ) -> AdmissionFuture<'a, CursorAdvanceOutcome> {
            Box::pin(async move {
                self.application()?
                    .advance_non_durable_source_cursor(AdvanceNonDurableSourceCursorRequest::new(
                        advance,
                        cancellation,
                    ))
                    .await
                    .map_err(Self::application_error)
            })
        }

        fn get_source_cursor<'a>(
            &'a self,
            source: &'a ObservationSourceIdentityV1,
            scope: &'a ObservationScopeV1,
        ) -> AdmissionFuture<'a, Option<ObservationSourceCursorV1>> {
            Box::pin(async move {
                self.store
                    .read_source_cursor(source, scope)
                    .await
                    .map_err(|_| HostAdmissionOutcome::registered_authority_unavailable())
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
                let mut state = self.store.state();
                let candidates = state
                    .observations
                    .iter()
                    .filter(|stored| {
                        stored.observation().source().provider().as_str() == provider
                            && stored.observation().scope() == scope
                            && !state.projected_sequences.contains(&stored.sequence())
                    })
                    .take(max)
                    .map(StoredObservation::sequence)
                    .collect::<Vec<_>>();
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
    }
}
