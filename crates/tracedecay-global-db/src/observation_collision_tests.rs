//! Production-shaped regression tests for the two durable Stage0a collision
//! failures observed on 5ddd16271 (ancestral on 2be2b9478 / 0.1.0-beta.34):
//!
//! 1. `observation_identity_collision` — a rewritten native record presents
//!    the same canonical observation id with a different payload digest. The
//!    refusal is deterministic and non-retryable, so it must record durable
//!    terminal coverage in the typed cursor-advance ledger; later catch-up and
//!    temporal triggers must not decode, classify, canonicalize, or hash that
//!    row again, and the retained row must stay byte-identical.
//! 2. projection-drain provenance collision with an existing output — a
//!    queued observation whose drain collides with an already-persisted
//!    provenance row must converge to a durable `output_collision` skip
//!    (checkpoint advances, queue drains) instead of permanently wedging.
//!    The converged skip must satisfy the skip authority contract
//!    (`schema_contract::invariants`): zero provenance rows for the
//!    observation plus exactly one disposition — never a skip that
//!    contradicts a retained provenance binding.
//!
//! Review-driven contracts pinned alongside:
//! * the refusal terminal survives cursor-advance retention and is bound to
//!   the exact refused candidate digest, so a later canonical payload
//!   revision replay still converges as `CoveredDuplicate`;
//! * coverage is recorded only at the sequential scan frontier — covered
//!   replays and gap-shaped candidates leave every ledger untouched;
//! * only the narrow existing-output collision converges on drain; divergent
//!   workflow/effect state stays a hard error;
//! * no-rework is proven behaviorally with a corruption tripwire: once the
//!   terminal refusal marker exists, the stored observation row's payload
//!   bytes and identity-derivation source columns are corrupted into
//!   undecodable garbage (an engine fixture the harness sanctions for
//!   post-admission corruption setup). The marker fast path never touches
//!   that row, so re-admission still returns the typed `IdentityCollision`
//!   with converged coverage; any regression that re-decodes, re-derives, or
//!   re-hashes stored data hits the corrupted bytes and fails loudly — and by
//!   the real sessions JSONL `FileBytes` path: zero bytes consumed and zero
//!   calls at the fully materialized host-admission boundary means no frame
//!   was deserialized on a subsequent trigger.

use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempDir;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Dispatch, Event, Metadata, Subscriber};
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, DurableObservationV1,
    ObservationCollisionOutcomeV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectionGenerationId, ProviderId, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    SessionId, UtcMicros,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationCoverageReason, ObservationPersistOutcome,
    ObservationProjectionStore, ObservationStore, ObservationStoreError, ObservationWrite,
    ProjectionPersistOutcome, ProjectionSkipReason, SESSION_MESSAGE_PROJECTOR_VERSION,
};

use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay_runtime_core::db::engine::params;

const COLLISION_PROVIDER: &str = "collision-test";
const ADMISSION_WORK_TRACE_TARGET: &str = "tracedecay::observation_admission_work";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AdmissionWorkSnapshot {
    identity_derivations: u64,
    payload_digests: u64,
    runtime_commands: u64,
}

#[derive(Default)]
struct AdmissionWorkTrace {
    identity_derivations: AtomicU64,
    payload_digests: AtomicU64,
    runtime_commands: AtomicU64,
}

impl AdmissionWorkTrace {
    fn snapshot(&self) -> AdmissionWorkSnapshot {
        AdmissionWorkSnapshot {
            identity_derivations: self.identity_derivations.load(Ordering::Relaxed),
            payload_digests: self.payload_digests.load(Ordering::Relaxed),
            runtime_commands: self.runtime_commands.load(Ordering::Relaxed),
        }
    }
}

struct AdmissionWorkSubscriber {
    trace: Arc<AdmissionWorkTrace>,
}

struct AdmissionWorkVisitor<'a> {
    trace: &'a AdmissionWorkTrace,
}

impl Visit for AdmissionWorkVisitor<'_> {
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() != "work" {
            return;
        }
        let counter = match value {
            "identity_derivation" => &self.trace.identity_derivations,
            "payload_digest" => &self.trace.payload_digests,
            "runtime_command" => &self.trace.runtime_commands,
            _ => return,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

impl Subscriber for AdmissionWorkSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target() == ADMISSION_WORK_TRACE_TARGET
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut visitor = AdmissionWorkVisitor { trace: &self.trace };
        event.record(&mut visitor);
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

/// The corrupted `observation_json` the no-rework tripwire writes over a
/// retained row. It stays syntactically valid JSON with a matching
/// `observation_id` (production retention bookkeeping and mount-time audits
/// run `json_valid`/`json_extract` over committed rows), but it fails any
/// `DurableObservationV1` serde decode, carries no identity-derivation
/// material to re-derive, and its bytes hash to a digest no canonical
/// payload could produce.
fn tripwire_observation_json(observation_id: &str) -> String {
    json!({
        "__tripwire": "corrupted stored observation row",
        "observation_id": observation_id,
    })
    .to_string()
}

/// Corrupted committed-cursor bytes: valid JSON, undecodable as a cursor.
const TRIPWIRE_CURSOR_JSON: &str = r#"{"__tripwire":"corrupted committed cursor"}"#;
/// Corrupted stored payload digest: no re-hash of any payload can match it.
const TRIPWIRE_PAYLOAD_DIGEST: &str = "tripwire:corrupted-payload-digest";

/// The fixture writes through the `observations_immutable_update` guard the
/// same way production retention's tombstone writer does: drop the trigger,
/// update inside the same transaction, recreate the trigger.
const DROP_OBSERVATION_UPDATE_TRIGGER: &str =
    "DROP TRIGGER IF EXISTS observations_immutable_update";
const CREATE_OBSERVATION_UPDATE_TRIGGER: &str = "CREATE TRIGGER \
     observations_immutable_update BEFORE UPDATE ON observations BEGIN \
     SELECT RAISE(ABORT, 'observations are immutable'); END";

/// One guarded write over a retained row's authority columns, used both to
/// arm the corruption tripwire and to restore the original bytes.
async fn overwrite_stored_observation_row(
    runtime: &HostAdmissionTestRuntimeV1,
    observation_id: &str,
    payload_digest: &str,
    observation_json: &str,
    committed_cursor_json: &str,
) {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let transaction = database.begin_write_transaction().await.unwrap();
    transaction
        .execute_batch(DROP_OBSERVATION_UPDATE_TRIGGER)
        .await
        .unwrap();
    let written = transaction
        .execute(
            "UPDATE observations
             SET payload_digest = ?2, observation_json = ?3, committed_cursor_json = ?4
             WHERE observation_id = ?1",
            params![
                observation_id,
                payload_digest,
                observation_json,
                committed_cursor_json
            ],
        )
        .await
        .unwrap();
    assert_eq!(written, 1, "the fixture must rewrite exactly one row");
    transaction
        .execute_batch(CREATE_OBSERVATION_UPDATE_TRIGGER)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
}

/// Original stored-row authority bytes captured before the tripwire arms, so
/// restart-bearing tests can restore the row before remount — mount-time
/// invariant convergence legitimately decodes committed observation rows.
struct StoredRowBytes {
    payload_digest: String,
    observation_json: String,
    committed_cursor_json: String,
}

/// Arms the no-rework corruption tripwire on one retained observation row —
/// an engine fixture for post-admission corruption setup, which the harness
/// doc explicitly sanctions.
///
/// The refusal fast path's contract is that a re-admitted identical candidate
/// is answered from the `observation_admission_refusals` marker and the
/// frontier cursor with bare-column reads; it never touches the retained
/// `observations` row. Overwriting that row's payload bytes and
/// identity-derivation source columns with undecodable garbage turns the
/// contract into a behavioral proof: if a regression reintroduces stored-row
/// decode, identity re-derivation, or payload re-hashing on the fast path,
/// the corrupted bytes make it fail loudly instead of passing silently.
async fn corrupt_stored_observation_row(
    runtime: &HostAdmissionTestRuntimeV1,
    observation_id: &str,
) -> StoredRowBytes {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let mut rows = snapshot
        .query(
            "SELECT payload_digest, observation_json, committed_cursor_json
             FROM observations WHERE observation_id = ?1",
            params![observation_id],
        )
        .await
        .expect("query retained observation row");
    let row = rows
        .next()
        .await
        .expect("read retained observation row")
        .expect("retained observation row");
    let original = StoredRowBytes {
        payload_digest: row.get::<String>(0).unwrap(),
        observation_json: row.get::<String>(1).unwrap(),
        committed_cursor_json: row.get::<String>(2).unwrap(),
    };
    drop(rows);
    overwrite_stored_observation_row(
        runtime,
        observation_id,
        TRIPWIRE_PAYLOAD_DIGEST,
        &tripwire_observation_json(observation_id),
        TRIPWIRE_CURSOR_JSON,
    )
    .await;
    original
}

/// Restores the original stored-row bytes captured by
/// [`corrupt_stored_observation_row`], disarming the tripwire before a
/// remount whose invariant convergence legitimately decodes committed rows.
async fn restore_stored_observation_row(
    runtime: &HostAdmissionTestRuntimeV1,
    observation_id: &str,
    original: &StoredRowBytes,
) {
    overwrite_stored_observation_row(
        runtime,
        observation_id,
        &original.payload_digest,
        &original.observation_json,
        &original.committed_cursor_json,
    )
    .await;
}

/// Hides the retained-row table behind a fixture-only name after the refusal
/// marker exists. The marker and cursor authorities remain available, while
/// any regression that issues even a bare-column read against `observations`
/// fails at the SQL boundary instead of being masked by an ignored result.
async fn hide_observation_table_behind_tripwire(runtime: &HostAdmissionTestRuntimeV1) {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let transaction = database.begin_write_transaction().await.unwrap();
    transaction
        .execute_batch("ALTER TABLE observations RENAME TO observations_tripwire_hidden")
        .await
        .expect("hide retained observation table behind tripwire name");
    transaction.commit().await.unwrap();
}

fn fixture_receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            tracedecay_domain::ComponentVersion::new("sanitizer.collision-test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

/// Anchors one observation write exactly as production ingest does.
fn anchored_write_for(
    observation: DurableObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> AnchoredObservationWrite {
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().ordering_domain(),
        observation.identity().position().end(),
    )
    .unwrap();
    anchored_write_with_cursor(observation, expected_cursor, next_cursor)
}

/// Anchors one observation write with an explicit next cursor (e.g. one that
/// carries a JSONL resume checkpoint).
fn anchored_write_with_cursor(
    observation: DurableObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    next_cursor: ObservationSourceCursorV1,
) -> AnchoredObservationWrite {
    let write = ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap();
    let projection_generation =
        ProjectionGenerationId::new("projection.collision-test.v1").unwrap();
    let authorization = tracedecay_store::build_observation_resolution_authorization_v1(
        write.observation(),
        "collision-test",
    )
    .unwrap();
    let anchor = tracedecay_store::build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap()
}

/// Raw persisted source input for one native transcript record: the exact
/// JSONL line a provider transcript would hold.
fn raw_source_line(
    session_id: &SessionId,
    record_id: &str,
    range: (u64, u64),
    text: &str,
) -> String {
    let provider = ProviderId::new(COLLISION_PROVIDER).unwrap();
    let record = ObservationId::new(record_id).unwrap();
    let relations = CanonicalObservationRelationsV1::new(session_id.clone())
        .with_message_id(ObservationId::new(format!("message.{record_id}")).unwrap());
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record,
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": text}),
            model: None,
            timestamp: Some(1_750_000_000),
        }],
        CanonicalObservationEvidenceV1::new(
            ObservationOrderingDomainV1::SnapshotOrder,
            ObservationSourceRangeV1::new(range.0, range.1).unwrap(),
        ),
    )
    .unwrap();
    serde_json::to_string(&envelope).unwrap()
}

