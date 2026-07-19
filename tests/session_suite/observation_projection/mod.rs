use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::global_db::GlobalDb;
use tracedecay::store::GlobalDbObservationStore;
use tracedecay_domain::{
    CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationIdV1,
    CanonicalObservationRelationsV1, CanonicalReasoningVisibilityV1, ClaudeByteRangeV1,
    ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1, ClaudeSourceCursorV1,
    ClaudeSourceIdentityV1, ComponentVersion, DurableClaudeObservationV1, DurableObservationV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadDigestV1, PayloadReferenceV1, ProjectionGenerationId,
    ProviderId, RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
    derive_exact_observation_anchor_id,
};
use tracedecay_store::{
    AnchoredObservationWrite, CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION, ObservationPersistOutcome,
    ObservationProjectionStatus, ObservationProjectionStore, ObservationStore, ObservationWrite,
    ProjectionPersistOutcome, ProjectionRebuildOutcome, ProjectionSkipReason, ProjectionStoreError,
    SESSION_MESSAGE_PROJECTOR_VERSION_V2, SESSION_MESSAGE_PROJECTOR_VERSION_V4,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

use crate::common::{isolated_lcm_db_path, open_lcm_db};

const GENERATION: u64 = 11;

fn source(session_id: &str) -> ClaudeSourceIdentityV1 {
    ClaudeSourceIdentityV1::new(SessionId::new(session_id).unwrap()).unwrap()
}

fn cursor(session_id: &str, byte_offset: u64) -> ClaudeSourceCursorV1 {
    cursor_in_generation(session_id, GENERATION, byte_offset)
}

fn cursor_in_generation(
    session_id: &str,
    generation: u64,
    byte_offset: u64,
) -> ClaudeSourceCursorV1 {
    ClaudeSourceCursorV1::new(
        source(session_id),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(generation).unwrap(),
        byte_offset,
    )
    .unwrap()
}

fn receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            ComponentVersion::new("sanitizer.projection-test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

fn observation(
    session_id: &str,
    start: u64,
    end: u64,
    receipt_id: &str,
    payload: Value,
) -> DurableClaudeObservationV1 {
    observation_in_generation(session_id, GENERATION, start, end, receipt_id, payload)
}

fn observation_in_generation(
    session_id: &str,
    generation: u64,
    start: u64,
    end: u64,
    receipt_id: &str,
    payload: Value,
) -> DurableClaudeObservationV1 {
    DurableClaudeObservationV1::new(
        ClaudeObservationIdentityMaterialV1::new(
            source(session_id),
            ObservationScopeV1::Profile,
            ClaudeFileGenerationV1::new(generation).unwrap(),
            ClaudeByteRangeV1::new(start, end).unwrap(),
        )
        .unwrap(),
        receipt(receipt_id, &payload),
        RetentionClass::new("retention.projection-test").unwrap(),
        payload,
    )
    .unwrap()
}

fn canonical_observation(provider: &str, ordinal: u64) -> DurableObservationV1 {
    canonical_observation_at(
        provider,
        ordinal,
        0,
        1,
        &format!("{provider} convergence canary"),
    )
}

fn canonical_observation_at(
    provider: &str,
    ordinal: u64,
    start: u64,
    end: u64,
    text: &str,
) -> DurableObservationV1 {
    let provider_id = ProviderId::new(provider).unwrap();
    let session_id = SessionId::new(format!("session.projection-{provider}")).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider_id.clone(), session_id.clone()).unwrap();
    let generation = ObservationSourceGenerationV1::new(1).unwrap();
    let range = ObservationSourceRangeV1::new(start, end).unwrap();
    let record_id = ObservationId::new(format!("record.projection-{provider}.{ordinal}")).unwrap();
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider_id,
        "message",
        record_id.clone(),
        CanonicalObservationRelationsV1::new(session_id),
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": text}),
            model: Some("model.fixture".to_owned()),
            timestamp: Some(1_750_000_000 + i64::try_from(ordinal).unwrap()),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        generation,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        record_id,
    )
    .unwrap();

    DurableObservationV1::new(
        identity,
        receipt(
            &format!("receipt.projection-{provider}.{ordinal}"),
            &payload,
        ),
        RetentionClass::new("retention.projection-test").unwrap(),
        payload,
    )
    .unwrap()
}

