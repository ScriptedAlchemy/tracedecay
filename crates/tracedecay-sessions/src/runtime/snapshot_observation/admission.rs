use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use tracedecay_domain::{
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::ObservationPersistOutcome;
use tracedecay_store::observation::{ObservationCoverageReason, ObservationCursorAdvance};

use crate::admission::{HostAdmission, is_admission_cancellation};
use crate::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::runtime::ingest_byte_budget::IngestByteBudget;
use crate::runtime::shared::TranscriptIngestStats;
use crate::runtime::source::{FileDiscoveryReport, TranscriptIngestError, TranscriptIngestResult};

use super::{canonical_snapshot_envelope, host_admission_error};

#[derive(Clone, Debug, Default)]
pub struct SnapshotCaptureOutcome {
    pub stats: TranscriptIngestStats,
    pub bytes_consumed: u64,
    pub deferred_by_byte_cap: bool,
}

/// Provider-specific record material needed by the shared snapshot admission loop.
pub trait SnapshotAdmissionRecord {
    fn provider(&self) -> &'static str;
    fn session_id(&self) -> &str;
    fn native_record_id(&self) -> &str;
    fn order(&self) -> u64;
    fn payload(&self) -> &[u8];
    fn capture_request(
        &self,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        cancellation: ObservationCancellation,
    ) -> TranscriptIngestResult<CaptureObservationRequest> {
        snapshot_capture_request(self, scope, generation, expected_cursor, cancellation)
    }
}

/// Builds the canonical capture request shared by every snapshot provider.
pub fn snapshot_capture_request<R>(
    record: &R,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    cancellation: ObservationCancellation,
) -> TranscriptIngestResult<CaptureObservationRequest>
where
    R: SnapshotAdmissionRecord + ?Sized,
{
    let provider = record.provider();
    let range = ObservationSourceRangeV1::new(record.order(), record.order() + 1)?;
    let parsed = tracedecay_runtime_core::privacy::parse_normalized_observation_record_v1(
        record.payload(),
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        |native| {
            canonical_snapshot_envelope(
                &native,
                provider,
                record.session_id(),
                record.native_record_id(),
                range,
            )
        },
    )
    .map_err(|_| TranscriptIngestError::NonDurableRecord {
        provider,
        offset: range.start(),
        end_offset: range.end(),
        reason: "normalized observation record is not durable",
    })?;
    let source = snapshot_source_identity(provider, record.session_id())?;
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        scope,
        generation,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        ObservationId::new(record.native_record_id())?,
    )?;
    CaptureObservationRequest::new(
        parsed,
        identity,
        expected_cursor,
        RetentionClass::new(format!("transcript.{provider}.v1"))?,
        cancellation,
    )
    .map_err(|_| TranscriptIngestError::InvalidFrameState { provider })
}

/// Post-record snapshot cursor shared by provider capture-request tests.
#[cfg(test)]
pub fn snapshot_cursor_after(
    provider: &'static str,
    session_id: &str,
    order: u64,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
) -> TranscriptIngestResult<ObservationSourceCursorV1> {
    Ok(ObservationSourceCursorV1::for_ordering(
        snapshot_source_identity(provider, session_id)?,
        scope,
        generation,
        ObservationOrderingDomainV1::SnapshotOrder,
        order + 1,
    )?)
}