/// Decodes one raw source line into the durable candidate a real catch-up
/// pass would build: parse the envelope, rebind identity to the scan's
/// generation and range, and receipt the sanitized payload.
fn decode_raw_source_record(
    session_id: &SessionId,
    raw_line: &str,
    generation: u64,
    range: (u64, u64),
    receipt_id: &str,
) -> DurableObservationV1 {
    let provider = ProviderId::new(COLLISION_PROVIDER).unwrap();
    let source = ObservationSourceIdentityV1::for_provider(provider, session_id.clone()).unwrap();
    let envelope: CanonicalObservationEnvelopeV1 = serde_json::from_str(raw_line).unwrap();
    let record = envelope.stable_record_id().clone();
    let payload = serde_json::to_value(&envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(generation).unwrap(),
        ObservationSourceRangeV1::new(range.0, range.1).unwrap(),
        ObservationOrderingDomainV1::SnapshotOrder,
        record,
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        fixture_receipt(receipt_id, &payload),
        RetentionClass::new("retention.collision-test").unwrap(),
        payload,
    )
    .unwrap()
}

/// One sanitized native transcript record at an explicit source range.
/// Candidates built with the same `record_id` share a canonical observation
/// id regardless of `generation`, range, or payload text — exactly the shape
/// a rewritten source file produces.
fn collision_observation_at(
    session_id: &SessionId,
    record_id: &str,
    generation: u64,
    range: (u64, u64),
    ordering_domain: ObservationOrderingDomainV1,
    text: &str,
    receipt_id: &str,
) -> DurableObservationV1 {
    let provider = ProviderId::new(COLLISION_PROVIDER).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(range.0, range.1).unwrap();
    let record = ObservationId::new(record_id).unwrap();
    let relations = CanonicalObservationRelationsV1::new(session_id.clone())
        .with_message_id(ObservationId::new(format!("message.{record_id}")).unwrap());
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record.clone(),
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": text}),
            model: None,
            timestamp: Some(1_750_000_000),
        }],
        CanonicalObservationEvidenceV1::new(ordering_domain, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(generation).unwrap(),
        range,
        ordering_domain,
        record,
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        fixture_receipt(receipt_id, &payload),
        RetentionClass::new("retention.collision-test").unwrap(),
        payload,
    )
    .unwrap()
}

fn collision_candidate_at(
    session_id: &SessionId,
    record_id: &str,
    generation: u64,
    evidence: CanonicalObservationEvidenceV1,
    text: &str,
    receipt_id: &str,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> (DurableObservationV1, AnchoredObservationWrite) {
    let observation = collision_observation_at(
        session_id,
        record_id,
        generation,
        (evidence.range().start(), evidence.range().end()),
        evidence.ordering_domain(),
        text,
        receipt_id,
    );
    let anchored = anchored_write_for(observation.clone(), expected_cursor);
    (observation, anchored)
}

fn collision_candidate(
    session_id: &SessionId,
    record_id: &str,
    generation: u64,
    text: &str,
    receipt_id: &str,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> (DurableObservationV1, AnchoredObservationWrite) {
    collision_candidate_at(
        session_id,
        record_id,
        generation,
        CanonicalObservationEvidenceV1::new(
            ObservationOrderingDomainV1::SnapshotOrder,
            ObservationSourceRangeV1::new(0, 1).unwrap(),
        ),
        text,
        receipt_id,
        expected_cursor,
    )
}

async fn admission_refused_advance_count(
    runtime: &HostAdmissionTestRuntimeV1,
    observation: &DurableObservationV1,
) -> i64 {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let source_json = serde_json::to_string(observation.source()).unwrap();
    let scope_json = serde_json::to_string(observation.scope()).unwrap();
    let mut rows = snapshot
        .query(
            "SELECT COUNT(*) FROM source_cursor_advances
             WHERE source_json = ?1 AND scope_json = ?2 AND reason = ?3",
            params![
                source_json,
                scope_json,
                ObservationCoverageReason::AdmissionRefused.as_str()
            ],
        )
        .await
        .expect("query cursor-advance ledger");
    rows.next()
        .await
        .expect("read cursor-advance ledger count")
        .expect("cursor-advance ledger count row")
        .get::<i64>(0)
        .expect("decode cursor-advance ledger count")
}

async fn table_count(runtime: &HostAdmissionTestRuntimeV1, table: &str) -> i64 {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let mut rows = snapshot
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
        .expect("query table count");
    rows.next()
        .await
        .expect("read table count")
        .expect("table count row")
        .get::<i64>(0)
        .expect("decode table count")
}

async fn admission_refused_total(runtime: &HostAdmissionTestRuntimeV1) -> i64 {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let mut rows = snapshot
        .query(
            "SELECT COUNT(*) FROM source_cursor_advances WHERE reason = ?1",
            params![ObservationCoverageReason::AdmissionRefused.as_str()],
        )
        .await
        .expect("query admission-refused advances");
    rows.next()
        .await
        .expect("read admission-refused count")
        .expect("admission-refused count row")
        .get::<i64>(0)
        .expect("decode admission-refused count")
}

async fn only_source_cursor(runtime: &HostAdmissionTestRuntimeV1) -> ObservationSourceCursorV1 {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let mut rows = snapshot
        .query("SELECT cursor_json FROM source_cursors", ())
        .await
        .expect("query source cursor");
    let encoded = rows
        .next()
        .await
        .expect("read source cursor")
        .expect("one source cursor")
        .get::<String>(0)
        .expect("decode source cursor column");
    assert!(
        rows.next()
            .await
            .expect("check unique source cursor")
            .is_none(),
        "fixture must have exactly one source cursor"
    );
    serde_json::from_str(&encoded).expect("decode typed source cursor")
}

type ProvenanceRow = (String, String, i64, String, String, String, String, i64);

/// Projected `sessions` row captured verbatim from a clean drain.
type ProjectedSessionRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
);

/// Projected `session_messages` row captured verbatim from a clean drain.
type ProjectedMessageRow = (
    String,
    String,
    String,
    String,
    Option<i64>,
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
);

async fn provenance_rows(runtime: &HostAdmissionTestRuntimeV1) -> Vec<ProvenanceRow> {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let mut rows = snapshot
        .query(
            "SELECT projector_version, observation_id, output_ordinal, receipt_id,
                    output_provider, output_message_id, output_digest, message_created
             FROM observation_projection_provenance
             ORDER BY projector_version, observation_id, output_ordinal",
            (),
        )
        .await
        .expect("query projection provenance");
    let mut collected = Vec::new();
    while let Some(row) = rows.next().await.expect("read projection provenance") {
        collected.push((
            row.get::<String>(0).unwrap(),
            row.get::<String>(1).unwrap(),
            row.get::<i64>(2).unwrap(),
            row.get::<String>(3).unwrap(),
            row.get::<String>(4).unwrap(),
            row.get::<String>(5).unwrap(),
            row.get::<String>(6).unwrap(),
            row.get::<i64>(7).unwrap(),
        ));
    }
    collected
}

/// Stage0a symptom 1, first RED requirement: the first non-retryable identity
/// collision must keep the retained row byte-identical and record durable
/// terminal coverage — the typed source cursor converges past the colliding
/// record and the refusal lands in the `source_cursor_advances` ledger with
/// the typed `admission_refused` reason.
#[tokio::test]
async fn identity_collision_records_durable_admission_refused_coverage() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.identity-collision.durable").unwrap();
    let (original, original_write) = collision_candidate(
        &session_id,
        "record.identity-collision",
        1,
        "original transcript record",
        "receipt.identity-collision.original",
        None,
    );
    assert!(matches!(
        store.persist_observation(original_write).await.unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    let committed_cursor = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();
    assert!(committed_cursor.is_some());

    // The source file was rewritten: a new generation re-presents the same
    // native record id with different content.
    let (rewritten, rewritten_write) = collision_candidate(
        &session_id,
        "record.identity-collision",
        2,
        "rewritten transcript record",
        "receipt.identity-collision.rewritten",
        committed_cursor,
    );
    assert_eq!(rewritten.observation_id(), original.observation_id());
    assert_ne!(
        rewritten.payload_reference().digest(),
        original.payload_reference().digest()
    );

    let error = store
        .persist_observation(rewritten_write.clone())
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            }
        ),
        "{error:?}"
    );

    // Immutable old row: the collision must not overwrite or mutate it.
    let stored = store
        .get_observation(original.observation_id())
        .await
        .unwrap()
        .expect("retained observation row");
    assert_eq!(
        stored.observation().payload_reference().digest(),
        original.payload_reference().digest()
    );
    assert_eq!(stored.observation().payload(), original.payload());

    // Durable terminal coverage: the typed cursor converges past the refused
    // record so catch-up never re-reads it...
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap()
            .as_ref(),
        Some(rewritten_write.next_cursor()),
        "identity collision must advance typed source coverage past the refused record"
    );
    // ...and the refusal is durable in the typed cursor-advance ledger.
    assert_eq!(
        admission_refused_advance_count(&runtime, &original).await,
        1,
        "identity collision must record one durable admission_refused advance"
    );
}

/// Stage0a symptom 1, second RED requirement: once the collision is durably
/// terminal, a re-admitted candidate (late catch-up pass or temporal trigger
/// holding a stale cursor) must fail with the same typed error WITHOUT
/// decoding the stored row, re-classifying the collision, probing the payload
/// revision, or computing another canonical digest.
#[tokio::test]
async fn re_admitted_identity_collision_short_circuits_without_decode_or_hash() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.identity-collision.readmitted").unwrap();
    let (original, original_write) = collision_candidate(
        &session_id,
        "record.identity-collision.readmitted",
        1,
        "original transcript record",
        "receipt.identity-collision.readmitted.original",
        None,
    );
    assert!(matches!(
        store.persist_observation(original_write).await.unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    let committed_cursor = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();
    let (_, rewritten_write) = collision_candidate(
        &session_id,
        "record.identity-collision.readmitted",
        2,
        "rewritten transcript record",
        "receipt.identity-collision.readmitted.rewritten",
        committed_cursor,
    );
    let first = store
        .persist_observation(rewritten_write.clone())
        .await
        .unwrap_err();
    assert!(
        matches!(
            first,
            ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            }
        ),
        "{first:?}"
    );

    // The terminal marker now exists. Arm the corruption tripwire: overwrite
    // the retained row's payload bytes and identity-derivation source columns
    // with undecodable garbage. The marker fast path never reads that row, so
    // re-admission must be unaffected; any regression that re-decodes,
    // re-derives, or re-hashes stored data now fails loudly.
    corrupt_stored_observation_row(&runtime, original.observation_id().as_str()).await;
    hide_observation_table_behind_tripwire(&runtime).await;

    // A later catch-up pass or temporal trigger re-presents the exact same
    // candidate with its now-stale expected cursor.
    let admission_work = Arc::new(AdmissionWorkTrace::default());
    let dispatch = Dispatch::new(AdmissionWorkSubscriber {
        trace: Arc::clone(&admission_work),
    });
    let trace_guard = tracing::dispatcher::set_default(&dispatch);
    let second = store
        .persist_observation(rewritten_write.clone())
        .await
        .unwrap_err();
    drop(trace_guard);
    assert!(
        matches!(
            second,
            ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            }
        ),
        "re-admission over the corrupted retained row must stay the typed terminal \
         collision — any stored-row decode, identity re-derivation, or payload re-hash \
         would have failed on the tripwire bytes; {second:?}"
    );
    assert_eq!(
        admission_work.snapshot(),
        AdmissionWorkSnapshot {
            identity_derivations: 0,
            payload_digests: 0,
            runtime_commands: 1,
        },
        "the terminal fast path must neither re-derive nor re-hash the valid candidate, \
         and may dispatch only the one canonical source-cursor read"
    );
    // Any access to the retained row — including an ignored bare-column read
    // that would evade a byte-corruption tripwire — would have failed because
    // the production table name is no longer present.
    assert_eq!(
        raw_hidden_observation_json(&runtime, original.observation_id().as_str()).await,
        tripwire_observation_json(original.observation_id().as_str()),
        "the fast path must not read or rewrite the hidden retained observation row"
    );
    // The terminal coverage stays single-row and the cursor stays put.
    assert_eq!(
        admission_refused_advance_count(&runtime, &original).await,
        1
    );
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap()
            .as_ref(),
        Some(rewritten_write.next_cursor())
    );
}