fn canonical_write(observation: DurableObservationV1) -> AnchoredObservationWrite {
    canonical_write_with_cursor(observation, None)
}

fn canonical_write_with_cursor(
    observation: DurableObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> AnchoredObservationWrite {
    let identity = observation.identity();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .unwrap();
    anchored_write(ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap())
}

fn anchored_write(write: ObservationWrite) -> AnchoredObservationWrite {
    let generation = ProjectionGenerationId::new("projection.observation-test.v4").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "projection-test")
            .unwrap();
    let anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, anchor, generation).unwrap()
}

fn write(
    observation: DurableClaudeObservationV1,
    expected_cursor: Option<ClaudeSourceCursorV1>,
) -> AnchoredObservationWrite {
    let next_cursor = cursor_in_generation(
        observation.source().session_id().as_str(),
        observation.identity().generation().file_id(),
        observation.identity().position().end(),
    );
    anchored_write(ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap())
}

async fn persist(
    store: &GlobalDbObservationStore<'_>,
    observation: DurableClaudeObservationV1,
    expected_cursor: Option<ClaudeSourceCursorV1>,
) -> u64 {
    match store
        .persist_observation(write(observation, expected_cursor))
        .await
        .unwrap()
    {
        ObservationPersistOutcome::Committed(receipt) => receipt.sequence(),
        other => panic!("new observation must commit, got {other:?}"),
    }
}

async fn drain_projection_queue(store: &GlobalDbObservationStore<'_>) {
    while let Some(observation_id) = store.next_queued_observation().await.unwrap() {
        store.project_observation(&observation_id).await.unwrap();
    }
}

async fn rebuild_projection_to_completion(
    store: &GlobalDbObservationStore<'_>,
    frontier: u64,
) -> ProjectionRebuildOutcome {
    for _ in 0..32 {
        let outcome = store.rebuild_projection(frontier).await.unwrap();
        if outcome.is_complete() {
            return outcome;
        }
    }
    panic!("projection rebuild did not complete within the bounded test budget");
}

fn conversational_payload(message_id: &str, text: &str) -> Value {
    json!({
        "type": "assistant",
        "uuid": format!("record-{message_id}"),
        "timestamp": "2025-06-15T15:06:40Z",
        "message": {
            "id": message_id,
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "model": "claude-sonnet-4"
        }
    })
}

