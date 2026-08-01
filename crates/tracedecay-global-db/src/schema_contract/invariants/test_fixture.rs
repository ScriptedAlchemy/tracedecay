//! Shared authority-row fixtures for the invariant tests.
//!
//! Test-only scaffolding: the repair, row-audit, and trigger tests all need the
//! same committed observation shape, so it is built once here rather than
//! diverging three ways. The payload is the checked-in Codex envelope, so a
//! seeded row decodes through the same contract the production audit uses.

use serde_json::Value;
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, ComponentVersion, DurableObservationV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
};

use tracedecay_runtime_core::db::engine::{Executor, TestConnection, params};
use crate::ensure_registered_schema;

pub(super) const GENERATION: u64 = 7;

/// An empty registered global database on a real file, with the full authority
/// schema installed.
pub(super) async fn open_registered() -> (TempDir, TestConnection) {
    let directory = TempDir::new().expect("temporary invariant database");
    let connection = TestConnection::open(&directory.path().join("sessions.db"));
    ensure_registered_schema(&connection)
        .await
        .expect("registered schema");
    (directory, connection)
}

/// A committed observation and the source cursor its commit implies.
pub(super) fn authority_fixture(
    index: u64,
    label: &str,
) -> (DurableObservationV1, ObservationSourceCursorV1) {
    let record_id = format!("record.invariant-{label}-{index}");
    let session_id = format!("invariant-{label}");
    let mut fixture: Value = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/provider_normalization/codex/session_meta.expected_envelope.json"
    ))
    .expect("checked-in codex envelope fixture");
    fixture["stable_record_id"] = Value::String(record_id.clone());
    fixture["relations"]["session_id"] = Value::String(session_id.clone());
    fixture["relations"]["thread_id"] = Value::String(session_id);
    let envelope: CanonicalObservationEnvelopeV1 =
        serde_json::from_value(fixture).expect("decode codex envelope fixture");
    let source = ObservationSourceIdentityV1::for_provider(
        envelope.provider().clone(),
        envelope.relations().session_id().clone(),
    )
    .expect("observation source identity");
    let payload = serde_json::to_value(envelope).expect("encode codex envelope fixture");
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt.invariant-{label}-{index}")).unwrap(),
            ComponentVersion::new("sanitizer.invariant.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
    )
    .expect("sanitization receipt");
    let generation = ObservationSourceGenerationV1::new(GENERATION).unwrap();
    let start = index * 100;
    let end = start + 100;
    let observation = DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source.clone(),
            ObservationScopeV1::Profile,
            generation,
            ObservationSourceRangeV1::new(start, end).unwrap(),
            ObservationOrderingDomainV1::FileBytes,
            ObservationId::new(record_id).unwrap(),
        )
        .expect("observation identity material"),
        receipt,
        RetentionClass::new("retention.invariant").unwrap(),
        payload,
    )
    .expect("durable observation");
    let cursor =
        ObservationSourceCursorV1::new(source, ObservationScopeV1::Profile, generation, end)
            .expect("committed source cursor");
    (observation, cursor)
}

/// Writes the receipt and observation rows exactly as the committing writer
/// does, so the redundant authority columns start out agreeing with their JSON.
pub(super) async fn seed_observation(
    conn: &impl Executor,
    index: u64,
    label: &str,
) -> (DurableObservationV1, ObservationSourceCursorV1) {
    let (observation, cursor) = authority_fixture(index, label);
    let receipt = observation.receipt();
    let payload_digest = observation.payload_reference().digest().as_str().to_owned();
    conn.execute(
        "INSERT INTO sanitization_receipts
         (receipt_id, sanitizer_version, payload_digest, receipt_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            receipt.receipt().receipt_id().as_str(),
            receipt.receipt().sanitizer_version().as_str(),
            payload_digest.as_str(),
            serde_json::to_string(receipt).unwrap()
        ],
    )
    .await
    .expect("seed sanitization receipt");
    conn.execute(
        "INSERT INTO observations
         (observation_id, payload_digest, receipt_id, observation_json, committed_cursor_json)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            observation.observation_id().as_str(),
            payload_digest.as_str(),
            receipt.receipt().receipt_id().as_str(),
            serde_json::to_string(&observation).unwrap(),
            serde_json::to_string(&cursor).unwrap()
        ],
    )
    .await
    .expect("seed committed observation");
    (observation, cursor)
}

/// The same cursor shifted along its ordering domain.
pub(super) fn shift(cursor: &ObservationSourceCursorV1, delta: i64) -> ObservationSourceCursorV1 {
    let position = u64::try_from(i64::try_from(cursor.position()).unwrap() + delta).unwrap();
    ObservationSourceCursorV1::new(
        cursor.source().clone(),
        cursor.scope().clone(),
        cursor.generation(),
        position,
    )
    .expect("shifted source cursor")
}

pub(super) async fn write_cursor(conn: &impl Executor, cursor: &ObservationSourceCursorV1) {
    conn.execute(
        "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
         VALUES (?1, ?2, ?3)",
        params![
            serde_json::to_string(cursor.source()).unwrap(),
            serde_json::to_string(cursor.scope()).unwrap(),
            serde_json::to_string(cursor).unwrap()
        ],
    )
    .await
    .expect("seed stored source cursor");
}