/// Runs one bounded snapshot sweep for a provider that re-reads complete files.
///
/// Every snapshot provider drives the same loop: bound discovery, defer when
/// discovery truncated, then charge each discovered path against the sweep
/// budget before loading and admitting its records. Providers supply only what
/// actually differs — how paths are discovered, how a path's input bytes are
/// charged, and how a path becomes `(generation, records)`.
///
/// This deliberately re-reads complete snapshots and derives a new source
/// generation from their content; it neither consults nor advances legacy parse
/// offsets. `max_new_bytes` is one logical source-byte budget for the complete
/// sweep.
pub async fn capture_snapshot_observations<R, D, B, L>(
    facade: &dyn HostAdmission,
    provider: &'static str,
    scope: ObservationScopeV1,
    cancellation: &ObservationCancellation,
    max_new_bytes: Option<u64>,
    discover: D,
    input_bytes_fn: B,
    load_fn: L,
) -> TranscriptIngestResult<SnapshotCaptureOutcome>
where
    R: SnapshotAdmissionRecord,
    D: FnOnce() -> FileDiscoveryReport,
    B: Fn(&Path) -> TranscriptIngestResult<u64>,
    L: Fn(&Path) -> TranscriptIngestResult<Option<(ObservationSourceGenerationV1, Vec<R>)>>,
{
    ensure_snapshot_admission_active(provider, cancellation)?;
    let discovery = discover();
    ensure_snapshot_admission_active(provider, cancellation)?;
    let mut runner = SnapshotAdmissionRunner::new(provider, max_new_bytes);
    if discovery.is_truncated() {
        runner.defer();
    }
    for path in discovery.paths {
        ensure_snapshot_admission_active(provider, cancellation)?;
        let input_bytes = input_bytes_fn(&path)?;
        ensure_snapshot_admission_active(provider, cancellation)?;
        runner
            .admit_batch(facade, input_bytes, &scope, cancellation, || load_fn(&path))
            .await?;
    }
    Ok(runner.finish())
}

/// Owns byte accounting and durable admission state for one snapshot-provider sweep.
pub struct SnapshotAdmissionRunner {
    provider: &'static str,
    budget: IngestByteBudget,
    stats: TranscriptIngestStats,
    sessions: BTreeSet<String>,
}

impl SnapshotAdmissionRunner {
    pub fn new(provider: &'static str, max_new_bytes: Option<u64>) -> Self {
        Self {
            provider,
            budget: match max_new_bytes {
                Some(limit) => IngestByteBudget::bounded_allowing_empty(limit),
                None => IngestByteBudget::unbounded(),
            },
            stats: TranscriptIngestStats::default(),
            sessions: BTreeSet::new(),
        }
    }

    pub fn defer(&mut self) {
        self.budget.defer();
    }