/// A replacement generation may change its ordering domain. The canonical
/// cursor-transition authority accepts that shape when the FileBytes range
/// restarts at zero, so collision refusal must commit the same terminal
/// coverage and keep later re-admission on the zero-record-work fast path.
#[tokio::test]
async fn replacement_domain_collision_records_terminal_coverage_without_rework() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.identity-collision.domain-replacement").unwrap();
    let (original, original_write) = collision_candidate(
        &session_id,
        "record.domain-replacement",
        1,
        "snapshot-ordered original record",
        "receipt.domain-replacement.original",
        None,
    );
    assert!(matches!(
        store.persist_observation(original_write).await.unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    let committed_cursor = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();
    assert_eq!(
        committed_cursor
            .as_ref()
            .map(ObservationSourceCursorV1::ordering_domain),
        Some(ObservationOrderingDomainV1::SnapshotOrder)
    );

    let (_, replacement_write) = collision_candidate_at(
        &session_id,
        "record.domain-replacement",
        2,
        CanonicalObservationEvidenceV1::new(
            ObservationOrderingDomainV1::FileBytes,
            ObservationSourceRangeV1::new(0, 1).unwrap(),
        ),
        "file-byte replacement record",
        "receipt.domain-replacement.replacement",
        committed_cursor,
    );
    let first = store
        .persist_observation(replacement_write.clone())
        .await
        .unwrap_err();
    assert!(matches!(
        first,
        ObservationStoreError::ObservationCollision {
            outcome: ObservationCollisionOutcomeV1::IdentityCollision,
            ..
        }
    ));
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap()
            .as_ref(),
        Some(replacement_write.next_cursor()),
        "the FileBytes replacement must commit terminal coverage from position zero"
    );
    assert_eq!(
        admission_refused_advance_count(&runtime, &original).await,
        1
    );
    assert_eq!(admission_refusal_rows(&runtime).await.len(), 1);

    // Arm the corruption tripwire before re-admission: the fast path must
    // answer from the marker without touching the corrupted retained row.
    corrupt_stored_observation_row(&runtime, original.observation_id().as_str()).await;
    let second = store
        .persist_observation(replacement_write)
        .await
        .unwrap_err();
    assert!(
        matches!(
            second,
            ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            }
        ),
        "re-admission over the corrupted retained row must stay the typed terminal \
         collision; {second:?}"
    );
    assert_eq!(
        raw_observation_json(&runtime, original.observation_id().as_str()).await,
        tripwire_observation_json(original.observation_id().as_str()),
        "the fast path must not read back or rewrite the retained observation row"
    );
}

/// Stage0a symptom 2: a projection drain that collides with an existing
/// provenance row for the same observation (an earlier projection era left a
/// divergent output binding behind) must converge to a durable
/// `output_collision` skip — checkpoint advances, the queue drains, replay is
/// an exact duplicate — while the real pre-existing output stays durable and
/// no partial replacement output rows leak.
#[tokio::test]
async fn drain_provenance_collision_with_existing_output_converges_to_durable_skip() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.provenance-collision.drain").unwrap();
    let (observation, write) = collision_candidate(
        &session_id,
        "record.provenance-collision",
        1,
        "provenance drain canary",
        "receipt.provenance-collision",
        None,
    );
    assert!(matches!(
        store.persist_observation(write).await.unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));

    // An earlier projection era bound this observation to a different output:
    // the stored provenance disagrees with what the drain now derives.
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let stale_anchor_id = tracedecay_domain::derive_exact_observation_anchor_id(
        observation.scope(),
        observation.observation_id(),
    )
    .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "INSERT INTO sessions (provider, session_id, project_key, project_path)
             VALUES (?1, ?2, 'user', 'user')",
            params![COLLISION_PROVIDER, session_id.as_str()],
        )
        .await
        .unwrap();
    transaction
        .execute(
            "INSERT INTO session_messages
                (provider, message_id, session_id, role, ordinal, text)
             VALUES (?1, ?2, ?3, 'assistant', 0, 'stale era output')",
            params![COLLISION_PROVIDER, "stale-era-output", session_id.as_str()],
        )
        .await
        .unwrap();
    transaction
        .execute(
            "INSERT INTO observation_projection_provenance
                (projector_version, observation_id, output_ordinal, receipt_id,
                 output_provider, output_message_id, output_digest, message_created,
                 retrieval_anchor_id)
             VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6, 1, ?7)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                observation.observation_id().as_str(),
                observation.receipt().receipt().receipt_id().as_str(),
                COLLISION_PROVIDER,
                "stale-era-output",
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                stale_anchor_id.as_str(),
            ],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let stale_rows = provenance_rows(&runtime).await;
    assert_eq!(stale_rows.len(), 1);

    assert_eq!(
        store.next_queued_observation().await.unwrap().as_ref(),
        Some(observation.observation_id())
    );

    let outcome = store
        .project_observation(observation.observation_id())
        .await
        .expect("drain must converge the provenance collision instead of wedging");
    assert!(
        matches!(
            outcome,
            ProjectionPersistOutcome::Skipped {
                reason: ProjectionSkipReason::OutputCollision,
                ..
            }
        ),
        "{outcome:?}"
    );

    // Checkpoint advanced past the collided observation and the queue drained.
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        1
    );
    assert!(store.next_queued_observation().await.unwrap().is_none());

    // Skip authority contract (schema_contract::invariants): a durable skip
    // is exactly zero provenance rows plus one disposition, with no alias or
    // workflow rows. The contradictory stale binding must be reconciled away,
    // not retained next to the skip, and no partial output rows may leak.
    assert_eq!(
        provenance_rows(&runtime).await,
        Vec::new(),
        "a converged skip must not retain contradictory provenance"
    );
    assert_eq!(
        table_count(&runtime, "observation_projection_aliases").await,
        0
    );
    assert_eq!(table_count(&runtime, "observation_workflow_facts").await, 0);
    assert_eq!(table_count(&runtime, "session_messages").await, 1);
    assert_eq!(table_count(&runtime, "sessions").await, 1);
    // The retained observation row itself stays immutable.
    let stored = store
        .get_observation(observation.observation_id())
        .await
        .unwrap()
        .expect("retained observation row");
    assert_eq!(stored.observation().payload(), observation.payload());

    // The skip is durable: a replay consults the recorded disposition and
    // converges as an exact duplicate.
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT reason FROM observation_projection_dispositions
             WHERE projector_version = ?1 AND observation_id = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                observation.observation_id().as_str()
            ],
        )
        .await
        .unwrap();
    let reason = rows
        .next()
        .await
        .unwrap()
        .expect("durable projection disposition row")
        .get::<String>(0)
        .unwrap();
    assert_eq!(reason, ProjectionSkipReason::OutputCollision.as_str());
    drop(rows);

    assert!(matches!(
        store
            .project_observation(observation.observation_id())
            .await
            .unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
}

/// A provenance binding that names a different output but has no backing
/// `session_messages` row is corrupt authority, not an existing-output
/// collision. It must stay a hard `ProvenanceCollision`: the checkpoint and
/// queue remain in place and the ghost provenance row is not deleted.
#[tokio::test]
async fn drain_keeps_ghost_provenance_binding_a_hard_error() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.provenance-collision.ghost").unwrap();
    let (observation, write) = collision_candidate(
        &session_id,
        "record.provenance-collision.ghost",
        1,
        "ghost provenance canary",
        "receipt.provenance-collision.ghost",
        None,
    );
    assert!(matches!(
        store.persist_observation(write).await.unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));

    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let stale_anchor_id = tracedecay_domain::derive_exact_observation_anchor_id(
        observation.scope(),
        observation.observation_id(),
    )
    .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "INSERT INTO observation_projection_provenance
                (projector_version, observation_id, output_ordinal, receipt_id,
                 output_provider, output_message_id, output_digest, message_created,
                 retrieval_anchor_id)
             VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6, 1, ?7)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                observation.observation_id().as_str(),
                observation.receipt().receipt().receipt_id().as_str(),
                COLLISION_PROVIDER,
                "ghost-output",
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                stale_anchor_id.as_str(),
            ],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let stale_rows = provenance_rows(&runtime).await;
    assert_eq!(stale_rows.len(), 1);
    assert_eq!(table_count(&runtime, "session_messages").await, 0);

    let error = store
        .project_observation(observation.observation_id())
        .await
        .expect_err("a ghost output binding must not be laundered into a durable skip");
    assert!(matches!(
        error,
        tracedecay_store::ProjectionStoreError::ProvenanceCollision
    ));
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        0
    );
    assert_eq!(
        store.next_queued_observation().await.unwrap().as_ref(),
        Some(observation.observation_id())
    );
    assert_eq!(provenance_rows(&runtime).await, stale_rows);
    assert_eq!(
        table_count(&runtime, "observation_projection_dispositions").await,
        0
    );
}

/// Rows in the retained admission-refusal authority. Returns an empty list
/// when the authority table does not exist yet, so contract assertions fail
/// cleanly instead of erroring on a missing table.
async fn admission_refusal_rows(runtime: &HostAdmissionTestRuntimeV1) -> Vec<(String, String)> {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let mut probe = snapshot
        .query(
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'observation_admission_refusals'",
            (),
        )
        .await
        .expect("probe admission refusal authority");
    if probe
        .next()
        .await
        .expect("read admission refusal authority probe")
        .is_none()
    {
        return Vec::new();
    }
    drop(probe);
    let mut rows = snapshot
        .query(
            "SELECT observation_id, refused_payload_digest
             FROM observation_admission_refusals
             ORDER BY observation_id, refused_payload_digest",
            (),
        )
        .await
        .expect("query admission refusal authority");
    let mut collected = Vec::new();
    while let Some(row) = rows.next().await.expect("read admission refusal rows") {
        collected.push((row.get::<String>(0).unwrap(), row.get::<String>(1).unwrap()));
    }
    collected
}

/// Raw `observation_json` column for one retained row, read without decoding
/// so byte-exact immutability (or an untouched tripwire corruption) can be
/// asserted directly.
async fn raw_observation_json(
    runtime: &HostAdmissionTestRuntimeV1,
    observation_id: &str,
) -> String {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let mut rows = snapshot
        .query(
            "SELECT observation_json FROM observations WHERE observation_id = ?1",
            params![observation_id],
        )
        .await
        .expect("query retained observation row");
    rows.next()
        .await
        .expect("read retained observation row")
        .expect("retained observation row")
        .get::<String>(0)
        .expect("decode retained observation column")
}

/// Reads the fixture-hidden retained row after the production fast path has
/// completed, so the test can still prove the tripwire bytes stayed intact.
async fn raw_hidden_observation_json(
    runtime: &HostAdmissionTestRuntimeV1,
    observation_id: &str,
) -> String {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let snapshot = database.read_snapshot().await.expect("read snapshot");
    let mut rows = snapshot
        .query(
            "SELECT observation_json FROM observations_tripwire_hidden
             WHERE observation_id = ?1",
            params![observation_id],
        )
        .await
        .expect("query hidden retained observation row");
    rows.next()
        .await
        .expect("read hidden retained observation row")
        .expect("hidden retained observation row")
        .get::<String>(0)
        .expect("decode hidden retained observation column")
}

/// One real catch-up pass over raw persisted source input: read the durable
/// cursor, decode only the records the cursor does not cover, and persist
/// each decoded candidate exactly as ingest would. Mirrors production provider ingest by
/// ABORTING the pass on a persist error — an identity collision ends the
/// pass, it does not skip to the next record.
async fn run_catch_up_pass(
    store: &crate::GlobalDbObservationStore,
    session_id: &SessionId,
    generation: u64,
    raw_lines: &[((u64, u64), String)],
    pass_label: &str,
) -> (
    usize,
    Vec<Result<ObservationPersistOutcome, ObservationStoreError>>,
) {
    let provider = ProviderId::new(COLLISION_PROVIDER).unwrap();
    let source = ObservationSourceIdentityV1::for_provider(provider, session_id.clone()).unwrap();
    let scope = ObservationScopeV1::Profile;
    let scan_generation = ObservationSourceGenerationV1::new(generation).unwrap();
    let mut decoded = 0;
    let mut receipts = Vec::new();
    for (index, (range, raw_line)) in raw_lines.iter().enumerate() {
        let cursor = store.get_source_cursor(&source, &scope).await.unwrap();
        let covered = cursor.as_ref().is_some_and(|cursor| {
            cursor.generation() == scan_generation
                && cursor.ordering_domain() == ObservationOrderingDomainV1::SnapshotOrder
                && cursor.position() >= range.1
        });
        if covered {
            continue;
        }
        decoded += 1;
        let observation = decode_raw_source_record(
            session_id,
            raw_line,
            generation,
            *range,
            &format!("receipt.catch-up.{pass_label}.{index}"),
        );
        let write = anchored_write_for(observation, cursor);
        let result = store.persist_observation(write).await;
        let aborted = result.is_err();
        receipts.push(result);
        if aborted {
            break;
        }
    }
    (decoded, receipts)
}