async fn table_count(tmp: &TempDir, table: &str) -> i64 {
    let db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    let quoted = table.replace('"', "\"\"");
    let mut rows = conn
        .query(&format!("SELECT COUNT(*) FROM \"{quoted}\""), ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn reinstall_projection_provenance_schema(tmp: &TempDir, extra_column: &str) {
    reinstall_projection_provenance_schema_with_options(tmp, extra_column, "").await;
}

async fn reinstall_projection_provenance_schema_with_options(
    tmp: &TempDir,
    extra_column: &str,
    table_options: &str,
) {
    let db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch(&format!(
        "BEGIN IMMEDIATE;
         DROP TRIGGER IF EXISTS projection_output_audit_invalidate_update_v1;
         DROP TRIGGER IF EXISTS projection_output_audit_invalidate_delete_v1;
         CREATE TABLE observation_projection_provenance_legacy (
            projector_version TEXT NOT NULL,
            observation_id TEXT NOT NULL,
            receipt_id TEXT NOT NULL,
            output_provider TEXT NOT NULL,
            output_message_id TEXT NOT NULL,
            output_digest TEXT NOT NULL,
            message_created INTEGER NOT NULL CHECK(message_created IN (0, 1)),
            {extra_column}
            PRIMARY KEY(projector_version, observation_id),
            UNIQUE(projector_version, output_provider, output_message_id),
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
         ) {table_options};
         INSERT INTO observation_projection_provenance_legacy
            (projector_version, observation_id, receipt_id, output_provider,
             output_message_id, output_digest, message_created)
         SELECT projector_version, observation_id, receipt_id, output_provider,
                output_message_id, output_digest, message_created
         FROM observation_projection_provenance;
         DROP TABLE observation_projection_provenance;
         ALTER TABLE observation_projection_provenance_legacy
            RENAME TO observation_projection_provenance;
         COMMIT;"
    ))
    .await
    .unwrap();
}

async fn reinstall_legacy_projection_provenance_schema(tmp: &TempDir) {
    reinstall_projection_provenance_schema(tmp, "").await;
}

async fn add_other_projector_owner(tmp: &TempDir, observation_id: &CanonicalObservationIdV1) {
    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute(
            "INSERT INTO observation_projection_provenance (
                projector_version, observation_id, receipt_id, output_provider,
                output_message_id, output_digest, message_created
             ) SELECT 'test-projector-v2', observation_id, receipt_id, output_provider,
                      output_message_id, output_digest, 0
               FROM observation_projection_provenance
               WHERE projector_version = ?1 AND observation_id = ?2",
            libsql::params![
                CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                observation_id.as_str(),
            ],
        )
        .await
        .unwrap();
}

async fn audited_projection_fixture(session_id: &str, message_id: &str) -> TempDir {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(
        session_id,
        0,
        100,
        &format!("receipt.{message_id}"),
        conversational_payload(message_id, "audited projection body"),
    );
    persist(&store, candidate, None).await;
    drain_projection_queue(&store).await;
    drop(db);

    let audited = GlobalDb::open_at(&isolated_lcm_db_path(&tmp))
        .await
        .expect("projected authority must pass its exhaustive audit");
    drop(audited);
    tmp
}

async fn projection_counts(tmp: &TempDir) -> (i64, i64, i64, i64, i64, i64) {
    (
        table_count(tmp, "sessions").await,
        table_count(tmp, "session_messages").await,
        table_count(tmp, "observation_projection_provenance").await,
        table_count(tmp, "observation_projection_checkpoints").await,
        table_count(tmp, "observation_projection_dispositions").await,
        table_count(tmp, "projection_queue").await,
    )
}

async fn projection_provenance_rows(
    tmp: &TempDir,
) -> Vec<(String, String, String, String, String, String, String)> {
    let db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT projector_version, observation_id, retrieval_anchor_id, receipt_id,
                    output_provider, output_message_id, output_digest
             FROM observation_projection_provenance
             ORDER BY observation_id",
            (),
        )
        .await
        .unwrap();
    let mut provenance = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        provenance.push((
            row.get(0).unwrap(),
            row.get(1).unwrap(),
            row.get(2).unwrap(),
            row.get(3).unwrap(),
            row.get(4).unwrap(),
            row.get(5).unwrap(),
            row.get(6).unwrap(),
        ));
    }
    provenance
}

async fn projected_message_texts(tmp: &TempDir) -> Vec<String> {
    projected_message_texts_where(tmp, "WHERE provider = 'claude'").await
}

async fn projected_raw_store_ids(tmp: &TempDir) -> Vec<(String, i64)> {
    let db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT message_id, store_id FROM lcm_raw_messages
             WHERE provider = 'claude' ORDER BY message_id",
            (),
        )
        .await
        .unwrap();
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        ids.push((row.get(0).unwrap(), row.get(1).unwrap()));
    }
    ids
}

async fn all_projected_message_texts(tmp: &TempDir) -> Vec<String> {
    projected_message_texts_where(tmp, "").await
}

async fn projected_message_texts_where(tmp: &TempDir, predicate: &str) -> Vec<String> {
    let db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    let sql = format!("SELECT text FROM session_messages {predicate} ORDER BY message_id");
    let mut rows = conn.query(&sql, ()).await.unwrap();
    let mut texts = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        texts.push(row.get(0).unwrap());
    }
    texts
}

async fn projection_ownership_rows(tmp: &TempDir) -> Vec<i64> {
    let db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT message_created
             FROM observation_projection_provenance ORDER BY observation_id",
            (),
        )
        .await
        .unwrap();
    let mut ownership = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        ownership.push(row.get(0).unwrap());
    }
    ownership
}

fn projection_output_ids(
    rows: &[(String, String, String, String, String, String, String)],
) -> Vec<String> {
    let mut ids = rows.iter().map(|row| row.5.clone()).collect::<Vec<_>>();
    ids.sort();
    ids
}

mod adoption;
mod failure_audit;
mod message_ids;
mod provenance_schema;
mod queue;
mod rebuild;