    pub async fn admit_batch<R, F>(
        &mut self,
        facade: &dyn HostAdmission,
        input_bytes: u64,
        scope: &ObservationScopeV1,
        cancellation: &ObservationCancellation,
        load: F,
    ) -> TranscriptIngestResult<()>
    where
        R: SnapshotAdmissionRecord,
        F: FnOnce() -> TranscriptIngestResult<Option<(ObservationSourceGenerationV1, Vec<R>)>>,
    {
        ensure_snapshot_admission_active(self.provider, cancellation)?;
        if !self.budget.try_consume(input_bytes) {
            return Ok(());
        }
        ensure_snapshot_admission_active(self.provider, cancellation)?;
        let loaded = load()?;
        ensure_snapshot_admission_active(self.provider, cancellation)?;
        let Some((generation, records)) = loaded else {
            return Ok(());
        };

        let mut cursors: BTreeMap<String, Option<ObservationSourceCursorV1>> = BTreeMap::new();
        let mut pending = Vec::new();
        for record in records {
            let provider = record.provider();
            ensure_snapshot_admission_active(provider, cancellation)?;
            let source_identity = snapshot_source_identity(provider, record.session_id())?;
            let range = ObservationSourceRangeV1::new(record.order(), record.order() + 1)?;
            ensure_snapshot_admission_active(provider, cancellation)?;
            let expected_cursor = session_cursor(
                facade,
                &mut cursors,
                provider,
                record.session_id(),
                &source_identity,
                scope,
                cancellation,
            )
            .await?;
            ensure_snapshot_admission_active(provider, cancellation)?;
            if snapshot_cursor_covers_range(expected_cursor.as_ref(), generation, range) {
                continue;
            }
            pending.push((record, source_identity, range));
        }

        for (record, source_identity, range) in pending {
            let provider = record.provider();
            ensure_snapshot_admission_active(provider, cancellation)?;
            let expected_cursor = session_cursor(
                facade,
                &mut cursors,
                provider,
                record.session_id(),
                &source_identity,
                scope,
                cancellation,
            )
            .await?;
            ensure_snapshot_admission_active(provider, cancellation)?;
            if snapshot_cursor_covers_range(expected_cursor.as_ref(), generation, range) {
                continue;
            }
            ensure_snapshot_admission_active(provider, cancellation)?;
            let request = record.capture_request(
                scope.clone(),
                generation,
                expected_cursor.clone(),
                cancellation.clone(),
            )?;
            ensure_snapshot_admission_active(provider, cancellation)?;
            let outcome = match facade.capture_observation(request).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    if is_admission_cancellation(&error, cancellation) {
                        return Err(TranscriptIngestError::Cancelled { provider });
                    }
                    if cancellation.is_cancelled() {
                        return Err(host_admission_error(provider, error));
                    }
                    let committed = snapshot_range_was_committed(
                        facade,
                        &source_identity,
                        scope,
                        generation,
                        range,
                    )
                    .await;
                    if cancellation.is_cancelled() {
                        return Err(host_admission_error(provider, error));
                    }
                    if committed {
                        cursors.remove(record.session_id());
                        continue;
                    }
                    return Err(host_admission_error(provider, error));
                }
            };
            ensure_snapshot_admission_active(provider, cancellation)?;
            match outcome {
                CaptureObservationOutcome::Persisted { outcome, .. }
                | CaptureObservationOutcome::AcceptedForReplay { outcome, .. } => {
                    if let ObservationPersistOutcome::Committed(receipt) = outcome.as_ref() {
                        self.stats.messages_upserted =
                            self.stats.messages_upserted.saturating_add(1);
                        cursors.insert(
                            record.session_id().to_owned(),
                            Some(receipt.committed_cursor().clone()),
                        );
                    } else {
                        cursors.remove(record.session_id());
                    }
                    self.sessions.insert(record.session_id().to_owned());
                }
                CaptureObservationOutcome::Rejected { receipt, .. } => {
                    ensure_snapshot_admission_active(provider, cancellation)?;
                    advance_snapshot_coverage(
                        facade,
                        provider,
                        source_identity,
                        range,
                        expected_cursor,
                        scope.clone(),
                        generation,
                        ObservationCoverageReason::SanitizerRejected,
                        receipt,
                        cancellation,
                    )
                    .await?;
                    cursors.remove(record.session_id());
                }
                CaptureObservationOutcome::Quarantined { receipt, .. } => {
                    ensure_snapshot_admission_active(provider, cancellation)?;
                    advance_snapshot_coverage(
                        facade,
                        provider,
                        source_identity,
                        range,
                        expected_cursor,
                        scope.clone(),
                        generation,
                        ObservationCoverageReason::SanitizerQuarantined,
                        receipt,
                        cancellation,
                    )
                    .await?;
                    cursors.remove(record.session_id());
                }
            }
        }
        Ok(())
    }

    pub fn finish(mut self) -> SnapshotCaptureOutcome {
        self.stats.sessions_upserted = self.sessions.len() as u64;
        SnapshotCaptureOutcome {
            stats: self.stats,
            bytes_consumed: self.budget.consumed(),
            deferred_by_byte_cap: self.budget.deferred(),
        }
    }
}

fn ensure_snapshot_admission_active(
    provider: &'static str,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<()> {
    if cancellation.is_cancelled() {
        return Err(TranscriptIngestError::Cancelled { provider });
    }
    Ok(())
}