/// Linux P1-3, covered-replay shape: an identity collision whose range the
/// durable cursor already covers is a replayed verification probe, not the
/// scan frontier. It must stay a typed fail-closed error and leave every
/// coverage ledger untouched — no `admission_refused` advance row, no
/// refusal-authority row, no cursor movement.
#[tokio::test]
async fn covered_replay_collision_leaves_coverage_state_untouched() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.identity-collision.covered-replay").unwrap();
    let (original, original_write) = collision_candidate(
        &session_id,
        "record.covered-replay",
        1,
        "original transcript record",
        "receipt.covered-replay.original",
        None,
    );
    assert!(matches!(
        store.persist_observation(original_write).await.unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    let committed_cursor = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();

    // Replayed probe: same generation and range the cursor already covers,
    // different payload, no expected cursor (a stale reader's view).
    let (_, covered_write) = collision_candidate(
        &session_id,
        "record.covered-replay",
        1,
        "conflicting replayed payload",
        "receipt.covered-replay.conflicting",
        None,
    );
    let error = store.persist_observation(covered_write).await.unwrap_err();
    assert!(
        matches!(
            error,
            ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            }
        ),
        "{error:?}"
    );

    assert_eq!(
        admission_refused_advance_count(&runtime, &original).await,
        0,
        "a covered replay must not write an admission_refused advance row"
    );
    assert_eq!(admission_refusal_rows(&runtime).await, Vec::new());
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap(),
        committed_cursor,
        "a covered replay must not move the cursor"
    );
}

/// Linux P1-3, stale-expected shape: a colliding candidate whose expected
/// cursor does not match the durable one is not the scan frontier. The
/// refusal must stay the typed identity collision — never a cursor conflict,
/// never recorded coverage.
#[tokio::test]
async fn stale_expected_cursor_collision_stays_a_typed_collision() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.identity-collision.stale-expected").unwrap();
    let (original, original_write) = collision_candidate(
        &session_id,
        "record.stale-expected",
        1,
        "original transcript record",
        "receipt.stale-expected.original",
        None,
    );
    assert!(matches!(
        store.persist_observation(original_write).await.unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    let committed_cursor = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();

    // A fabricated frontier: contiguous with its own claimed position but
    // disagreeing with the durable cursor (durable is at 1, claim is at 5).
    let stale_expected = ObservationSourceCursorV1::for_ordering(
        original.source().clone(),
        original.scope().clone(),
        original.identity().generation(),
        original.identity().ordering_domain(),
        5,
    )
    .unwrap();
    let (_, gap_write) = collision_candidate_at(
        &session_id,
        "record.stale-expected",
        1,
        CanonicalObservationEvidenceV1::new(
            ObservationOrderingDomainV1::SnapshotOrder,
            ObservationSourceRangeV1::new(5, 6).unwrap(),
        ),
        "conflicting gap payload",
        "receipt.stale-expected.conflicting",
        Some(stale_expected),
    );
    let error = store.persist_observation(gap_write).await.unwrap_err();
    assert!(
        matches!(
            error,
            ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            }
        ),
        "a stale-expected collision must stay the typed collision, got {error:?}"
    );

    assert_eq!(
        admission_refused_advance_count(&runtime, &original).await,
        0
    );
    assert_eq!(admission_refusal_rows(&runtime).await, Vec::new());
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap(),
        committed_cursor
    );
}

/// FileBytes replacement generations must restart at zero. The canonical
/// observation-write contract rejects a mid-file replacement before it can
/// reach persistence, so no refusal, coverage, or cursor mutation can occur.
#[tokio::test]
async fn file_bytes_generation_jump_is_rejected_before_persistence() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.identity-collision.generation-jump").unwrap();
    let (original, original_write) = collision_candidate(
        &session_id,
        "record.generation-jump",
        1,
        "original transcript record",
        "receipt.generation-jump.original",
        None,
    );
    assert!(matches!(
        store.persist_observation(original_write).await.unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    let committed_cursor = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();

    // Same record id resurfaces in a NEW generation but mid-file (71..72)
    // with the current durable cursor as the expected view: bytes 0..71 of
    // that generation were never scanned.
    let jump_observation = collision_observation_at(
        &session_id,
        "record.generation-jump",
        2,
        (71, 72),
        ObservationOrderingDomainV1::FileBytes,
        "conflicting jump payload",
        "receipt.generation-jump.conflicting",
    );
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        jump_observation.source().clone(),
        jump_observation.scope().clone(),
        jump_observation.identity().generation(),
        jump_observation.identity().ordering_domain(),
        jump_observation.identity().position().end(),
    )
    .unwrap();
    let error =
        ObservationWrite::new(jump_observation, committed_cursor.clone(), next_cursor).unwrap_err();
    assert!(
        matches!(error, ObservationStoreError::CursorObservationMismatch),
        "{error:?}"
    );

    assert_eq!(
        admission_refused_advance_count(&runtime, &original).await,
        0,
        "a generation jump must not record admission_refused coverage over unscanned bytes"
    );
    assert_eq!(admission_refusal_rows(&runtime).await, Vec::new());
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap(),
        committed_cursor,
        "a generation jump must not move the cursor over unscanned bytes"
    );
}

/// The refusal CAS compares typed cursor authority. JSON whitespace is not
/// part of that authority and must not prevent atomic coverage.
#[tokio::test]
async fn refusal_cas_accepts_equivalent_cursor_json_spelling() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.identity-collision.cursor-wire").unwrap();
    let (original, original_write) = collision_candidate(
        &session_id,
        "record.cursor-wire",
        1,
        "original cursor wire record",
        "receipt.cursor-wire.original",
        None,
    );
    assert!(matches!(
        store.persist_observation(original_write).await.unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    let committed_cursor = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap()
        .expect("committed cursor");

    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let transaction = database.begin_write_transaction().await.unwrap();
    let updated = transaction
        .execute(
            "UPDATE source_cursors
             SET cursor_json = ' ' || cursor_json
             WHERE source_json = ?1 AND scope_json = ?2",
            params![
                serde_json::to_string(original.source()).unwrap(),
                serde_json::to_string(original.scope()).unwrap()
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        updated, 1,
        "the fixture must rewrite the durable cursor row"
    );
    transaction.commit().await.unwrap();
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap()
            .as_ref(),
        Some(&committed_cursor),
        "the alternate JSON spelling must decode to the same typed cursor"
    );

    let (_, refusal_write) = collision_candidate(
        &session_id,
        "record.cursor-wire",
        2,
        "rewritten cursor wire record",
        "receipt.cursor-wire.refused",
        Some(committed_cursor),
    );
    let expected_next = refusal_write.next_cursor().clone();
    let error = store.persist_observation(refusal_write).await.unwrap_err();
    assert!(matches!(
        error,
        ObservationStoreError::ObservationCollision {
            outcome: ObservationCollisionOutcomeV1::IdentityCollision,
            ..
        }
    ));
    assert_eq!(admission_refusal_rows(&runtime).await.len(), 1);
    assert_eq!(
        admission_refused_advance_count(&runtime, &original).await,
        1
    );
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap()
            .as_ref(),
        Some(&expected_next)
    );
}

/// A codex observation in either its legacy route-context form or its current
/// canonical form. Mirrors the workspace revision-compatibility fixtures: the
/// two forms share one canonical observation id, and moving legacy → current
/// is a recognized canonical payload revision replay.
fn codex_revision_observation(
    session_id: &SessionId,
    generation: u64,
    receipt_id: &str,
    legacy: bool,
    content: &str,
) -> DurableObservationV1 {
    let stable_record_id = ObservationId::new("record.codex.revision").unwrap();
    let mut relations = CanonicalObservationRelationsV1::new(session_id.clone())
        .with_message_id(stable_record_id.clone());
    if legacy {
        relations = relations.with_turn_id(ObservationId::new("route.turn").unwrap());
    }
    let session_fact = CanonicalObservationFactV1::Session {
        project_path: Some(if legacy {
            "/route/project".to_owned()
        } else {
            "/stable/project".to_owned()
        }),
        location_path: Some(if legacy {
            "/route/location".to_owned()
        } else {
            "/stable/project".to_owned()
        }),
        transcript_path: legacy.then(|| "/route/rollout.jsonl".to_owned()),
        title: None,
        started_at: None,
        ended_at: None,
        source: Some("codex_rollout".to_owned()),
        native_source: Some("codex".to_owned()),
        profile: None,
        location_provenance: Some("rollout_context".to_owned()),
    };
    let message_fact = CanonicalObservationFactV1::Message {
        role: CanonicalMessageRoleV1::Assistant,
        content: json!(content),
        model: None,
        timestamp: None,
    };
    let range = ObservationSourceRangeV1::new(0, 1).unwrap();
    let payload = serde_json::to_value(
        CanonicalObservationEnvelopeV1::new(
            ProviderId::new("codex").unwrap(),
            "message",
            stable_record_id.clone(),
            relations,
            vec![session_fact, message_fact],
            CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SqliteRowId, range),
        )
        .unwrap(),
    )
    .unwrap();
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new("codex").unwrap(),
        session_id.clone(),
    )
    .unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(generation).unwrap(),
        range,
        ObservationOrderingDomainV1::SqliteRowId,
        stable_record_id,
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        fixture_receipt(receipt_id, &payload),
        RetentionClass::new("retention.collision-test").unwrap(),
        payload,
    )
    .unwrap()
}

/// Codex P2: an earlier invalid rewrite records the refusal terminal, and a
/// LATER candidate at the same generation and range that IS a recognized
/// canonical payload revision replay must still converge as
/// `CoveredDuplicate`. The terminal is bound to the exact refused candidate
/// digest, so it can never blanket-reject every differing digest.
#[tokio::test]
async fn canonical_payload_revision_replay_survives_an_earlier_refusal() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.codex.revision-after-refusal").unwrap();

    // Retained legacy-form record.
    let legacy = codex_revision_observation(
        &session_id,
        1,
        "receipt.codex.revision.legacy",
        true,
        "stable authored content",
    );
    assert!(matches!(
        store
            .persist_observation(anchored_write_for(legacy.clone(), None))
            .await
            .unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    let committed_cursor = store
        .get_source_cursor(legacy.source(), legacy.scope())
        .await
        .unwrap();

    // Invalid rewrite at the rescan frontier (generation 2, from zero):
    // authored content changed, so it is a true identity collision and
    // records the refusal terminal.
    let corrupted = codex_revision_observation(
        &session_id,
        2,
        "receipt.codex.revision.corrupted",
        false,
        "corrupted rewrite content",
    );
    assert_eq!(corrupted.observation_id(), legacy.observation_id());
    let refusal = store
        .persist_observation(anchored_write_for(corrupted.clone(), committed_cursor))
        .await
        .unwrap_err();
    assert!(
        matches!(
            refusal,
            ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            }
        ),
        "{refusal:?}"
    );
    assert_eq!(admission_refused_advance_count(&runtime, &legacy).await, 1);

    // The recognized revision replay arrives at the SAME generation and range
    // as the refusal: same canonical id, same authored content, only the
    // bounded legacy source-context fields differ from the retained form. Its
    // digest differs from BOTH the retained and the refused digests, so the
    // terminal must not swallow it.
    let revision = codex_revision_observation(
        &session_id,
        2,
        "receipt.codex.revision.current",
        false,
        "stable authored content",
    );
    assert_eq!(revision.observation_id(), legacy.observation_id());
    assert_ne!(
        revision.payload_reference().digest(),
        corrupted.payload_reference().digest()
    );
    let outcome = store
        .persist_observation(anchored_write_for(revision, None))
        .await
        .expect("a recognized revision replay must not be terminally rejected");
    assert!(
        matches!(outcome, ObservationPersistOutcome::CoveredDuplicate(_)),
        "{outcome:?}"
    );
    // The retained row is still the legacy form, untouched.
    let stored = store
        .get_observation(legacy.observation_id())
        .await
        .unwrap()
        .expect("retained observation row");
    assert_eq!(stored.observation().payload(), legacy.payload());
}

