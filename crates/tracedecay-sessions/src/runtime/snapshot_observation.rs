use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::Path;

use serde_json::{Map, Value};
use tracedecay_domain::{
    CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CanonicalReasoningVisibilityV1, CanonicalWorkflowEvidenceKindV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::ObservationPersistOutcome;
use tracedecay_store::observation::{ObservationCoverageReason, ObservationCursorAdvance};

use crate::application::host_admission::{
    HostAdmissionFacade, HostAdmissionOutcome, HostAdmissionStatus, WireReadOutcome,
    read_bounded_to_string,
};
use crate::application::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use tracedecay_runtime_core::privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use crate::runtime::SessionMessageRecord;
use crate::runtime::ingest_byte_budget::IngestByteBudget;
use crate::runtime::shared::TranscriptIngestStats;
use crate::runtime::source::{
    FileDiscoveryReport, TranscriptIngestError, TranscriptIngestResult, canonical_framed_sha256,
};

pub const MAX_SNAPSHOT_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_SNAPSHOT_METADATA_BYTES: u64 = 256 * 1024;
/// Largest atomic snapshot task admitted by the shared scheduler.
///
/// Cline-family tasks may read one API transcript, one UI companion, and all
/// three metadata candidates before finding the first valid document.
pub const MAX_SNAPSHOT_CAPTURE_UNIT_BYTES: u64 =
    (2 * MAX_SNAPSHOT_FILE_BYTES) + (3 * MAX_SNAPSHOT_METADATA_BYTES);

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
    let parsed = parse_normalized_observation_record_v1(
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

/// Provider-specific domain separators for [`stable_snapshot_message_id`].
#[derive(Clone, Copy)]
pub struct StableMessageIdDomains {
    pub delimited_domain: &'static [u8],
    pub delimited_prefix: &'static str,
    pub derived_domain: &'static [u8],
    pub derived_prefix: &'static str,
}

/// Two-tier snapshot message identity: native fast path with a collision-safe
/// delimited fallback, otherwise a derived digest over the provider frames.
pub fn stable_snapshot_message_id(
    domains: StableMessageIdDomains,
    id: &str,
    native_id: Option<&str>,
    derived_frames: &[&[u8]],
) -> String {
    if let Some(native_id) = native_id {
        if !id.contains(':') && !native_id.contains(':') {
            return format!("{id}:{native_id}");
        }
        let digest = canonical_framed_sha256(
            domains.delimited_domain,
            &[id.as_bytes(), native_id.as_bytes()],
        );
        return format!("{}{digest}", domains.delimited_prefix);
    }
    let digest = canonical_framed_sha256(domains.derived_domain, derived_frames);
    format!("{}{digest}", domains.derived_prefix)
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
/// actually differs — the discovery report, how a path's input bytes are
/// charged, and how a path becomes `(generation, records)`.
///
/// This deliberately re-reads complete snapshots and derives a new source
/// generation from their content; it neither consults nor advances legacy parse
/// offsets. `max_new_bytes` is one logical source-byte budget for the complete
/// sweep.
pub async fn capture_snapshot_observations<R, B, L>(
    facade: &HostAdmissionFacade<'_>,
    scope: ObservationScopeV1,
    cancellation: &ObservationCancellation,
    max_new_bytes: Option<u64>,
    discovery: FileDiscoveryReport,
    input_bytes_fn: B,
    load_fn: L,
) -> TranscriptIngestResult<SnapshotCaptureOutcome>
where
    R: SnapshotAdmissionRecord,
    B: Fn(&Path) -> TranscriptIngestResult<u64>,
    L: Fn(&Path) -> TranscriptIngestResult<Option<(ObservationSourceGenerationV1, Vec<R>)>>,
{
    let mut runner = SnapshotAdmissionRunner::new(max_new_bytes);
    if discovery.is_truncated() {
        runner.defer();
    }
    for path in discovery.paths {
        let input_bytes = input_bytes_fn(&path)?;
        runner
            .admit_batch(facade, input_bytes, &scope, cancellation, || load_fn(&path))
            .await?;
    }
    Ok(runner.finish())
}

/// Owns byte accounting and durable admission state for one snapshot-provider sweep.
pub struct SnapshotAdmissionRunner {
    budget: IngestByteBudget,
    stats: TranscriptIngestStats,
    sessions: BTreeSet<String>,
}

impl SnapshotAdmissionRunner {
    pub fn new(max_new_bytes: Option<u64>) -> Self {
        Self {
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
        facade: &HostAdmissionFacade<'_>,
        input_bytes: u64,
        scope: &ObservationScopeV1,
        cancellation: &ObservationCancellation,
        load: F,
    ) -> TranscriptIngestResult<()>
    where
        R: SnapshotAdmissionRecord,
        F: FnOnce() -> TranscriptIngestResult<Option<(ObservationSourceGenerationV1, Vec<R>)>>,
    {
        if !self.budget.try_consume(input_bytes) {
            return Ok(());
        }
        let Some((generation, records)) = load()? else {
            return Ok(());
        };

        let mut cursors: BTreeMap<String, Option<ObservationSourceCursorV1>> = BTreeMap::new();
        let mut pending = Vec::new();
        for record in records {
            let provider = record.provider();
            let source_identity = snapshot_source_identity(provider, record.session_id())?;
            let range = ObservationSourceRangeV1::new(record.order(), record.order() + 1)?;
            let expected_cursor = session_cursor(
                facade,
                &mut cursors,
                provider,
                record.session_id(),
                &source_identity,
                scope,
            )
            .await?;
            if snapshot_cursor_covers_range(expected_cursor.as_ref(), generation, range) {
                continue;
            }
            pending.push((record, source_identity, range));
        }

        for (record, source_identity, range) in pending {
            let provider = record.provider();
            let expected_cursor = session_cursor(
                facade,
                &mut cursors,
                provider,
                record.session_id(),
                &source_identity,
                scope,
            )
            .await?;
            if snapshot_cursor_covers_range(expected_cursor.as_ref(), generation, range) {
                continue;
            }
            let request = record.capture_request(
                scope.clone(),
                generation,
                expected_cursor.clone(),
                cancellation.clone(),
            )?;
            let outcome = match facade.capture_observation(request).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    if snapshot_range_was_committed(
                        facade,
                        &source_identity,
                        scope,
                        generation,
                        range,
                    )
                    .await
                    {
                        cursors.remove(record.session_id());
                        continue;
                    }
                    return Err(host_admission_error(provider, error));
                }
            };
            match outcome {
                CaptureObservationOutcome::Persisted { outcome, .. } => {
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

/// Reads a session's durable cursor once per sweep, reusing the committed cursor
/// carried by each capture receipt instead of re-selecting it per record.
async fn session_cursor(
    facade: &HostAdmissionFacade<'_>,
    cursors: &mut BTreeMap<String, Option<ObservationSourceCursorV1>>,
    provider: &'static str,
    session_id: &str,
    source: &ObservationSourceIdentityV1,
    scope: &ObservationScopeV1,
) -> TranscriptIngestResult<Option<ObservationSourceCursorV1>> {
    if let Some(cursor) = cursors.get(session_id) {
        return Ok(cursor.clone());
    }
    let cursor = facade
        .get_source_cursor(source, scope)
        .await
        .map_err(|outcome| host_admission_error(provider, outcome))?;
    cursors.insert(session_id.to_owned(), cursor.clone());
    Ok(cursor)
}

pub fn non_durable_snapshot_record(
    provider: &'static str,
    path: &Path,
    reason: &'static str,
) -> TranscriptIngestError {
    let end_offset = std::fs::metadata(path).map_or(0, |metadata| metadata.len());
    TranscriptIngestError::NonDurableRecord {
        provider,
        offset: 0,
        end_offset,
        reason,
    }
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

pub fn bounded_snapshot_input_len(
    provider: &'static str,
    path: &Path,
    byte_cap: u64,
) -> TranscriptIngestResult<u64> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(0);
    };
    if metadata.len() > byte_cap {
        return Err(TranscriptIngestError::NonDurableRecord {
            provider,
            offset: 0,
            end_offset: metadata.len(),
            reason: "snapshot input exceeds provider byte bound",
        });
    }
    Ok(metadata.len())
}

pub fn read_snapshot_text_bounded(
    provider: &'static str,
    path: &Path,
    byte_cap: u64,
) -> TranscriptIngestResult<Option<String>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(TranscriptIngestError::ScanIo {
                operation: "open",
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let max_bytes = usize::try_from(byte_cap).unwrap_or(usize::MAX);
    match read_bounded_to_string(&mut file, max_bytes).map_err(|source| {
        TranscriptIngestError::ScanIo {
            operation: "read",
            path: path.to_path_buf(),
            source,
        }
    })? {
        WireReadOutcome::Ready(text) => Ok(Some(text)),
        WireReadOutcome::Oversized => Err(TranscriptIngestError::NonDurableRecord {
            provider,
            offset: 0,
            end_offset: byte_cap.saturating_add(1),
            reason: "snapshot metadata exceeds provider byte bound",
        }),
    }
}

pub fn host_admission_error(
    provider: &'static str,
    outcome: HostAdmissionOutcome,
) -> TranscriptIngestError {
    let reason = outcome.reason_code.unwrap_or(match outcome.status {
        HostAdmissionStatus::Backpressured => "observation_admission_backpressured",
        HostAdmissionStatus::Unavailable => "observation_authority_unavailable",
        HostAdmissionStatus::Unknown => "observation_provider_unsupported",
        HostAdmissionStatus::Degraded => "observation_admission_degraded",
        HostAdmissionStatus::Supported
        | HostAdmissionStatus::AcceptedForReplay
        | HostAdmissionStatus::Committed
        | HostAdmissionStatus::ExactDuplicate => "observation_admission_incomplete",
    });
    TranscriptIngestError::NonDurableRecord {
        provider,
        offset: 0,
        end_offset: 0,
        reason,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn advance_snapshot_coverage(
    facade: &HostAdmissionFacade<'_>,
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
    facade: &HostAdmissionFacade<'_>,
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
        .map_err(|outcome| host_admission_error(provider, outcome))
}

/// Human-readable host-admission failure message shared by the SQLite-backed
/// snapshot providers, prefixed with the caller's provider label.
pub fn host_admission_status_message(
    provider_label: &str,
    status: HostAdmissionStatus,
) -> String {
    match status {
        HostAdmissionStatus::Backpressured => {
            format!("{provider_label} observation admission was backpressured")
        }
        HostAdmissionStatus::Unavailable => {
            format!("{provider_label} observation authority is unavailable")
        }
        HostAdmissionStatus::Unknown => {
            format!("{provider_label} observation provider is unsupported")
        }
        HostAdmissionStatus::Degraded => {
            format!("{provider_label} observation admission was degraded")
        }
        HostAdmissionStatus::Supported
        | HostAdmissionStatus::AcceptedForReplay
        | HostAdmissionStatus::Committed
        | HostAdmissionStatus::ExactDuplicate => {
            format!("{provider_label} observation admission was incomplete")
        }
    }
}

pub fn snapshot_message_fields(
    provider: &str,
    message: &SessionMessageRecord,
) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert("provider".to_string(), Value::String(provider.to_string()));
    fields.insert(
        "session_id".to_string(),
        Value::String(message.session_id.clone()),
    );
    fields.insert(
        "message_id".to_string(),
        Value::String(message.message_id.clone()),
    );
    fields.insert("role".to_string(), Value::String(message.role.clone()));
    fields.insert("ordinal".to_string(), Value::from(message.ordinal));
    fields.insert("text".to_string(), Value::String(message.text.clone()));
    if let Some(timestamp) = message.timestamp {
        fields.insert("timestamp".to_string(), Value::from(timestamp));
    }
    if let Some(kind) = &message.kind {
        fields.insert("kind".to_string(), Value::String(kind.clone()));
    }
    if let Some(model) = &message.model {
        fields.insert("model".to_string(), Value::String(model.clone()));
    }
    fields
}

pub async fn snapshot_range_was_committed(
    facade: &HostAdmissionFacade<'_>,
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

pub fn canonical_snapshot_envelope(
    native: &Value,
    provider: &str,
    session_id: &str,
    message_id: &str,
    range: ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let invalid = || ObservationRecordParseErrorV1::NormalizationFailed;
    let role = match native.get("role").and_then(Value::as_str) {
        Some("user") => CanonicalMessageRoleV1::User,
        Some("assistant") => CanonicalMessageRoleV1::Assistant,
        Some("system") => CanonicalMessageRoleV1::System,
        Some("tool") => CanonicalMessageRoleV1::Tool,
        _ => CanonicalMessageRoleV1::Unknown,
    };
    let timestamp = native.get("timestamp").and_then(Value::as_i64);
    let mut facts = Vec::new();
    if let Some(text) = native.get("text").cloned() {
        facts.push(CanonicalObservationFactV1::Message {
            role,
            content: text,
            model: native
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            timestamp,
        });
    }
    append_tool_invocation_facts(&mut facts, native, message_id)?;
    if let Some(result) = native.get("tool_result").filter(|value| value.is_object()) {
        facts.push(CanonicalObservationFactV1::ToolResult {
            invocation_id: optional_observation_id(result, "invocation_id")?,
            content: result.get("content").cloned().unwrap_or(Value::Null),
            success: result.get("success").and_then(Value::as_bool),
        });
    }
    if let Some(usage) = native.get("usage").filter(|value| value.is_object()) {
        facts.push(CanonicalObservationFactV1::Usage {
            input_tokens: usage.get("input_tokens").and_then(Value::as_u64),
            output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
            cache_read_tokens: usage.get("cache_read_input_tokens").and_then(Value::as_u64),
            cache_write_tokens: usage
                .get("cache_creation_input_tokens")
                .and_then(Value::as_u64),
            reasoning_tokens: usage.get("reasoning_tokens").and_then(Value::as_u64),
        });
    }
    append_reasoning_fact(&mut facts, native)?;
    append_typed_git_fact(&mut facts, native);
    append_typed_workflow_fact(&mut facts, native);

    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range);
    if let Some(sequence) = native.get("ordinal").and_then(Value::as_u64) {
        evidence = evidence.with_native_sequence(sequence);
    }
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }

    let message_observation_id = ObservationId::new(message_id).map_err(|_| invalid())?;
    let mut relations =
        CanonicalObservationRelationsV1::new(SessionId::new(session_id).map_err(|_| invalid())?)
            .with_message_id(message_observation_id.clone());
    relations = apply_optional_relation(
        relations,
        native,
        "thread_id",
        CanonicalObservationRelationsV1::with_thread_id,
    )?;
    relations = apply_optional_relation(
        relations,
        native,
        "turn_id",
        CanonicalObservationRelationsV1::with_turn_id,
    )?;
    relations = apply_optional_relation(
        relations,
        native,
        "parent_message_id",
        CanonicalObservationRelationsV1::with_parent_message_id,
    )?;
    relations = apply_optional_relation(
        relations,
        native,
        "agent_id",
        CanonicalObservationRelationsV1::with_agent_id,
    )?;
    relations = apply_optional_relation(
        relations,
        native,
        "parent_agent_id",
        CanonicalObservationRelationsV1::with_parent_agent_id,
    )?;

    CanonicalObservationEnvelopeV1::new(
        ProviderId::new(provider).map_err(|_| invalid())?,
        native
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("message"),
        message_observation_id,
        relations,
        facts,
        evidence,
    )
    .map_err(|_| invalid())
}

fn optional_observation_id(
    value: &Value,
    key: &str,
) -> Result<Option<ObservationId>, ObservationRecordParseErrorV1> {
    let Some(raw) = value.get(key).and_then(Value::as_str) else {
        return Ok(None);
    };
    ObservationId::new(raw)
        .map(Some)
        .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
}

fn apply_optional_relation(
    relations: CanonicalObservationRelationsV1,
    native: &Value,
    key: &str,
    apply: fn(CanonicalObservationRelationsV1, ObservationId) -> CanonicalObservationRelationsV1,
) -> Result<CanonicalObservationRelationsV1, ObservationRecordParseErrorV1> {
    match optional_observation_id(native, key)? {
        Some(id) => Ok(apply(relations, id)),
        None => Ok(relations),
    }
}

fn append_tool_invocation_facts(
    facts: &mut Vec<CanonicalObservationFactV1>,
    native: &Value,
    message_id: &str,
) -> Result<(), ObservationRecordParseErrorV1> {
    if let Some(calls) = native
        .get("tool_calls")
        .or_else(|| native.get("tool_invocations"))
        .and_then(Value::as_array)
    {
        for (index, call) in calls.iter().enumerate() {
            let Some(name) = call
                .pointer("/function/name")
                .or_else(|| call.get("name"))
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
            let arguments = call
                .pointer("/function/arguments")
                .or_else(|| call.get("arguments"))
                .cloned()
                .unwrap_or(Value::Null);
            let arguments = match arguments {
                Value::String(raw) => serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
                value => value,
            };
            let invocation_id = call
                .get("id")
                .or_else(|| call.get("invocation_id"))
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map_or_else(|| format!("{message_id}:tool:{index}"), str::to_owned);
            facts.push(CanonicalObservationFactV1::ToolInvocation {
                invocation_id: ObservationId::new(invocation_id)
                    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
                name: name.to_string(),
                arguments,
            });
        }
        return Ok(());
    }

    // Snapshot providers derive `tool_names` from native exposed tool-call records.
    // This fallback preserves that evidence without inventing arguments; payload
    // shapers must supply `tool_calls` when typed arguments are available.
    for (index, name) in native
        .get("tool_names")
        .and_then(Value::as_str)
        .into_iter()
        .flat_map(|names| names.split(',').filter(|name| !name.is_empty()))
        .enumerate()
    {
        facts.push(CanonicalObservationFactV1::ToolInvocation {
            invocation_id: ObservationId::new(format!("{message_id}:tool:{index}"))
                .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
            name: name.to_string(),
            arguments: Value::Null,
        });
    }
    Ok(())
}

fn append_reasoning_fact(
    facts: &mut Vec<CanonicalObservationFactV1>,
    native: &Value,
) -> Result<(), ObservationRecordParseErrorV1> {
    let content = native
        .get("reasoning")
        .filter(|value| !value.is_null())
        .cloned();
    let visibility = match native.get("reasoning_visibility").and_then(Value::as_str) {
        Some(raw) => Some(
            parse_reasoning_visibility(raw)
                .map_err(|()| ObservationRecordParseErrorV1::NormalizationFailed)?,
        ),
        None => None,
    };

    match (visibility, content) {
        (Some(CanonicalReasoningVisibilityV1::Visible), Some(content)) => {
            facts.push(CanonicalObservationFactV1::Reasoning {
                visibility: CanonicalReasoningVisibilityV1::Visible,
                content: Some(content),
            });
        }
        (Some(CanonicalReasoningVisibilityV1::Visible), None) => {
            return Err(ObservationRecordParseErrorV1::NormalizationFailed);
        }
        (Some(visibility), _) => {
            // Non-visible states never carry content; ignore any bag content.
            facts.push(CanonicalObservationFactV1::Reasoning {
                visibility,
                content: None,
            });
        }
        // Content without provider visibility may be a protocol or metadata echo.
        // Leave it unset rather than inventing exposed reasoning semantics.
        (None, _) => {}
    }
    Ok(())
}

fn parse_reasoning_visibility(raw: &str) -> Result<CanonicalReasoningVisibilityV1, ()> {
    match raw {
        "visible" => Ok(CanonicalReasoningVisibilityV1::Visible),
        "redacted" => Ok(CanonicalReasoningVisibilityV1::Redacted),
        "unavailable" => Ok(CanonicalReasoningVisibilityV1::Unavailable),
        "not_applicable" => Ok(CanonicalReasoningVisibilityV1::NotApplicable),
        _ => Err(()),
    }
}

fn append_typed_git_fact(facts: &mut Vec<CanonicalObservationFactV1>, native: &Value) {
    let Some(git) = native.get("git").filter(|value| !value.is_null()) else {
        return;
    };
    let Some(evidence_kind) = typed_git_evidence_kind(git) else {
        // Untyped bags stay unset rather than inventing Unknown semantics.
        return;
    };
    facts.push(CanonicalObservationFactV1::Git {
        evidence_kind,
        reference: git
            .get("reference")
            .and_then(Value::as_str)
            .map(str::to_string),
        content: git.get("content").cloned(),
    });
}

fn typed_git_evidence_kind(git: &Value) -> Option<CanonicalGitEvidenceKindV1> {
    match git.get("evidence_kind").and_then(Value::as_str)? {
        "diff" => Some(CanonicalGitEvidenceKindV1::Diff),
        "file_edit" => Some(CanonicalGitEvidenceKindV1::FileEdit),
        "commit" => Some(CanonicalGitEvidenceKindV1::Commit),
        "branch" => Some(CanonicalGitEvidenceKindV1::Branch),
        "pull_request" => Some(CanonicalGitEvidenceKindV1::PullRequest),
        "unknown" => Some(CanonicalGitEvidenceKindV1::Unknown),
        _ => None,
    }
}

fn append_typed_workflow_fact(facts: &mut Vec<CanonicalObservationFactV1>, native: &Value) {
    let Some(workflow) = native.get("workflow").filter(|value| !value.is_null()) else {
        return;
    };
    let Some(evidence_kind) = typed_workflow_evidence_kind(workflow) else {
        return;
    };
    facts.push(CanonicalObservationFactV1::Workflow {
        evidence_kind,
        reference: workflow
            .get("reference")
            .and_then(Value::as_str)
            .map(str::to_string),
        content: workflow.get("content").cloned(),
    });
}

fn typed_workflow_evidence_kind(workflow: &Value) -> Option<CanonicalWorkflowEvidenceKindV1> {
    match workflow.get("evidence_kind").and_then(Value::as_str)? {
        "plan" => Some(CanonicalWorkflowEvidenceKindV1::Plan),
        "task" => Some(CanonicalWorkflowEvidenceKindV1::Task),
        "subagent" => Some(CanonicalWorkflowEvidenceKindV1::Subagent),
        "model_fallback" => Some(CanonicalWorkflowEvidenceKindV1::ModelFallback),
        "attribution" => Some(CanonicalWorkflowEvidenceKindV1::Attribution),
        "pull_request" => Some(CanonicalWorkflowEvidenceKindV1::PullRequest),
        "unknown" => Some(CanonicalWorkflowEvidenceKindV1::Unknown),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn range(start: u64, end: u64) -> ObservationSourceRangeV1 {
        ObservationSourceRangeV1::new(start, end).expect("valid range")
    }

    fn envelope(native: &Value) -> CanonicalObservationEnvelopeV1 {
        canonical_snapshot_envelope(native, "kiro", "session-1", "message-1", range(0, 1))
            .expect("canonical envelope")
    }

    #[test]
    fn snapshot_budget_is_aggregate_and_reports_deferral() {
        let mut runner = SnapshotAdmissionRunner::new(Some(5));
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
    fn oversized_metadata_is_rejected_without_materializing_the_sparse_payload() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("meta.json");
        let file = File::create(&path).unwrap();
        file.set_len(MAX_SNAPSHOT_METADATA_BYTES + 1).unwrap();

        let error = read_snapshot_text_bounded("test", &path, MAX_SNAPSHOT_METADATA_BYTES)
            .expect_err("sparse oversized metadata must be non-durable");
        assert!(matches!(
            error,
            TranscriptIngestError::NonDurableRecord {
                reason: "snapshot metadata exceeds provider byte bound",
                ..
            }
        ));
    }

    #[test]
    fn unbounded_snapshot_budget_still_reports_consumed_bytes() {
        let mut runner = SnapshotAdmissionRunner::new(None);
        assert!(runner.budget.try_consume(7));
        assert!(runner.budget.try_consume(11));
        let outcome = runner.finish();
        assert_eq!(outcome.bytes_consumed, 18);
        assert!(!outcome.deferred_by_byte_cap);
    }

    #[test]
    fn absent_native_facts_stay_unset() {
        let canonical = envelope(&json!({
            "role": "assistant",
            "text": "hello",
            "kind": "message",
        }));
        assert_eq!(canonical.facts().len(), 1);
        assert!(matches!(
            &canonical.facts()[0],
            CanonicalObservationFactV1::Message { .. }
        ));
        assert!(!canonical.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Reasoning { .. }
                | CanonicalObservationFactV1::Git { .. }
                | CanonicalObservationFactV1::Workflow { .. }
                | CanonicalObservationFactV1::ToolInvocation { .. }
        )));
        let encoded = serde_json::to_value(&canonical).expect("serialize");
        assert!(encoded["relations"].get("thread_id").is_none());
        assert!(encoded["relations"].get("turn_id").is_none());
        assert!(encoded["relations"].get("agent_id").is_none());
        assert!(encoded["relations"].get("parent_message_id").is_none());
        assert!(encoded["relations"].get("parent_agent_id").is_none());
    }

    #[test]
    fn untyped_git_and_workflow_bags_are_not_invented() {
        let canonical = envelope(&json!({
            "role": "assistant",
            "text": "hello",
            "git": {"commit": "abc"},
            "workflow": {"task": "todo"},
        }));
        assert_eq!(canonical.facts().len(), 1);
        assert!(!canonical.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Git { .. } | CanonicalObservationFactV1::Workflow { .. }
        )));
    }

    #[test]
    fn explicit_reasoning_visibility_states_are_preserved() {
        let cases = [
            (
                "visible",
                CanonicalReasoningVisibilityV1::Visible,
                Some(json!("exposed")),
            ),
            ("redacted", CanonicalReasoningVisibilityV1::Redacted, None),
            (
                "unavailable",
                CanonicalReasoningVisibilityV1::Unavailable,
                None,
            ),
            (
                "not_applicable",
                CanonicalReasoningVisibilityV1::NotApplicable,
                None,
            ),
        ];
        for (visibility, expected, content) in cases {
            let mut native = json!({
                "role": "assistant",
                "text": "hello",
                "reasoning_visibility": visibility,
            });
            if let Some(content) = content {
                native["reasoning"] = content;
            }
            let canonical = envelope(&native);
            assert!(canonical.facts().iter().any(|fact| match fact {
                CanonicalObservationFactV1::Reasoning {
                    visibility,
                    content,
                } => {
                    *visibility == expected
                        && if expected == CanonicalReasoningVisibilityV1::Visible {
                            content.as_ref() == Some(&json!("exposed"))
                        } else {
                            content.is_none()
                        }
                }
                _ => false,
            }));
        }
    }

    #[test]
    fn reasoning_content_without_visibility_is_not_emitted() {
        let canonical = envelope(&json!({
            "role": "assistant",
            "text": "hello",
            "reasoning": "must-never-leak",
        }));
        assert!(
            !canonical
                .facts()
                .iter()
                .any(|fact| matches!(fact, CanonicalObservationFactV1::Reasoning { .. }))
        );
        let encoded = serde_json::to_string(&canonical).expect("serialize");
        assert!(!encoded.contains("must-never-leak"));
        assert!(!encoded.contains("\"kind\":\"reasoning\""));
    }

    #[test]
    fn relations_and_tool_arguments_populate_from_typed_native_fields() {
        let canonical = envelope(&json!({
            "role": "assistant",
            "text": "hello",
            "thread_id": "thread-1",
            "turn_id": "turn-1",
            "parent_message_id": "parent-message-1",
            "agent_id": "agent-1",
            "parent_agent_id": "parent-agent-1",
            "tool_calls": [{
                "id": "call-1",
                "name": "read_file",
                "arguments": {"path": "src/main.rs"}
            }],
            "git": {
                "evidence_kind": "commit",
                "reference": "abc123"
            },
            "workflow": {
                "evidence_kind": "task",
                "reference": "task-9"
            },
        }));
        let encoded = serde_json::to_value(&canonical).expect("serialize");
        assert_eq!(encoded["relations"]["thread_id"], "thread-1");
        assert_eq!(encoded["relations"]["turn_id"], "turn-1");
        assert_eq!(
            encoded["relations"]["parent_message_id"],
            "parent-message-1"
        );
        assert_eq!(encoded["relations"]["agent_id"], "agent-1");
        assert_eq!(encoded["relations"]["parent_agent_id"], "parent-agent-1");
        assert!(canonical.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::ToolInvocation {
                invocation_id,
                name,
                arguments,
            } if invocation_id.as_str() == "call-1"
                && name == "read_file"
                && arguments.get("path") == Some(&json!("src/main.rs"))
        )));
        assert!(canonical.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Git {
                evidence_kind: CanonicalGitEvidenceKindV1::Commit,
                reference: Some(reference),
                ..
            } if reference == "abc123"
        )));
        assert!(canonical.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::Task,
                reference: Some(reference),
                ..
            } if reference == "task-9"
        )));
    }

    #[test]
    fn tool_names_without_arguments_keep_null_arguments() {
        let canonical = envelope(&json!({
            "role": "assistant",
            "text": "hello",
            "tool_names": "read_file,write_file",
        }));
        let tools: Vec<_> = canonical
            .facts()
            .iter()
            .filter(|fact| matches!(fact, CanonicalObservationFactV1::ToolInvocation { .. }))
            .collect();
        assert_eq!(tools.len(), 2);
        assert!(tools.iter().all(|fact| matches!(
            fact,
            CanonicalObservationFactV1::ToolInvocation {
                arguments: Value::Null,
                ..
            }
        )));
    }

    #[test]
    fn no_protocol_echo_authorship_or_invented_lineage() {
        let canonical = envelope(&json!({
            "role": "unknown-protocol",
            "text": "echoed protocol noise",
            "session_id": "must-not-become-agent",
            "provider": "must-not-become-parent",
            "protocol_echo": true,
            "echo": {"role": "user", "text": "forged"},
        }));
        assert!(canonical.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Unknown,
                ..
            }
        )));
        assert!(canonical.relations().agent_id().is_none());
        assert!(canonical.relations().parent_agent_id().is_none());
        let encoded = serde_json::to_value(&canonical).expect("serialize");
        assert!(encoded["relations"].get("parent_message_id").is_none());
        assert_eq!(encoded["relations"]["session_id"], "session-1");
        assert_eq!(encoded["relations"]["message_id"], "message-1");
        assert_eq!(canonical.stable_record_id().as_str(), "message-1");
    }

    #[test]
    fn identity_is_path_independent() {
        let left = envelope(&json!({
            "role": "assistant",
            "text": "hello",
            "source_path": "/tmp/a/session.json",
            "cwd": "/tmp/a",
        }));
        let right = envelope(&json!({
            "role": "assistant",
            "text": "hello",
            "source_path": "/other/b/session.json",
            "cwd": "/other/b",
        }));
        assert_eq!(left.stable_record_id(), right.stable_record_id());
        assert_eq!(
            left.relations().session_id().as_str(),
            right.relations().session_id().as_str()
        );
        let left_json = serde_json::to_string(&left).expect("serialize left");
        let right_json = serde_json::to_string(&right).expect("serialize right");
        assert!(!left_json.contains("/tmp/"));
        assert!(!right_json.contains("/other/"));
        assert_eq!(left_json, right_json);
    }
}