/// Reads a session's durable cursor once per sweep, reusing the committed cursor
/// carried by each capture receipt instead of re-selecting it per record.
async fn session_cursor(
    facade: &dyn HostAdmission,
    cursors: &mut BTreeMap<String, Option<ObservationSourceCursorV1>>,
    provider: &'static str,
    session_id: &str,
    source: &ObservationSourceIdentityV1,
    scope: &ObservationScopeV1,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<Option<ObservationSourceCursorV1>> {
    if let Some(cursor) = cursors.get(session_id) {
        return Ok(cursor.clone());
    }
    let cursor = facade
        .get_source_cursor(source, scope)
        .await
        .map_err(|outcome| {
            if is_admission_cancellation(&outcome, cancellation) {
                TranscriptIngestError::Cancelled { provider }
            } else {
                host_admission_error(provider, outcome)
            }
        })?;
    ensure_snapshot_admission_active(provider, cancellation)?;
    cursors.insert(session_id.to_owned(), cursor.clone());
    Ok(cursor)
}

pub fn snapshot_source_identity(
    provider: &'static str,
    session_id: &str,
) -> TranscriptIngestResult<ObservationSourceIdentityV1> {
    Ok(ObservationSourceIdentityV1::for_provider(
        ProviderId::new(provider)?,
        SessionId::new(session_id.to_string())?,
    )?)
}

#[allow(clippy::too_many_arguments)]
pub async fn advance_snapshot_coverage(
    facade: &dyn HostAdmission,
    provider: &'static str,
    source: ObservationSourceIdentityV1,
    range: ObservationSourceRangeV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    reason: ObservationCoverageReason,
    receipt: tracedecay_domain::SanitizationReceiptV1,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<()> {
    advance_snapshot_coverage_maybe(
        facade,
        provider,
        source,
        range,
        expected_cursor,
        scope,
        generation,
        reason,
        Some(receipt),
        cancellation,
    )
    .await
}

/// [`advance_snapshot_coverage`] for coverage transitions whose sanitization
/// receipt is optional (structural covers carry no receipt; sanitizer covers do).
#[allow(clippy::too_many_arguments)]
pub async fn advance_snapshot_coverage_maybe(
    facade: &dyn HostAdmission,
    provider: &'static str,
    source: ObservationSourceIdentityV1,
    range: ObservationSourceRangeV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    reason: ObservationCoverageReason,
    receipt: Option<tracedecay_domain::SanitizationReceiptV1>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<()> {
    let advance = match receipt {
        Some(receipt) => ObservationCursorAdvance::for_ordering_with_sanitization_receipt(
            source,
            scope,
            generation,
            ObservationOrderingDomainV1::SnapshotOrder,
            expected_cursor,
            range,
            reason,
            receipt,
        ),
        None => ObservationCursorAdvance::for_ordering(
            source,
            scope,
            generation,
            ObservationOrderingDomainV1::SnapshotOrder,
            expected_cursor,
            range,
            reason,
        ),
    }
    .map_err(|_| TranscriptIngestError::InvalidFrameState { provider })?;
    facade
        .advance_non_durable_source_cursor(advance, cancellation.clone())
        .await
        .map(|_| ())
        .map_err(|outcome| {
            if is_admission_cancellation(&outcome, cancellation) {
                TranscriptIngestError::Cancelled { provider }
            } else {
                host_admission_error(provider, outcome)
            }
        })?;
    ensure_snapshot_admission_active(provider, cancellation)
}

pub async fn snapshot_range_was_committed(
    facade: &dyn HostAdmission,
    source: &ObservationSourceIdentityV1,
    scope: &ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    range: ObservationSourceRangeV1,
) -> bool {
    // Recovery-only probe: the caller preserves the original capture error if
    // the authority cannot confirm that the cursor advanced.
    let cursor = facade.get_source_cursor(source, scope).await.ok().flatten();
    snapshot_cursor_covers_range(cursor.as_ref(), generation, range)
}

pub fn snapshot_cursor_covers_range(
    cursor: Option<&ObservationSourceCursorV1>,
    generation: ObservationSourceGenerationV1,
    range: ObservationSourceRangeV1,
) -> bool {
    cursor
        .is_some_and(|cursor| cursor.generation() == generation && cursor.position() >= range.end())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::admission::HostAdmission;
    use crate::admission::test_support::{MemoryHostAdmission, PanicHostAdmission};

    use super::*;

    #[derive(Clone)]
    struct TestSnapshotRecord {
        session_id: String,
        native_record_id: String,
        payload: Vec<u8>,
    }

    impl SnapshotAdmissionRecord for TestSnapshotRecord {
        fn provider(&self) -> &'static str {
            "test"
        }

        fn session_id(&self) -> &str {
            &self.session_id
        }

        fn native_record_id(&self) -> &str {
            &self.native_record_id
        }

        fn order(&self) -> u64 {
            0
        }

        fn payload(&self) -> &[u8] {
            &self.payload
        }
    }

    fn test_record() -> TestSnapshotRecord {
        TestSnapshotRecord {
            session_id: "session-1".to_owned(),
            native_record_id: "message-1".to_owned(),
            payload: br#"{
                "provider": "test",
                "session_id": "session-1",
                "message_id": "message-1",
                "role": "user",
                "ordinal": 0,
                "text": "retry me"
            }"#
            .to_vec(),
        }
    }

    fn discovery(paths: Vec<PathBuf>) -> FileDiscoveryReport {
        FileDiscoveryReport {
            paths,
            truncated: None,
            skipped_oversized_entries: 0,
            bytes_charged: 0,
        }
    }

    #[tokio::test]
    async fn pre_cancelled_snapshot_sweep_returns_control_error_before_admission() {
        let cancellation = ObservationCancellation::default();
        cancellation.cancel();

        let error = capture_snapshot_observations::<TestSnapshotRecord, _, _, _>(
            &PanicHostAdmission,
            "test",
            ObservationScopeV1::Profile,
            &cancellation,
            None,
            || discovery(Vec::new()),
            |_| Ok(0),
            |_| Ok(None),
        )
        .await
        .expect_err("pre-cancellation must terminate the sweep");

        assert!(matches!(
            error,
            TranscriptIngestError::Cancelled { provider: "test" }
        ));
    }

    #[tokio::test]
    async fn mid_sweep_cancellation_advances_no_coverage_and_retry_commits() {
        let admission = MemoryHostAdmission::default();
        let cancellation = ObservationCancellation::default();
        admission.cancel_on_next_cursor_read(cancellation.clone());
        let path = PathBuf::from("session-1.snapshot");
        let generation = ObservationSourceGenerationV1::new(1).unwrap();

        let error = capture_snapshot_observations(
            &admission,
            "test",
            ObservationScopeV1::Profile,
            &cancellation,
            None,
            || discovery(vec![path.clone()]),
            |_| Ok(1),
            |_| Ok(Some((generation, vec![test_record()]))),
        )
        .await
        .expect_err("mid-sweep cancellation must terminate the sweep");

        assert!(matches!(
            error,
            TranscriptIngestError::Cancelled { provider: "test" }
        ));
        assert!(admission.observations().is_empty());
        let source = snapshot_source_identity("test", "session-1").unwrap();
        assert!(
            admission
                .get_source_cursor(&source, &ObservationScopeV1::Profile)
                .await
                .unwrap()
                .is_none(),
            "cancellation must not advance source coverage"
        );
        assert!(
            admission
                .get_parse_offset(&ObservationScopeV1::Profile, "host-coverage://test/v1",)
                .await
                .unwrap()
                .is_none(),
            "cancellation must not publish provider coverage"
        );

        let retry = capture_snapshot_observations(
            &admission,
            "test",
            ObservationScopeV1::Profile,
            &ObservationCancellation::default(),
            None,
            || discovery(vec![path]),
            |_| Ok(1),
            |_| Ok(Some((generation, vec![test_record()]))),
        )
        .await
        .expect("retry after cancellation must succeed");

        assert_eq!(retry.stats.messages_upserted, 1);
        assert_eq!(admission.observations().len(), 1);
    }

    #[test]
    fn snapshot_budget_is_aggregate_and_reports_deferral() {
        let mut runner = SnapshotAdmissionRunner::new("test", Some(5));
        assert!(runner.budget.try_consume(3));
        assert!(!runner.budget.try_consume(3));
        assert!(runner.budget.try_consume(2));
        runner.sessions.insert("session-1".to_owned());

        let outcome = runner.finish();
        assert_eq!(outcome.bytes_consumed, 5);
        assert!(outcome.deferred_by_byte_cap);
        assert_eq!(outcome.stats.sessions_upserted, 1);
    }

    #[test]
    fn unbounded_snapshot_budget_still_reports_consumed_bytes() {
        let mut runner = SnapshotAdmissionRunner::new("test", None);
        assert!(runner.budget.try_consume(7));
        assert!(runner.budget.try_consume(11));
        let outcome = runner.finish();
        assert_eq!(outcome.bytes_consumed, 18);
        assert!(!outcome.deferred_by_byte_cap);
    }
}