/// Store-level retention/restart contract driven from raw persisted source
/// input; the real provider-boundary proof is the Vibe journey below:
///
/// 1. a real gen-1 catch-up pass ingests the original record from its raw
///    JSONL line;
/// 2. the file is rewritten (generation 2): the real rescan pass decodes the
///    rewritten record, refuses it terminally, and continues past it;
/// 3. later catch-up passes read the durable cursor and reopen ZERO source
///    records — no decode, no identity derivation, no hashing;
/// 4. production cursor-advance retention reclaims the superseded
///    `admission_refused` advance row, and the terminal STILL holds: a stale
///    in-flight re-admission answers from the retained refusal authority with
///    zero adapter-side stored-row work;
/// 5. the same holds across a full store restart, and the retained row stays
///    byte-identical throughout.
#[tokio::test]
async fn terminal_refusal_survives_retention_and_catch_up_never_reopens_the_record() {
    use crate::observation::retention::{ObservationRetentionConfig, RetentionMode};

    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.terminal-refusal.retention").unwrap();

    // Raw persisted source input.
    let original_lines = vec![(
        (0, 1),
        raw_source_line(
            &session_id,
            "record.retention.0",
            (0, 1),
            "original record zero",
        ),
    )];
    let rewritten_lines = vec![
        (
            (0, 1),
            raw_source_line(
                &session_id,
                "record.retention.0",
                (0, 1),
                "rewritten record zero",
            ),
        ),
        (
            (1, 2),
            raw_source_line(
                &session_id,
                "record.retention.1",
                (1, 2),
                "appended record one",
            ),
        ),
    ];

    // Pass 0: gen-1 ingest of the original file.
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 1, &original_lines, "gen1").await;
    assert_eq!(decoded, 1);
    assert!(matches!(
        receipts[0],
        Ok(ObservationPersistOutcome::Committed(_))
    ));

    // Pass 1: gen-2 rescan of the rewritten file. Record zero collides, is
    // terminally refused, and — like production ingest — ABORTS the pass. The
    // refusal's own coverage advance lets the follow-up pass move on to
    // record one and converge.
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 2, &rewritten_lines, "gen2").await;
    assert_eq!(decoded, 1, "the collision aborts the pass");
    assert!(matches!(
        receipts[0],
        Err(ObservationStoreError::ObservationCollision {
            outcome: ObservationCollisionOutcomeV1::IdentityCollision,
            ..
        })
    ));
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 2, &rewritten_lines, "gen2-resume").await;
    assert_eq!(decoded, 1, "the resumed pass skips the refused coverage");
    assert!(matches!(
        receipts[0],
        Ok(ObservationPersistOutcome::Committed(_))
    ));
    let refused = decode_raw_source_record(
        &session_id,
        &rewritten_lines[0].1,
        2,
        (0, 1),
        "receipt.catch-up.gen2.0",
    );
    let retained_row = raw_observation_json(&runtime, refused.observation_id().as_str()).await;
    assert_eq!(admission_refusal_rows(&runtime).await.len(), 1);

    // The terminal marker exists: arm the corruption tripwire on the retained
    // row. Every later pass in this test — catch-up, production retention,
    // the stale re-admission — must complete without touching it.
    let original_row =
        corrupt_stored_observation_row(&runtime, refused.observation_id().as_str()).await;

    // Pass 2: a later catch-up pass reopens nothing.
    let (decoded, _) = run_catch_up_pass(&store, &session_id, 2, &rewritten_lines, "gen2-b").await;
    assert_eq!(
        decoded, 0,
        "catch-up must not reopen covered source records"
    );

    // Production retention reclaims the superseded admission_refused advance
    // row (the cursor has moved strictly past its coverage).
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let report = database
        .run_observation_retention(
            None,
            &ObservationRetentionConfig::default(),
            RetentionMode::Apply,
            tracedecay_application::clock::now_micros().0,
        )
        .await
        .expect("apply observation retention");
    assert!(report.applied);
    let legacy_observation = decode_raw_source_record(
        &session_id,
        &original_lines[0].1,
        1,
        (0, 1),
        "receipt.catch-up.gen1.0",
    );
    assert_eq!(
        admission_refused_advance_count(&runtime, &legacy_observation).await,
        0,
        "retention must reclaim the superseded admission_refused advance row"
    );
    // The refusal terminal itself is a retained authority.
    assert_eq!(
        admission_refusal_rows(&runtime).await.len(),
        1,
        "the refusal terminal must survive cursor-advance retention"
    );

    // A stale in-flight re-admission (a temporal trigger re-presenting the
    // refused candidate without a current frontier view) still terminates.
    // The armed tripwire is the no-rework proof: any stored-row decode,
    // identity re-derivation, or payload re-hash would fail on the corrupted
    // bytes instead of producing this typed refusal.
    let stale_replay = anchored_write_for(refused.clone(), None);
    let error = store.persist_observation(stale_replay).await.unwrap_err();
    assert!(
        matches!(
            error,
            ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(
        raw_observation_json(&runtime, refused.observation_id().as_str()).await,
        tripwire_observation_json(refused.observation_id().as_str()),
        "no pass may read back, repair, or rewrite the corrupted retained row"
    );

    // Disarm the tripwire before remount: mount-time invariant convergence
    // legitimately decodes committed observation rows.
    restore_stored_observation_row(&runtime, refused.observation_id().as_str(), &original_row)
        .await;

    // Restart: the terminal and coverage are durable, catch-up still reopens
    // nothing, and the retained row is byte-identical.
    drop(store);
    drop(runtime);
    let reopened = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let reopened_store = reopened
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let (decoded, _) =
        run_catch_up_pass(&reopened_store, &session_id, 2, &rewritten_lines, "gen2-c").await;
    assert_eq!(decoded, 0);
    assert_eq!(admission_refusal_rows(&reopened).await.len(), 1);
    assert_eq!(
        raw_observation_json(&reopened, refused.observation_id().as_str()).await,
        retained_row,
        "the retained observation row must stay byte-identical"
    );
}

/// Items 3 and 6 of the owner review, closed together: after production
/// cursor-advance retention has reclaimed the `admission_refused` advance
/// row, a REAL subsequent catch-up/temporal pass — a generation-3 rescan that
/// re-reads the rewritten file from raw persisted source input and rebuilds
/// every candidate through the ingest pipeline, NOT a preconstructed write —
/// re-admits the refused record and must be suppressed by the retained
/// terminal with ZERO store-side decode/canonicalize/SHA work.
///
/// The no-rework proof is the corruption tripwire: the retained row's
/// payload bytes and identity-derivation source columns are garbage for the
/// whole re-admission window, so the typed suppression can only come from
/// the marker fast path — the retained row is never read back, decoded,
/// collision classified, or revision-probed. The real Vibe journey below
/// separately proves the production source boundary performs no subsequent
/// frame materialization.
#[tokio::test]
async fn post_retention_rescan_re_admits_from_raw_source_without_terminal_rework() {
    use crate::observation::retention::{ObservationRetentionConfig, RetentionMode};

    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.terminal-refusal.rescan").unwrap();
    let original_lines = vec![(
        (0, 1),
        raw_source_line(
            &session_id,
            "record.rescan.0",
            (0, 1),
            "original record zero",
        ),
    )];
    let rewritten_lines = vec![
        (
            (0, 1),
            raw_source_line(
                &session_id,
                "record.rescan.0",
                (0, 1),
                "rewritten record zero",
            ),
        ),
        (
            (1, 2),
            raw_source_line(
                &session_id,
                "record.rescan.1",
                (1, 2),
                "appended record one",
            ),
        ),
    ];

    // Collide at N: gen-1 ingest, then the gen-2 rescan refuses the rewritten
    // record terminally and commits the appended record, advancing the cursor
    // strictly past the refused coverage.
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 1, &original_lines, "gen1").await;
    assert_eq!(decoded, 1);
    assert!(matches!(
        receipts[0],
        Ok(ObservationPersistOutcome::Committed(_))
    ));
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 2, &rewritten_lines, "gen2").await;
    assert_eq!(decoded, 1, "the collision aborts the pass like production");
    assert!(matches!(
        receipts[0],
        Err(ObservationStoreError::ObservationCollision {
            outcome: ObservationCollisionOutcomeV1::IdentityCollision,
            ..
        })
    ));
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 2, &rewritten_lines, "gen2-resume").await;
    assert_eq!(decoded, 1, "the resumed pass skips the refused coverage");
    assert!(matches!(
        receipts[0],
        Ok(ObservationPersistOutcome::Committed(_))
    ));
    let refused = decode_raw_source_record(
        &session_id,
        &rewritten_lines[0].1,
        2,
        (0, 1),
        "receipt.catch-up.gen2.0",
    );
    let retained_row = raw_observation_json(&runtime, refused.observation_id().as_str()).await;

    // Arm the corruption tripwire: retention, the gen-3 re-admission, and
    // every later pass must complete without touching the retained row.
    let original_row =
        corrupt_stored_observation_row(&runtime, refused.observation_id().as_str()).await;

    // Run production retention: the superseded admission_refused advance row
    // is reclaimed, the refusal terminal survives.
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    database
        .run_observation_retention(
            None,
            &ObservationRetentionConfig::default(),
            RetentionMode::Apply,
            tracedecay_application::clock::now_micros().0,
        )
        .await
        .expect("apply observation retention");
    assert_eq!(
        admission_refused_advance_count(&runtime, &refused).await,
        0,
        "retention must reclaim the superseded admission_refused advance row"
    );

    // The file changes again: a REAL gen-3 rescan re-reads the raw source and
    // re-admits the refused record through the ingest pipeline itself. The
    // fast path answers from the terminal, converges coverage with one typed
    // cursor-advance write, and aborts the pass like production.
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 3, &rewritten_lines, "gen3").await;
    assert_eq!(
        decoded, 1,
        "a rescan after a real file change re-reads the raw source and aborts on the collision"
    );
    assert!(
        matches!(
            receipts[0],
            Err(ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            })
        ),
        "the re-admission over the corrupted retained row must stay the typed terminal \
         collision — any stored-row decode, identity re-derivation, or payload re-hash \
         would have failed on the tripwire bytes; {:?}",
        receipts[0]
    );
    // The suppression above was answered by the retained refusal terminal:
    // it must have survived cursor-advance retention.
    assert_eq!(
        admission_refusal_rows(&runtime).await.len(),
        1,
        "the refusal terminal must survive cursor-advance retention"
    );
    // The resumed pass commits the appended record past the converged
    // coverage, and the NEXT pass reopens zero source records.
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 3, &rewritten_lines, "gen3-resume").await;
    assert_eq!(decoded, 1, "the resumed pass skips the refused coverage");
    assert!(receipts[0].is_ok(), "{:?}", receipts[0]);
    let (decoded, _) = run_catch_up_pass(&store, &session_id, 3, &rewritten_lines, "gen3-b").await;
    assert_eq!(decoded, 0, "the converged rescan reopens no source records");

    // The corrupted bytes are untouched: no pass read back, repaired, or
    // rewrote the retained row.
    assert_eq!(
        raw_observation_json(&runtime, refused.observation_id().as_str()).await,
        tripwire_observation_json(refused.observation_id().as_str()),
        "no pass may read back, repair, or rewrite the corrupted retained row"
    );
    // Disarm the tripwire; the restored row is byte-identical to the
    // pre-corruption capture.
    restore_stored_observation_row(&runtime, refused.observation_id().as_str(), &original_row)
        .await;
    assert_eq!(
        raw_observation_json(&runtime, refused.observation_id().as_str()).await,
        retained_row,
        "the retained observation row must stay byte-identical"
    );
}

/// EOF gate: the refused record is the LAST record of its file, and
/// production ingest ABORTS a pass on the collision, so no following
/// committed record can ever advance coverage on its behalf. Across
/// production retention, a new generation, and a full restart, the refusal
/// fast path itself must converge each new scan frontier so later passes
/// reopen nothing — zero decode, zero identity derivation, zero hashing,
/// proven by keeping the retained row corrupted for the whole window.
#[tokio::test]
async fn eof_refusal_converges_new_generation_rescans_without_reopening() {
    use crate::observation::retention::{ObservationRetentionConfig, RetentionMode};

    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.terminal-refusal.eof").unwrap();
    // The refused record is the ONLY record: nothing follows it, ever.
    let original_lines = vec![(
        (0, 1),
        raw_source_line(&session_id, "record.eof.0", (0, 1), "original eof record"),
    )];
    let rewritten_lines = vec![(
        (0, 1),
        raw_source_line(&session_id, "record.eof.0", (0, 1), "rewritten eof record"),
    )];

    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 1, &original_lines, "gen1").await;
    assert_eq!(decoded, 1);
    assert!(matches!(
        receipts[0],
        Ok(ObservationPersistOutcome::Committed(_))
    ));

    // Gen-2 rescan: the EOF record collides and the pass aborts. The refusal
    // records terminal + coverage, so the SAME generation never reopens it.
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 2, &rewritten_lines, "gen2").await;
    assert_eq!(decoded, 1);
    assert!(matches!(
        receipts[0],
        Err(ObservationStoreError::ObservationCollision {
            outcome: ObservationCollisionOutcomeV1::IdentityCollision,
            ..
        })
    ));
    let refused = decode_raw_source_record(
        &session_id,
        &rewritten_lines[0].1,
        2,
        (0, 1),
        "receipt.catch-up.gen2.0",
    );
    let retained_row = raw_observation_json(&runtime, refused.observation_id().as_str()).await;

    // Arm the corruption tripwire: retention, the gen-3 re-admission, and
    // every later pass must complete without touching the retained row.
    let original_row =
        corrupt_stored_observation_row(&runtime, refused.observation_id().as_str()).await;
    let (decoded, _) = run_catch_up_pass(&store, &session_id, 2, &rewritten_lines, "gen2-b").await;
    assert_eq!(
        decoded, 0,
        "the refused EOF coverage holds within its generation"
    );

    // Production retention runs (the EOF advance is the frontier itself, so
    // it is not reclaimable yet — the terminal must not depend on that).
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    database
        .run_observation_retention(
            None,
            &ObservationRetentionConfig::default(),
            RetentionMode::Apply,
            tracedecay_application::clock::now_micros().0,
        )
        .await
        .expect("apply observation retention");

    // Gen-3 rescan (file touched again): the re-admit is answered from the
    // terminal AND converges the new generation's coverage, so this exact
    // decode happens once per real file change — never again for gen 3.
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 3, &rewritten_lines, "gen3").await;
    assert_eq!(decoded, 1);
    assert!(
        matches!(
            receipts[0],
            Err(ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            })
        ),
        "the EOF re-admit over the corrupted retained row must stay the typed terminal \
         collision — any stored-row decode, identity re-derivation, or payload re-hash \
         would have failed on the tripwire bytes; {:?}",
        receipts[0]
    );
    let (decoded, _) = run_catch_up_pass(&store, &session_id, 3, &rewritten_lines, "gen3-b").await;
    assert_eq!(
        decoded, 0,
        "later gen-3 passes must never reopen the refused EOF record"
    );

    // Retention now reclaims the superseded gen-2 advance; the terminal and
    // the converged coverage survive.
    database
        .run_observation_retention(
            None,
            &ObservationRetentionConfig::default(),
            RetentionMode::Apply,
            tracedecay_application::clock::now_micros().0,
        )
        .await
        .expect("apply observation retention");
    assert_eq!(admission_refusal_rows(&runtime).await.len(), 1);
    assert_eq!(
        raw_observation_json(&runtime, refused.observation_id().as_str()).await,
        tripwire_observation_json(refused.observation_id().as_str()),
        "no pass may read back, repair, or rewrite the corrupted retained row"
    );

    // Disarm the tripwire before remount: mount-time invariant convergence
    // legitimately decodes committed observation rows.
    restore_stored_observation_row(&runtime, refused.observation_id().as_str(), &original_row)
        .await;

    // Restart: coverage and terminal are durable; nothing reopens.
    drop(store);
    drop(runtime);
    let reopened = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let reopened_store = reopened
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let (decoded, _) =
        run_catch_up_pass(&reopened_store, &session_id, 3, &rewritten_lines, "gen3-c").await;
    assert_eq!(
        decoded, 0,
        "restarted rescans must never reopen the refused EOF record"
    );
    assert_eq!(
        raw_observation_json(&reopened, refused.observation_id().as_str()).await,
        retained_row,
        "the retained observation row must stay byte-identical"
    );
}

/// Atomicity gate: the refusal marker commits before its cursor advance, so a
/// failure between the two — the injected cursor-advance failure state, here
/// seeded durably as exactly what such a crash leaves behind — produces a
/// marker with unconverged coverage. That orphan must be self-repairing: the
/// next frontier pass answers from the marker AND repairs coverage, so the
/// record is reopened at most once and never again.
#[tokio::test]
async fn orphaned_refusal_marker_repairs_coverage_on_the_next_frontier_pass() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.terminal-refusal.orphan").unwrap();
    let original_lines = vec![(
        (0, 1),
        raw_source_line(&session_id, "record.orphan.0", (0, 1), "original record"),
    )];
    let rewritten_lines = vec![(
        (0, 1),
        raw_source_line(&session_id, "record.orphan.0", (0, 1), "rewritten record"),
    )];
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 1, &original_lines, "gen1").await;
    assert_eq!(decoded, 1);
    assert!(matches!(
        receipts[0],
        Ok(ObservationPersistOutcome::Committed(_))
    ));

    // Injected cursor-advance failure: the marker transaction committed, the
    // advance did not. Seed exactly that durable state — the refusal marker
    // exists while the cursor still sits at generation 1.
    let refused = decode_raw_source_record(
        &session_id,
        &rewritten_lines[0].1,
        2,
        (0, 1),
        "receipt.catch-up.gen2.0",
    );
    let retained = decode_raw_source_record(
        &session_id,
        &original_lines[0].1,
        1,
        (0, 1),
        "receipt.catch-up.gen1.0",
    );
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let transaction = database.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "INSERT INTO observation_admission_refusals
                (observation_id, refused_payload_digest, retained_payload_digest, refused_at)
             VALUES (?1, ?2, ?3, 1)",
            params![
                refused.observation_id().as_str(),
                refused.payload_reference().digest().as_str(),
                retained.payload_reference().digest().as_str(),
            ],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    assert_eq!(admission_refusal_rows(&runtime).await.len(), 1);
    let retained_row = raw_observation_json(&runtime, refused.observation_id().as_str()).await;

    // Arm the corruption tripwire: the orphan-marker repair must answer from
    // the marker and the frontier cursor without touching the retained row.
    let original_row =
        corrupt_stored_observation_row(&runtime, refused.observation_id().as_str()).await;

    // The next frontier pass re-admits the record from raw source: the
    // orphaned marker must answer it AND repair the missing coverage.
    let (decoded, receipts) =
        run_catch_up_pass(&store, &session_id, 2, &rewritten_lines, "gen2").await;
    assert_eq!(decoded, 1);
    assert!(
        matches!(
            receipts[0],
            Err(ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            })
        ),
        "the orphan-marker re-admit over the corrupted retained row must stay the typed \
         terminal collision — any stored-row decode, identity re-derivation, or payload \
         re-hash would have failed on the tripwire bytes; {:?}",
        receipts[0]
    );

    // Coverage is repaired: later passes never reopen the record, even after
    // a restart, and no duplicate marker rows appear.
    let (decoded, _) = run_catch_up_pass(&store, &session_id, 2, &rewritten_lines, "gen2-b").await;
    assert_eq!(decoded, 0, "repaired coverage must not reopen the record");
    assert_eq!(admission_refusal_rows(&runtime).await.len(), 1);
    assert_eq!(
        raw_observation_json(&runtime, refused.observation_id().as_str()).await,
        tripwire_observation_json(refused.observation_id().as_str()),
        "no pass may read back, repair, or rewrite the corrupted retained row"
    );

    // Disarm the tripwire before remount: mount-time invariant convergence
    // legitimately decodes committed observation rows.
    restore_stored_observation_row(&runtime, refused.observation_id().as_str(), &original_row)
        .await;
    drop(store);
    drop(runtime);
    let reopened = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let reopened_store = reopened
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let (decoded, _) =
        run_catch_up_pass(&reopened_store, &session_id, 2, &rewritten_lines, "gen2-c").await;
    assert_eq!(decoded, 0);
    assert_eq!(
        raw_observation_json(&reopened, refused.observation_id().as_str()).await,
        retained_row,
        "the retained observation row must stay byte-identical"
    );
}

#[derive(Clone)]
struct ProductionJsonlAdmission {
    store: crate::GlobalDbObservationStore,
    capture_calls: Arc<AtomicU64>,
}

impl ProductionJsonlAdmission {
    fn new(store: crate::GlobalDbObservationStore) -> Self {
        Self {
            store,
            capture_calls: Arc::new(AtomicU64::new(0)),
        }
    }

    fn capture_count(&self) -> u64 {
        self.capture_calls.load(Ordering::Relaxed)
    }

    fn application(
        &self,
    ) -> Result<
        tracedecay_sessions::observation::ObservationApplication<crate::GlobalDbObservationStore>,
        tracedecay_sessions::admission::HostAdmissionOutcome,
    > {
        let sanitizer = tracedecay_runtime_core::privacy::RecordSanitizerV1::observation_v1()
            .map_err(|_| {
                tracedecay_sessions::admission::HostAdmissionOutcome::retained_unavailable(
                    "sanitizer_unavailable",
                )
            })?;
        Ok(
            tracedecay_sessions::observation::ObservationApplication::new(
                self.store.clone(),
                sanitizer,
            ),
        )
    }

    fn classify_application_error(
        error: tracedecay_sessions::observation::ObservationApplicationError,
    ) -> tracedecay_sessions::admission::HostAdmissionOutcome {
        use tracedecay_sessions::observation::ObservationApplicationError;
        match error {
            ObservationApplicationError::Cancelled => {
                tracedecay_sessions::admission::HostAdmissionOutcome::retained_backpressured(
                    "admission_cancelled",
                )
            }
            ObservationApplicationError::Store(ObservationStoreError::CursorConflict {
                ..
            }) => tracedecay_sessions::admission::HostAdmissionOutcome::retained_backpressured(
                "cursor_conflict",
            ),
            ObservationApplicationError::Store(ObservationStoreError::Storage { .. }) => {
                tracedecay_sessions::admission::HostAdmissionOutcome::retained_unavailable(
                    "authority_write_failed",
                )
            }
            ObservationApplicationError::Store(ObservationStoreError::ObservationCollision {
                ..
            }) => tracedecay_sessions::admission::HostAdmissionOutcome::degraded(
                "observation_identity_collision",
            ),
            ObservationApplicationError::Contract(_) => {
                tracedecay_sessions::admission::HostAdmissionOutcome::degraded(
                    "invalid_observation_contract",
                )
            }
            ObservationApplicationError::Privacy(_) => {
                tracedecay_sessions::admission::HostAdmissionOutcome::degraded(
                    "privacy_boundary_failed",
                )
            }
            ObservationApplicationError::Store(_) => {
                tracedecay_sessions::admission::HostAdmissionOutcome::degraded(
                    "observation_store_failed",
                )
            }
        }
    }
}

impl tracedecay_sessions::admission::HostAdmission for ProductionJsonlAdmission {
    fn capture_observation<'a>(
        &'a self,
        request: tracedecay_sessions::observation::CaptureObservationRequest,
    ) -> tracedecay_sessions::admission::AdmissionFuture<
        'a,
        tracedecay_sessions::observation::CaptureObservationOutcome,
    > {
        Box::pin(async move {
            self.capture_calls.fetch_add(1, Ordering::Relaxed);
            self.application()?
                .capture_observation(request)
                .await
                .map_err(Self::classify_application_error)
        })
    }

    fn advance_non_durable_source_cursor<'a>(
        &'a self,
        advance: tracedecay_store::observation::ObservationCursorAdvance,
        cancellation: tracedecay_sessions::observation::ObservationCancellation,
    ) -> tracedecay_sessions::admission::AdmissionFuture<
        'a,
        tracedecay_store::observation::CursorAdvanceOutcome,
    > {
        Box::pin(async move {
            self.application()?
                .advance_non_durable_source_cursor(
                    tracedecay_sessions::observation::AdvanceNonDurableSourceCursorRequest::new(
                        advance,
                        cancellation,
                    ),
                )
                .await
                .map_err(Self::classify_application_error)
        })
    }

    fn get_source_cursor<'a>(
        &'a self,
        source: &'a ObservationSourceIdentityV1,
        scope: &'a ObservationScopeV1,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, Option<ObservationSourceCursorV1>>
    {
        Box::pin(async move {
            self.store
                .get_source_cursor(source, scope)
                .await
                .map_err(|_| {
                    tracedecay_sessions::admission::HostAdmissionOutcome::retained_unavailable(
                        "authority_read_failed",
                    )
                })
        })
    }

    fn drain_projection_queue<'a>(
        &'a self,
        _provider: &'a str,
        _scope: &'a ObservationScopeV1,
        _cancellation: &'a tracedecay_sessions::observation::ObservationCancellation,
        _max: usize,
    ) -> tracedecay_sessions::admission::AdmissionFuture<
        'a,
        tracedecay_sessions::admission::HostProjectionDrainOutcome,
    > {
        Box::pin(async { Ok(Default::default()) })
    }

    fn has_session_message<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _provider: &'a str,
        _message_id: &'a str,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, bool> {
        Box::pin(async { Ok(false) })
    }

    fn get_parse_offset<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _path: &'a str,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, Option<tracedecay_store::ParseOffset>>
    {
        Box::pin(async { Ok(None) })
    }

    fn advance_parse_offset<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _path: &'a str,
        _offset: tracedecay_store::ParseOffset,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn write_vibe_fixture(vibe_home: &Path, workspace: &Path, body: &str) -> std::path::PathBuf {
    let session_dir = vibe_home.join("logs/session/session-vibe-eof-refusal");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join("meta.json"),
        json!({
            "session_id": "session-vibe-eof-refusal",
            "working_directory": workspace,
            "model": "vibe-test-model"
        })
        .to_string(),
    )
    .unwrap();
    let transcript = session_dir.join("messages.jsonl");
    std::fs::write(
        &transcript,
        format!(
            "{}\n",
            json!({"role": "user", "content": body, "timestamp": 1})
        ),
    )
    .unwrap();
    transcript
}

fn replace_vibe_eof(transcript: &Path, body: &str) {
    let replacement = transcript.with_extension("replacement");
    std::fs::write(
        &replacement,
        format!(
            "{}\n",
            json!({"role": "user", "content": body, "timestamp": 1})
        ),
    )
    .unwrap();
    std::fs::remove_file(transcript).unwrap();
    std::fs::rename(replacement, transcript).unwrap();
}

async fn run_vibe_trigger(
    source: &tracedecay_sessions::runtime::vibe::VibeSource,
    workspace: &Path,
    admission: &ProductionJsonlAdmission,
) -> Result<
    tracedecay_sessions::runtime::vibe::VibeCaptureOutcome,
    tracedecay_sessions::runtime::source::TranscriptIngestError,
> {
    tracedecay_sessions::runtime::vibe::capture_vibe_observations(
        admission,
        source,
        workspace,
        ObservationScopeV1::Profile,
        None,
        &tracedecay_sessions::observation::ObservationCancellation::default(),
    )
    .await
}

/// Real provider-path gate: a one-record Vibe `messages.jsonl` is admitted by
/// the shipping shared JSONL scanner with `FileBytes` resume checkpoints.
/// A rewritten EOF record collides once; the global-db transaction records the
/// refusal and cursor together. Retention, another file generation, the next
/// trigger, and a full store restart must never rematerialize that record.
#[tokio::test]
async fn vibe_jsonl_eof_refusal_survives_retention_generation_and_restart_without_rework() {
    use crate::observation::retention::{ObservationRetentionConfig, RetentionMode};

    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let vibe_home = tmp.path().join("vibe-home");
    let transcript = write_vibe_fixture(&vibe_home, &workspace, "original eof record");
    let source = tracedecay_sessions::runtime::vibe::VibeSource::with_vibe_home(&vibe_home)
        .for_user_scope(Vec::new());
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path().join("profile"))
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let admission = ProductionJsonlAdmission::new(store);

    let initial_calls = admission.capture_count();
    let initial = run_vibe_trigger(&source, &workspace, &admission)
        .await
        .expect("original EOF record must persist through the production Vibe path");
    assert!(initial.bytes_consumed > 0);
    assert_eq!(admission.capture_count() - initial_calls, 1);
    let original_cursor = only_source_cursor(&runtime).await;
    assert_eq!(
        original_cursor.ordering_domain(),
        ObservationOrderingDomainV1::FileBytes
    );
    assert_eq!(
        original_cursor.position(),
        std::fs::metadata(&transcript).unwrap().len()
    );

    let settled_calls = admission.capture_count();
    let settled = run_vibe_trigger(&source, &workspace, &admission)
        .await
        .expect("unchanged Vibe source must resume at EOF");
    assert_eq!(settled.bytes_consumed, 0);
    assert_eq!(admission.capture_count() - settled_calls, 0);

    replace_vibe_eof(&transcript, "rewritten eof record");
    let collision_calls = admission.capture_count();
    let collision = run_vibe_trigger(&source, &workspace, &admission)
        .await
        .expect_err("the rewritten stable EOF identity must collide once");
    assert!(matches!(
        collision,
        tracedecay_sessions::runtime::source::TranscriptIngestError::HostAdmission {
            provider: "vibe",
            reason: "observation_identity_collision",
            retryable: false,
        }
    ));
    assert_eq!(admission.capture_count() - collision_calls, 1);
    let refusals = admission_refusal_rows(&runtime).await;
    assert_eq!(refusals.len(), 1);
    assert_eq!(admission_refused_total(&runtime).await, 1);
    let refused_cursor = only_source_cursor(&runtime).await;
    assert_ne!(refused_cursor.generation(), original_cursor.generation());
    assert_eq!(
        refused_cursor.position(),
        std::fs::metadata(&transcript).unwrap().len()
    );
    let retained_observation_id = refusals[0].0.clone();
    let retained_row = raw_observation_json(&runtime, &retained_observation_id).await;

    let next_calls = admission.capture_count();
    let next = run_vibe_trigger(&source, &workspace, &admission)
        .await
        .expect("atomic refusal coverage must settle the next trigger");
    assert_eq!(next.bytes_consumed, 0);
    assert_eq!(
        admission.capture_count() - next_calls,
        0,
        "zero calls at the materialized HostAdmission boundary proves no frame deserialize, native/canonical ID derivation, or payload hashing"
    );
    assert_eq!(only_source_cursor(&runtime).await, refused_cursor);

    runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap()
        .run_observation_retention(
            None,
            &ObservationRetentionConfig::default(),
            RetentionMode::Apply,
            tracedecay_application::clock::now_micros().0,
        )
        .await
        .expect("apply observation retention");
    assert_eq!(admission_refusal_rows(&runtime).await.len(), 1);

    replace_vibe_eof(&transcript, "rewritten eof record");
    let generation_calls = admission.capture_count();
    let generation_collision = run_vibe_trigger(&source, &workspace, &admission)
        .await
        .expect_err("a real new file generation re-admits the refused EOF exactly once");
    assert!(matches!(
        generation_collision,
        tracedecay_sessions::runtime::source::TranscriptIngestError::HostAdmission {
            provider: "vibe",
            reason: "observation_identity_collision",
            retryable: false,
        }
    ));
    assert_eq!(admission.capture_count() - generation_calls, 1);
    let generation_cursor = only_source_cursor(&runtime).await;
    assert_ne!(generation_cursor.generation(), refused_cursor.generation());
    assert_eq!(
        generation_cursor.position(),
        std::fs::metadata(&transcript).unwrap().len()
    );

    let generation_settled_calls = admission.capture_count();
    let generation_settled = run_vibe_trigger(&source, &workspace, &admission)
        .await
        .expect("the new-generation refusal must settle its FileBytes cursor");
    assert_eq!(generation_settled.bytes_consumed, 0);
    assert_eq!(admission.capture_count() - generation_settled_calls, 0);
    assert_eq!(only_source_cursor(&runtime).await, generation_cursor);

    runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap()
        .run_observation_retention(
            None,
            &ObservationRetentionConfig::default(),
            RetentionMode::Apply,
            tracedecay_application::clock::now_micros().0,
        )
        .await
        .expect("apply post-generation observation retention");
    assert_eq!(admission_refusal_rows(&runtime).await.len(), 1);
    assert!(admission_refused_total(&runtime).await >= 1);
    assert_eq!(
        raw_observation_json(&runtime, &retained_observation_id).await,
        retained_row,
        "collision handling must never rewrite the retained observation row"
    );

    drop(admission);
    drop(runtime);
    let reopened = HostAdmissionTestRuntimeV1::profile(tmp.path().join("profile"))
        .await
        .unwrap();
    let reopened_admission = ProductionJsonlAdmission::new(
        reopened
            .observation_store(HostAdmissionScope::Profile)
            .unwrap(),
    );
    let restart_calls = reopened_admission.capture_count();
    let restart = run_vibe_trigger(&source, &workspace, &reopened_admission)
        .await
        .expect("restart must resume at the durable new-generation EOF");
    assert_eq!(restart.bytes_consumed, 0);
    assert_eq!(reopened_admission.capture_count() - restart_calls, 0);
    assert_eq!(only_source_cursor(&reopened).await, generation_cursor);
    assert_eq!(admission_refusal_rows(&reopened).await.len(), 1);
    assert_eq!(
        raw_observation_json(&reopened, &retained_observation_id).await,
        retained_row
    );
}

/// Linux gate 2: a REAL injected cursor-advance failure — a conflicting
/// coverage row already owns the exact advance-ledger key the refusal must
/// claim, so recording coverage genuinely fails inside the authority
/// transaction. Marker and coverage are one atomic transaction: the failure
/// must leave NO visible refusal marker (no orphan), no cursor movement, and
/// the injected row untouched; clearing the conflict lets the next frontier
/// pass record marker + coverage together.
#[tokio::test]
async fn failed_coverage_advance_leaves_no_visible_refusal_marker() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.refusal.atomic-injection").unwrap();
    let (original, original_write) = collision_candidate(
        &session_id,
        "record.atomic-injection",
        1,
        "original transcript record",
        "receipt.atomic-injection.original",
        None,
    );
    assert!(matches!(
        store.persist_observation(original_write).await.unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    let committed_cursor = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();

    // Inject the advance failure: a conflicting coverage row already holds
    // the exact (source, scope, coverage) key the refusal will claim, with a
    // different reason and a bound receipt.
    let (rewritten, rewritten_write) = collision_candidate(
        &session_id,
        "record.atomic-injection",
        2,
        "rewritten transcript record",
        "receipt.atomic-injection.rewritten",
        committed_cursor.clone(),
    );
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let source_json = serde_json::to_string(rewritten.source()).unwrap();
    let scope_json = serde_json::to_string(rewritten.scope()).unwrap();
    let coverage_json =
        serde_json::to_string(&tracedecay_store::observation::ObservationCoverageV1::new(
            rewritten.identity().generation(),
            rewritten.identity().ordering_domain(),
            rewritten.identity().position(),
        ))
        .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "INSERT INTO source_cursor_advances
                (source_json, scope_json, coverage_json, reason, receipt_id)
             VALUES (?1, ?2, ?3, 'canonical_payload_revision', ?4)",
            params![
                source_json.as_str(),
                scope_json.as_str(),
                coverage_json.as_str(),
                original.receipt().receipt().receipt_id().as_str(),
            ],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    // The refusal hits the injected advance failure. Marker and coverage are
    // atomic, so NOTHING may be visible: no orphan marker, no cursor move.
    let error = store
        .persist_observation(rewritten_write.clone())
        .await
        .unwrap_err();
    assert_eq!(
        admission_refusal_rows(&runtime).await,
        Vec::new(),
        "a failed coverage advance must not leave a visible refusal marker"
    );
    assert!(
        matches!(error, ObservationStoreError::CursorAdvanceCollision),
        "the injected advance failure must surface as the typed cursor-advance \
         collision, got {error:?}"
    );
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap(),
        committed_cursor,
        "a failed coverage advance must not move the cursor"
    );

    // Clear the injected conflict (operator remediation) and re-present: the
    // refusal records marker + coverage together.
    let transaction = database.begin_write_transaction().await.unwrap();
    transaction
        .execute_batch("DROP TRIGGER IF EXISTS source_cursor_advances_immutable_delete_v1")
        .await
        .unwrap();
    transaction
        .execute(
            "DELETE FROM source_cursor_advances
             WHERE source_json = ?1 AND scope_json = ?2 AND coverage_json = ?3",
            params![
                source_json.as_str(),
                scope_json.as_str(),
                coverage_json.as_str()
            ],
        )
        .await
        .unwrap();
    transaction
        .execute_batch(
            "CREATE TRIGGER source_cursor_advances_immutable_delete_v1 BEFORE DELETE ON \
             source_cursor_advances BEGIN SELECT RAISE(ABORT, \
             'source cursor advances are immutable'); END",
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let error = store
        .persist_observation(rewritten_write.clone())
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            ObservationStoreError::ObservationCollision {
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(admission_refusal_rows(&runtime).await.len(), 1);
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap()
            .as_ref(),
        Some(rewritten_write.next_cursor()),
        "marker and coverage must land together"
    );
}

/// Narrow-collision gate: a durable provenance row that names the SAME output
/// as the drain now derives but disagrees on its content — corrupt digest,
/// receipt, or anchor — is corrupt provenance authority, not an
/// existing-output collision. It must stay a hard `ProvenanceCollision` with
/// the queue item retained and the checkpoint unmoved; only a row binding a
/// DIFFERENT output converges to the durable skip.
#[tokio::test]
async fn drain_keeps_corrupt_provenance_with_matching_output_a_hard_error() {
    // Learn the derived output binding from a clean drain of the identical
    // fixture in a scratch store (the derivation is deterministic).
    let scratch = TempDir::new().unwrap();
    let scratch_runtime = HostAdmissionTestRuntimeV1::profile(scratch.path())
        .await
        .unwrap();
    let scratch_store = scratch_runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.provenance.corrupt-binding").unwrap();
    let (scratch_observation, scratch_write) = collision_candidate(
        &session_id,
        "record.corrupt-binding",
        1,
        "corrupt binding canary",
        "receipt.corrupt-binding",
        None,
    );
    assert!(matches!(
        scratch_store
            .persist_observation(scratch_write)
            .await
            .unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    assert!(matches!(
        scratch_store
            .project_observation(scratch_observation.observation_id())
            .await
            .unwrap(),
        ProjectionPersistOutcome::Projected(_)
    ));
    let clean_rows = provenance_rows(&scratch_runtime).await;
    assert_eq!(clean_rows.len(), 1);
    let (_, _, _, _, derived_provider, derived_message_id, _, _) = clean_rows[0].clone();
    // Capture the clean drain's projected session and message rows verbatim.
    let scratch_database = scratch_runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered scratch database");
    let scratch_snapshot = scratch_database.read_snapshot().await.unwrap();
    let mut rows = scratch_snapshot
        .query(
            "SELECT provider, session_id, project_key, project_path, title, started_at,
                    ended_at, transcript_path, metadata_json, parent_session_id, is_subagent,
                    agent_id, parent_tool_use_id
             FROM sessions",
            (),
        )
        .await
        .unwrap();
    let session_row = rows.next().await.unwrap().expect("projected session row");
    let projected_session: ProjectedSessionRow = (
        session_row.get(0).unwrap(),
        session_row.get(1).unwrap(),
        session_row.get(2).unwrap(),
        session_row.get(3).unwrap(),
        session_row.get(4).unwrap(),
        session_row.get(5).unwrap(),
        session_row.get(6).unwrap(),
        session_row.get(7).unwrap(),
        session_row.get(8).unwrap(),
        session_row.get(9).unwrap(),
        session_row.get(10).unwrap(),
        session_row.get(11).unwrap(),
        session_row.get(12).unwrap(),
    );
    drop(rows);
    let mut rows = scratch_snapshot
        .query(
            "SELECT provider, message_id, session_id, role, timestamp, ordinal, text, kind,
                    model, tool_names, source_path, source_offset, metadata_json
             FROM session_messages",
            (),
        )
        .await
        .unwrap();
    let message_row = rows.next().await.unwrap().expect("projected message row");
    let projected_message: ProjectedMessageRow = (
        message_row.get(0).unwrap(),
        message_row.get(1).unwrap(),
        message_row.get(2).unwrap(),
        message_row.get(3).unwrap(),
        message_row.get(4).unwrap(),
        message_row.get(5).unwrap(),
        message_row.get(6).unwrap(),
        message_row.get(7).unwrap(),
        message_row.get(8).unwrap(),
        message_row.get(9).unwrap(),
        message_row.get(10).unwrap(),
        message_row.get(11).unwrap(),
        message_row.get(12).unwrap(),
    );
    drop(rows);

    // Main store: the SAME observation with its projected output rows already
    // durable — but the provenance row naming that SAME output carries a
    // corrupt digest. This is discordant provenance authority, not an
    // existing-output collision.
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let (observation, write) = collision_candidate(
        &session_id,
        "record.corrupt-binding",
        1,
        "corrupt binding canary",
        "receipt.corrupt-binding",
        None,
    );
    assert!(matches!(
        store.persist_observation(write).await.unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let anchor_id = tracedecay_domain::derive_exact_observation_anchor_id(
        observation.scope(),
        observation.observation_id(),
    )
    .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "INSERT INTO sessions
                (provider, session_id, project_key, project_path, title, started_at, ended_at,
                 transcript_path, metadata_json, parent_session_id, is_subagent, agent_id,
                 parent_tool_use_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                projected_session.0.as_str(),
                projected_session.1.as_str(),
                projected_session.2.as_str(),
                projected_session.3.as_str(),
                projected_session.4.as_deref(),
                projected_session.5,
                projected_session.6,
                projected_session.7.as_deref(),
                projected_session.8.as_deref(),
                projected_session.9.as_deref(),
                projected_session.10,
                projected_session.11.as_deref(),
                projected_session.12.as_deref(),
            ],
        )
        .await
        .unwrap();
    transaction
        .execute(
            "INSERT INTO session_messages
                (provider, message_id, session_id, role, timestamp, ordinal, text, kind, model,
                 tool_names, source_path, source_offset, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                projected_message.0.as_str(),
                projected_message.1.as_str(),
                projected_message.2.as_str(),
                projected_message.3.as_str(),
                projected_message.4,
                projected_message.5,
                projected_message.6.as_str(),
                projected_message.7.as_deref(),
                projected_message.8.as_deref(),
                projected_message.9.as_deref(),
                projected_message.10.as_deref(),
                projected_message.11,
                projected_message.12.as_deref(),
            ],
        )
        .await
        .unwrap();
    transaction
        .execute(
            "INSERT INTO observation_projection_provenance
                (projector_version, observation_id, output_ordinal, receipt_id,
                 output_provider, output_message_id, output_digest, message_created,
                 retrieval_anchor_id)
             VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6, 1, ?7)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                observation.observation_id().as_str(),
                observation.receipt().receipt().receipt_id().as_str(),
                derived_provider.as_str(),
                derived_message_id.as_str(),
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                anchor_id.as_str(),
            ],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let seeded_rows = provenance_rows(&runtime).await;

    let error = store
        .project_observation(observation.observation_id())
        .await
        .expect_err("corrupt provenance naming the derived output must stay a hard error");
    assert!(
        matches!(
            error,
            tracedecay_store::ProjectionStoreError::ProvenanceCollision
        ),
        "{error:?}"
    );
    assert_eq!(
        store.next_queued_observation().await.unwrap().as_ref(),
        Some(observation.observation_id()),
        "corrupt provenance must not be silently skipped past"
    );
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        0,
        "the checkpoint must not move over corrupt provenance"
    );
    assert_eq!(
        provenance_rows(&runtime).await,
        seeded_rows,
        "the corrupt row is evidence and must stay untouched"
    );
}

/// Linux P1-2: only the narrow existing-output collision converges to a
/// durable skip. Divergent durable workflow-fact state is corrupt authority,
/// not an output collision — it must stay a hard `ProvenanceCollision` error
/// with the queue item retained and the checkpoint unmoved.
#[tokio::test]
async fn drain_keeps_divergent_workflow_fact_state_a_hard_error() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();

    // A checked-in codex goal record: its canonical projection carries
    // workflow facts alongside the message output.
    let record_id = ObservationId::new("record.goal.divergent").unwrap();
    let encoded = include_str!(
        "../../../tests/fixtures/provider_normalization/codex/thread_goal_updated.expected_envelope.json"
    )
    .replace("$STABLE_RECORD_ID", record_id.as_str());
    let envelope: CanonicalObservationEnvelopeV1 = serde_json::from_str(&encoded).unwrap();
    let provider = envelope.provider().clone();
    let goal_session = envelope.relations().session_id().clone();
    let range = envelope.evidence().range();
    let ordering_domain = envelope.evidence().ordering_domain();
    let payload = serde_json::to_value(&envelope).unwrap();
    let source = ObservationSourceIdentityV1::for_provider(provider, goal_session).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(1).unwrap(),
        range,
        ordering_domain,
        record_id,
    )
    .unwrap();
    let observation = DurableObservationV1::new(
        identity,
        fixture_receipt("receipt.goal.divergent", &payload),
        RetentionClass::new("retention.collision-test").unwrap(),
        payload,
    )
    .unwrap();
    assert!(matches!(
        store
            .persist_observation(anchored_write_for(observation.clone(), None))
            .await
            .unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));

    // Divergent durable workflow-fact rows already hold this observation's
    // fact ordinals with different content.
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let anchor_id = tracedecay_domain::derive_exact_observation_anchor_id(
        observation.scope(),
        observation.observation_id(),
    )
    .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    for ordinal in 0..4_i64 {
        transaction
            .execute(
                "INSERT INTO observation_workflow_facts (
                    projector_version, observation_id, fact_ordinal, retrieval_anchor_id,
                    receipt_id, observation_sequence, provider, session_id, semantic_kind,
                    ordering_domain, content_text, output_digest
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 1, 'codex', ?6, 'goal', ?7,
                           'divergent seeded goal state',
                           'sha256:0000000000000000000000000000000000000000000000000000000000000000')",
                params![
                    SESSION_MESSAGE_PROJECTOR_VERSION,
                    observation.observation_id().as_str(),
                    ordinal,
                    anchor_id.as_str(),
                    observation.receipt().receipt().receipt_id().as_str(),
                    observation.source().session_id().as_str(),
                    "snapshot_order",
                ],
            )
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();

    let error = store
        .project_observation(observation.observation_id())
        .await
        .expect_err("divergent durable workflow state must stay a hard error");
    assert!(
        matches!(
            error,
            tracedecay_store::ProjectionStoreError::ProvenanceCollision
        ),
        "{error:?}"
    );
    // The queue item is retained and the checkpoint has not moved: corrupt
    // authority is surfaced, never silently skipped past.
    assert_eq!(
        store.next_queued_observation().await.unwrap().as_ref(),
        Some(observation.observation_id())
    );
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        0
    );
}
