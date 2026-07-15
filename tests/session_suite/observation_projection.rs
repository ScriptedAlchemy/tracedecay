use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::global_db::GlobalDb;
use tracedecay::store::GlobalDbObservationStore;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationIdV1, CanonicalObservationRelationsV1,
    ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1,
    ClaudeSourceCursorV1, ClaudeSourceIdentityV1, ComponentVersion, DurableClaudeObservationV1,
    DurableObservationV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadDigestV1, PayloadReferenceV1, ProviderId, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    SessionId,
};
use tracedecay_store::{
    CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION, ObservationPersistOutcome,
    ObservationProjectionStatus, ObservationProjectionStore, ObservationStore, ObservationWrite,
    ProjectionPersistOutcome, ProjectionSkipReason, ProjectionStoreError,
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
    let provider_id = ProviderId::new(provider).unwrap();
    let session_id = SessionId::new(format!("session.projection-{provider}")).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider_id.clone(), session_id.clone()).unwrap();
    let generation = ObservationSourceGenerationV1::new(1).unwrap();
    let range = ObservationSourceRangeV1::new(0, 1).unwrap();
    let record_id = ObservationId::new(format!("record.projection-{provider}")).unwrap();
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider_id,
        "message",
        record_id.clone(),
        CanonicalObservationRelationsV1::new(session_id),
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": format!("{provider} convergence canary")}),
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
        receipt(&format!("receipt.projection-{provider}"), &payload),
        RetentionClass::new("retention.projection-test").unwrap(),
        payload,
    )
    .unwrap()
}

fn canonical_write(observation: DurableObservationV1) -> ObservationWrite {
    let identity = observation.identity();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .unwrap();
    ObservationWrite::new(observation, None, next_cursor).unwrap()
}

fn write(
    observation: DurableClaudeObservationV1,
    expected_cursor: Option<ClaudeSourceCursorV1>,
) -> ObservationWrite {
    let next_cursor = cursor_in_generation(
        observation.source().session_id().as_str(),
        observation.identity().generation().file_id(),
        observation.identity().position().end(),
    );
    ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap()
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
) -> Vec<(String, String, String, String, String, String)> {
    let db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT projector_version, observation_id, receipt_id, output_provider,
                    output_message_id, output_digest
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
        ));
    }
    provenance
}

async fn projected_message_texts(tmp: &TempDir) -> Vec<String> {
    projected_message_texts_where(tmp, "WHERE provider = 'claude'").await
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

fn projection_output_ids(rows: &[(String, String, String, String, String, String)]) -> Vec<String> {
    let mut ids = rows.iter().map(|row| row.4.clone()).collect::<Vec<_>>();
    ids.sort();
    ids
}

#[tokio::test]
async fn queued_projection_commits_search_effect_provenance_checkpoint_and_replay_noop() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(
        "session-atomic",
        0,
        100,
        "receipt.atomic-projection",
        json!({
            "type": "assistant",
            "uuid": "record-message-atomic",
            "timestamp": "2025-06-15T15:08:43Z",
            "message": {
                "id": "message-atomic",
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "private-reasoning-canary"},
                    {"type": "text", "text": "atomic searchable canary"},
                    {"type": "tool_use", "name": "Read", "input": {"path": "README.md"}}
                ],
                "model": "claude-sonnet-4"
            }
        }),
    );
    let sequence = persist(&store, candidate.clone(), None).await;
    assert_eq!(sequence, 1);
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        0
    );

    let projected = store
        .project_observation(candidate.observation_id())
        .await
        .unwrap();
    assert!(matches!(projected, ProjectionPersistOutcome::Projected(_)));
    assert_eq!(projected.checkpoint().last_sequence(), sequence);
    assert_eq!(
        projected.checkpoint().projector_version(),
        CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION
    );
    assert_eq!(
        store
            .get_observation(candidate.observation_id())
            .await
            .unwrap()
            .unwrap()
            .projection_status(),
        ObservationProjectionStatus::NotQueued
    );
    let hits = db
        .search_session_messages("claude", Some("user"), "atomic searchable", 10)
        .await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message.message_id, "message-atomic");
    assert_eq!(hits[0].message.role, "assistant");
    assert_eq!(hits[0].message.timestamp, Some(1_750_000_123));
    assert_eq!(hits[0].message.ordinal, 0);
    assert_eq!(hits[0].message.kind.as_deref(), Some("message"));
    assert_eq!(hits[0].message.model.as_deref(), Some("claude-sonnet-4"));
    assert_eq!(hits[0].message.tool_names.as_deref(), Some("Read"));
    assert_eq!(
        hits[0].message.source_path.as_deref(),
        Some("claude:session-atomic")
    );
    assert_eq!(hits[0].message.source_offset, Some(0));
    assert!(!hits[0].message.text.contains("private-reasoning-canary"));
    assert_eq!(projection_counts(&tmp).await, (1, 1, 1, 1, 0, 0));
    let provenance = projection_provenance_rows(&tmp).await;
    assert_eq!(provenance.len(), 1);
    assert_eq!(provenance[0].0, CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION);
    assert_eq!(provenance[0].1, candidate.observation_id().as_str());
    assert_eq!(provenance[0].2, "receipt.atomic-projection");
    assert_eq!(provenance[0].3, "claude");
    assert_eq!(provenance[0].4, "message-atomic");
    assert!(PayloadDigestV1::new(provenance[0].5.clone()).is_ok());

    let before = projection_counts(&tmp).await;
    let replay = store
        .project_observation(candidate.observation_id())
        .await
        .unwrap();
    assert!(matches!(
        replay,
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
    assert_eq!(replay.checkpoint(), projected.checkpoint());
    assert_eq!(projection_counts(&tmp).await, before);
}

#[tokio::test]
async fn safe_sanitized_uuid_remains_the_v1_message_id() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let payload = json!({
        "type": "assistant",
        "uuid": "safe-sanitized-uuid",
        "timestamp": "2025-06-15T15:06:40Z",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": "safe UUID body"}],
            "model": "claude-sonnet-4"
        }
    });
    persist(
        &store,
        observation("session-safe-uuid", 0, 100, "receipt.safe-uuid", payload),
        None,
    )
    .await;
    drain_projection_queue(&store).await;

    assert_eq!(
        projection_output_ids(&projection_provenance_rows(&tmp).await),
        ["safe-sanitized-uuid"]
    );
}

#[tokio::test]
async fn redacted_message_ids_use_injective_v1_fallbacks() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let marker = "[TraceDecay redacted:message-id]";
    let mut first = conversational_payload(marker, "first redacted message ID");
    first["uuid"] = Value::from("record-first-redacted-message-id");
    let mut second = conversational_payload(marker, "second redacted message ID");
    second["uuid"] = Value::from("record-second-redacted-message-id");
    persist(
        &store,
        observation(
            "session-redacted-message-id",
            0,
            100,
            "receipt.redacted-message-id-first",
            first,
        ),
        None,
    )
    .await;
    persist(
        &store,
        observation(
            "session-redacted-message-id",
            100,
            200,
            "receipt.redacted-message-id-second",
            second,
        ),
        Some(cursor("session-redacted-message-id", 100)),
    )
    .await;
    drain_projection_queue(&store).await;

    assert_eq!(
        projection_output_ids(&projection_provenance_rows(&tmp).await),
        [
            "session-redacted-message-id:11:0",
            "session-redacted-message-id:11:100",
        ]
    );
}

#[tokio::test]
async fn redacted_uuid_ids_use_injective_v1_fallbacks() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    for (start, end, receipt_id, text) in [
        (0, 100, "receipt.redacted-uuid-first", "first redacted UUID"),
        (
            100,
            200,
            "receipt.redacted-uuid-second",
            "second redacted UUID",
        ),
    ] {
        let payload = json!({
            "type": "assistant",
            "uuid": "[TraceDecay redacted:uuid]",
            "timestamp": "2025-06-15T15:06:40Z",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": text}],
                "model": "claude-sonnet-4"
            }
        });
        persist(
            &store,
            observation("session-redacted-uuid", start, end, receipt_id, payload),
            (start != 0).then(|| cursor("session-redacted-uuid", start)),
        )
        .await;
    }
    drain_projection_queue(&store).await;

    assert_eq!(
        projection_output_ids(&projection_provenance_rows(&tmp).await),
        ["session-redacted-uuid:11:0", "session-redacted-uuid:11:100",]
    );
}

#[tokio::test]
async fn non_conversational_observation_is_skipped_without_blocking_the_checkpoint() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let skipped = observation(
        "session-skip",
        0,
        50,
        "receipt.skip",
        json!({"type": "progress", "data": {"status": "working"}}),
    );
    let message = observation(
        "session-skip",
        50,
        100,
        "receipt.after-skip",
        conversational_payload("message-after-skip", "checkpoint advanced canary"),
    );
    persist(&store, skipped.clone(), None).await;
    persist(&store, message.clone(), Some(cursor("session-skip", 50))).await;

    let outcome = store
        .project_observation(skipped.observation_id())
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        ProjectionPersistOutcome::Skipped {
            reason: ProjectionSkipReason::NonConversationalRecord,
            ..
        }
    ));
    assert_eq!(outcome.checkpoint().last_sequence(), 1);
    assert_eq!(projection_counts(&tmp).await, (0, 0, 0, 1, 1, 1));
    assert_eq!(
        store.next_queued_observation().await.unwrap().as_ref(),
        Some(message.observation_id())
    );

    store
        .project_observation(message.observation_id())
        .await
        .unwrap();
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        2
    );
    assert_eq!(projection_counts(&tmp).await, (1, 1, 1, 1, 1, 0));
    assert!(matches!(
        store
            .project_observation(skipped.observation_id())
            .await
            .unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
    let rebuilt = store.rebuild_projection(2).await.unwrap();
    assert_eq!(rebuilt.projected_rows(), 1);
    assert_eq!(rebuilt.skipped_observations(), 1);
    assert_eq!(projection_counts(&tmp).await, (1, 1, 1, 1, 1, 0));
}

#[tokio::test]
async fn bounded_next_queue_item_resumes_after_restart_and_drains_idempotently() {
    let tmp = TempDir::new().unwrap();
    let first = observation(
        "session-restart",
        0,
        100,
        "receipt.restart-1",
        conversational_payload("message-restart-1", "restart first canary"),
    );
    let second = observation(
        "session-restart",
        100,
        200,
        "receipt.restart-2",
        conversational_payload("message-restart-2", "restart second canary"),
    );

    {
        let db = open_lcm_db(&tmp).await;
        let store = GlobalDbObservationStore::new(&db);
        persist(&store, first.clone(), None).await;
        persist(&store, second.clone(), Some(cursor("session-restart", 100))).await;
        assert_eq!(
            store.next_queued_observation().await.unwrap().as_ref(),
            Some(first.observation_id())
        );
        store
            .project_observation(first.observation_id())
            .await
            .unwrap();
    }

    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        1
    );
    assert_eq!(
        store.next_queued_observation().await.unwrap().as_ref(),
        Some(second.observation_id())
    );
    store
        .project_observation(second.observation_id())
        .await
        .unwrap();
    assert!(store.next_queued_observation().await.unwrap().is_none());
    assert!(matches!(
        store
            .project_observation(second.observation_id())
            .await
            .unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
    assert_eq!(projection_counts(&tmp).await, (1, 2, 2, 1, 0, 0));
}

#[tokio::test]
async fn stale_exact_duplicate_queue_item_is_consumed_before_later_observation() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let first = observation(
        "session-stale-queue",
        0,
        100,
        "receipt.stale-queue-1",
        conversational_payload("message-stale-queue-1", "stale queue first canary"),
    );
    let second = observation(
        "session-stale-queue",
        100,
        200,
        "receipt.stale-queue-2",
        conversational_payload("message-stale-queue-2", "stale queue second canary"),
    );
    persist(&store, first.clone(), None).await;
    persist(
        &store,
        second.clone(),
        Some(cursor("session-stale-queue", 100)),
    )
    .await;
    store
        .project_observation(first.observation_id())
        .await
        .unwrap();

    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    raw_db
        .connect()
        .unwrap()
        .execute(
            "INSERT INTO projection_queue (observation_id, observation_sequence)
             VALUES (?1, 1)",
            libsql::params![first.observation_id().as_str()],
        )
        .await
        .unwrap();

    assert_eq!(
        store.next_queued_observation().await.unwrap().as_ref(),
        Some(first.observation_id())
    );
    assert!(matches!(
        store
            .project_observation(first.observation_id())
            .await
            .unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
    assert_eq!(
        store.next_queued_observation().await.unwrap().as_ref(),
        Some(second.observation_id())
    );

    drain_projection_queue(&store).await;
    assert!(store.next_queued_observation().await.unwrap().is_none());
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        2
    );
    assert_eq!(projection_counts(&tmp).await, (1, 2, 2, 1, 0, 0));
}

#[tokio::test]
async fn exact_v1_message_is_adopted_and_richer_session_survives_rebuild() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(
        "session-v1",
        0,
        100,
        "receipt.v1",
        conversational_payload("message-v1", "v1 parity canary"),
    );
    persist(&store, candidate.clone(), None).await;

    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "INSERT INTO sessions
            (provider, session_id, project_key, project_path, title, is_subagent)
         VALUES (?1, ?2, 'legacy-project', '/legacy/project', 'V1 title', 0)",
        libsql::params!["claude", "session-v1"],
    )
    .await
    .unwrap();
    let metadata_json = serde_json::to_string(&json!({
        "source": "claude_transcript",
        "raw_type": "assistant",
        "source_generation": GENERATION,
    }))
    .unwrap();
    conn.execute(
        "INSERT INTO session_messages
            (provider, message_id, session_id, role, timestamp, ordinal, text, kind, model,
             tool_names, source_path, source_offset, metadata_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        libsql::params![
            "claude",
            "message-v1",
            "session-v1",
            "assistant",
            1_750_000_000_i64,
            0_i64,
            serde_json::to_string(&json!([{"type": "text", "text": "v1 parity canary"}])).unwrap(),
            "message",
            "claude-sonnet-4",
            Option::<String>::None,
            "claude:session-v1",
            0_i64,
            metadata_json,
        ],
    )
    .await
    .unwrap();

    store
        .project_observation(candidate.observation_id())
        .await
        .unwrap();
    assert_eq!(projection_ownership_rows(&tmp).await, vec![0]);
    assert_eq!(projection_counts(&tmp).await, (1, 1, 1, 1, 0, 0));

    store.rebuild_projection(0).await.unwrap();
    assert_eq!(projection_counts(&tmp).await, (1, 1, 0, 1, 0, 1));
    store.rebuild_projection(1).await.unwrap();
    assert_eq!(projection_ownership_rows(&tmp).await, vec![0]);
    assert_eq!(projection_counts(&tmp).await, (1, 1, 1, 1, 0, 0));
    let mut rows = conn
        .query(
            "SELECT project_key, project_path, title FROM sessions
             WHERE provider = 'claude' AND session_id = 'session-v1'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "legacy-project");
    assert_eq!(row.get::<String>(1).unwrap(), "/legacy/project");
    assert_eq!(row.get::<String>(2).unwrap(), "V1 title");
}

#[tokio::test]
async fn adopted_message_is_not_mutated_by_rollover_and_rebuilds_cleanly() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let original = observation_in_generation(
        "session-adopted-rollover",
        GENERATION,
        0,
        100,
        "receipt.adopted-rollover-1",
        conversational_payload("message-adopted-rollover", "adopted original canary"),
    );
    let replacement = observation_in_generation(
        "session-adopted-rollover",
        GENERATION + 1,
        0,
        100,
        "receipt.adopted-rollover-2",
        conversational_payload(
            "message-adopted-rollover",
            "adopted replacement must not appear",
        ),
    );
    persist(&store, original.clone(), None).await;
    persist(
        &store,
        replacement.clone(),
        Some(cursor_in_generation(
            "session-adopted-rollover",
            GENERATION,
            100,
        )),
    )
    .await;

    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "INSERT INTO sessions
            (provider, session_id, project_key, project_path, is_subagent)
         VALUES ('claude', 'session-adopted-rollover', 'user', 'user', 0)",
        (),
    )
    .await
    .unwrap();
    let metadata_json = serde_json::to_string(&json!({
        "source": "claude_transcript",
        "raw_type": "assistant",
        "source_generation": GENERATION,
    }))
    .unwrap();
    conn.execute(
        "INSERT INTO session_messages
            (provider, message_id, session_id, role, timestamp, ordinal, text, kind, model,
             tool_names, source_path, source_offset, metadata_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        libsql::params![
            "claude",
            "message-adopted-rollover",
            "session-adopted-rollover",
            "assistant",
            1_750_000_000_i64,
            0_i64,
            serde_json::to_string(&json!([{"type": "text", "text": "adopted original canary"}]))
                .unwrap(),
            "message",
            "claude-sonnet-4",
            Option::<String>::None,
            "claude:session-adopted-rollover",
            0_i64,
            metadata_json,
        ],
    )
    .await
    .unwrap();

    drain_projection_queue(&store).await;
    assert_eq!(projection_ownership_rows(&tmp).await, vec![0, 0]);
    let texts = projected_message_texts(&tmp).await;
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("adopted original canary"));
    assert!(!texts[0].contains("adopted replacement must not appear"));
    assert!(matches!(
        store
            .project_observation(replacement.observation_id())
            .await
            .unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));

    store.rebuild_projection(0).await.unwrap();
    assert_eq!(projection_counts(&tmp).await, (1, 1, 0, 1, 0, 2));
    drain_projection_queue(&store).await;
    assert_eq!(projection_counts(&tmp).await, (1, 1, 2, 1, 0, 0));
    assert_eq!(projection_ownership_rows(&tmp).await, vec![0, 0]);
    let texts = projected_message_texts(&tmp).await;
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("adopted original canary"));
    assert!(!texts[0].contains("adopted replacement must not appear"));
}

#[tokio::test]
async fn projection_failure_rolls_back_effect_fts_provenance_checkpoint_and_queue() {
    for (stage, trigger) in [
        ("message", "BEFORE INSERT ON session_messages"),
        (
            "provenance",
            "BEFORE INSERT ON observation_projection_provenance",
        ),
        ("dequeue", "BEFORE DELETE ON projection_queue"),
        (
            "checkpoint",
            "BEFORE INSERT ON observation_projection_checkpoints",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let db = open_lcm_db(&tmp).await;
        let store = GlobalDbObservationStore::new(&db);
        let message_id = format!("message-rollback-{stage}");
        let searchable = format!("rollback searchable {stage}");
        let candidate = observation(
            &format!("session-rollback-{stage}"),
            0,
            100,
            &format!("receipt.rollback-{stage}"),
            conversational_payload(&message_id, &searchable),
        );
        persist(&store, candidate.clone(), None).await;

        let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
            .build()
            .await
            .unwrap();
        let raw_conn = raw_db.connect().unwrap();
        raw_conn
            .execute_batch(&format!(
                "CREATE TRIGGER fail_projection_{stage}
                 {trigger} BEGIN
                    SELECT RAISE(ABORT, 'injected projection {stage} failure');
                 END;"
            ))
            .await
            .unwrap();

        let error = store
            .project_observation(candidate.observation_id())
            .await
            .expect_err("injected projection failure must abort the transaction");
        assert!(
            matches!(error, ProjectionStoreError::Storage { .. }),
            "{stage} failure surfaced as {error:?}"
        );

        drop(raw_conn);
        drop(raw_db);
        drop(db);

        let reopened_db = open_lcm_db(&tmp).await;
        let reopened_store = GlobalDbObservationStore::new(&reopened_db);
        assert_eq!(
            reopened_store
                .projection_checkpoint()
                .await
                .unwrap()
                .last_sequence(),
            0,
            "{stage} failure advanced the checkpoint"
        );
        assert_eq!(
            projection_counts(&tmp).await,
            (0, 0, 0, 0, 0, 1),
            "{stage} failure committed partial projection state"
        );
        assert_eq!(
            (
                table_count(&tmp, "sanitization_receipts").await,
                table_count(&tmp, "observations").await,
                table_count(&tmp, "source_cursors").await,
            ),
            (1, 1, 1),
            "{stage} failure changed durable ingestion rows"
        );
        assert!(
            projection_provenance_rows(&tmp).await.is_empty(),
            "{stage} failure committed projection provenance"
        );
        assert!(
            reopened_db
                .search_session_messages("claude", Some("user"), &searchable, 10)
                .await
                .is_empty(),
            "{stage} failure leaked a searchable message"
        );
        let stored = reopened_store
            .get_observation(candidate.observation_id())
            .await
            .unwrap()
            .expect("failed projection must preserve the durable observation");
        assert_eq!(
            stored.observation(),
            &candidate,
            "{stage} failure changed the durable observation"
        );
        assert_eq!(
            stored.projection_status(),
            ObservationProjectionStatus::Queued,
            "{stage} failure consumed the queue item"
        );

        let trigger_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
            .build()
            .await
            .unwrap();
        let trigger_conn = trigger_db.connect().unwrap();
        trigger_conn
            .execute_batch(&format!("DROP TRIGGER fail_projection_{stage};"))
            .await
            .unwrap();
        drop(trigger_conn);
        drop(trigger_db);
        reopened_store
            .project_observation(candidate.observation_id())
            .await
            .unwrap();
        let recovered_counts = projection_counts(&tmp).await;
        assert_eq!(recovered_counts, (1, 1, 1, 1, 0, 0));
        assert_eq!(
            reopened_db
                .search_session_messages("claude", Some("user"), &searchable, 10)
                .await
                .len(),
            1
        );
        let replay = reopened_store
            .project_observation(candidate.observation_id())
            .await
            .unwrap();
        assert!(matches!(
            replay,
            ProjectionPersistOutcome::ExactDuplicate(_)
        ));
        assert_eq!(projection_counts(&tmp).await, recovered_counts);
    }
}

#[tokio::test]
async fn divergent_output_collision_is_typed_and_rolls_back_every_projection_write() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let first = observation(
        "session-collision",
        0,
        100,
        "receipt.collision-first",
        conversational_payload("shared-message", "original collision canary"),
    );
    let second = observation(
        "session-collision",
        100,
        200,
        "receipt.collision-second",
        conversational_payload("shared-message", "divergent collision canary"),
    );
    persist(&store, first.clone(), None).await;
    persist(
        &store,
        second.clone(),
        Some(cursor("session-collision", 100)),
    )
    .await;
    store
        .project_observation(first.observation_id())
        .await
        .unwrap();
    let counts_before = projection_counts(&tmp).await;

    let error = store
        .project_observation(second.observation_id())
        .await
        .expect_err("same legacy key with different output must collide");
    assert!(matches!(
        error,
        ProjectionStoreError::OutputCollision { provider, message_id }
            if provider == "claude" && message_id == "shared-message"
    ));
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        1
    );
    assert_eq!(projection_counts(&tmp).await, counts_before);
    assert_eq!(
        store
            .get_observation(second.observation_id())
            .await
            .unwrap()
            .unwrap()
            .projection_status(),
        ObservationProjectionStatus::Queued
    );
    assert_eq!(
        db.search_session_messages("claude", Some("user"), "original collision", 10)
            .await
            .len(),
        1
    );
    let texts = projected_message_texts(&tmp).await;
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("original collision canary"));
    assert!(!texts[0].contains("divergent collision canary"));

    let rebuild_error = store
        .rebuild_projection(2)
        .await
        .expect_err("rebuild must preserve divergent-output collision semantics");
    assert!(matches!(
        rebuild_error,
        ProjectionStoreError::OutputCollision { provider, message_id }
            if provider == "claude" && message_id == "shared-message"
    ));
    assert_eq!(projection_counts(&tmp).await, counts_before);
}

#[tokio::test]
async fn reused_message_id_across_sources_collides_without_consuming_queue() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let first = observation_in_generation(
        "session-reused-id-a",
        GENERATION,
        0,
        100,
        "receipt.reused-id-a",
        conversational_payload("shared-cross-source", "cross-source original canary"),
    );
    let second = observation_in_generation(
        "session-reused-id-b",
        GENERATION + 1,
        0,
        100,
        "receipt.reused-id-b",
        conversational_payload("shared-cross-source", "cross-source replacement canary"),
    );
    persist(&store, first.clone(), None).await;
    persist(&store, second.clone(), None).await;
    store
        .project_observation(first.observation_id())
        .await
        .unwrap();
    let counts_before = projection_counts(&tmp).await;
    let provenance_before = projection_provenance_rows(&tmp).await;
    let texts_before = projected_message_texts(&tmp).await;
    let checkpoint_before = store.projection_checkpoint().await.unwrap();

    let error = store
        .project_observation(second.observation_id())
        .await
        .expect_err("a reused message ID from another typed source must collide");
    assert!(matches!(
        error,
        ProjectionStoreError::OutputCollision { provider, message_id }
            if provider == "claude" && message_id == "shared-cross-source"
    ));
    assert_eq!(
        store.projection_checkpoint().await.unwrap(),
        checkpoint_before
    );
    assert_eq!(projection_counts(&tmp).await, counts_before);
    assert_eq!(projection_provenance_rows(&tmp).await, provenance_before);
    assert_eq!(projected_message_texts(&tmp).await, texts_before);
    assert_eq!(texts_before.len(), 1);
    assert!(texts_before[0].contains("cross-source original canary"));
    assert_eq!(
        store
            .get_observation(second.observation_id())
            .await
            .unwrap()
            .unwrap()
            .projection_status(),
        ObservationProjectionStatus::Queued
    );
}

#[tokio::test]
async fn reordered_delivery_then_frozen_frontier_rebuild_converges() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let observations = [
        observation(
            "session-rebuild",
            0,
            100,
            "receipt.rebuild-1",
            conversational_payload("message-rebuild-1", "frozen frontier alpha"),
        ),
        observation(
            "session-rebuild",
            100,
            200,
            "receipt.rebuild-2",
            conversational_payload("message-rebuild-2", "frozen frontier beta"),
        ),
        observation(
            "session-rebuild",
            200,
            300,
            "receipt.rebuild-3",
            conversational_payload("message-rebuild-3", "past frontier gamma"),
        ),
    ];
    let mut expected_cursor = None;
    for candidate in &observations {
        persist(&store, candidate.clone(), expected_cursor.clone()).await;
        expected_cursor = Some(cursor(
            "session-rebuild",
            candidate.identity().position().end(),
        ));
    }

    let error = store
        .project_observation(observations[1].observation_id())
        .await
        .expect_err("out-of-order projection must not skip the checkpoint frontier");
    assert!(matches!(
        error,
        ProjectionStoreError::Gap {
            expected: 1,
            actual: 2
        }
    ));
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        0
    );
    assert_eq!(projection_counts(&tmp).await, (0, 0, 0, 0, 0, 3));

    for candidate in &observations[..2] {
        store
            .project_observation(candidate.observation_id())
            .await
            .unwrap();
    }
    let mut rows_before = db
        .search_session_messages("claude", Some("user"), "frozen frontier", 10)
        .await
        .into_iter()
        .map(|hit| (hit.message.message_id, hit.message.text))
        .collect::<Vec<_>>();
    rows_before.sort();
    let counts_before = projection_counts(&tmp).await;
    let provenance_before = projection_provenance_rows(&tmp).await;
    assert_eq!(counts_before, (1, 2, 2, 1, 0, 1));

    let rebuilt = store.rebuild_projection(2).await.unwrap();
    assert_eq!(rebuilt.checkpoint().last_sequence(), 2);
    assert_eq!(rebuilt.projected_rows(), 2);
    let mut rows_after = db
        .search_session_messages("claude", Some("user"), "frozen frontier", 10)
        .await
        .into_iter()
        .map(|hit| (hit.message.message_id, hit.message.text))
        .collect::<Vec<_>>();
    rows_after.sort();
    assert_eq!(rows_after, rows_before);
    assert_eq!(projection_provenance_rows(&tmp).await, provenance_before);
    assert_eq!(projection_counts(&tmp).await, counts_before);
    assert!(
        projected_message_texts(&tmp)
            .await
            .iter()
            .all(|text| !text.contains("past frontier gamma"))
    );
    assert_eq!(
        store
            .get_observation(observations[2].observation_id())
            .await
            .unwrap()
            .unwrap()
            .projection_status(),
        ObservationProjectionStatus::Queued
    );

    let final_outcome = store
        .project_observation(observations[2].observation_id())
        .await
        .unwrap();
    assert_eq!(final_outcome.checkpoint().last_sequence(), 3);
    let texts = projected_message_texts(&tmp).await;
    assert_eq!(texts.len(), 3);
    assert_eq!(
        texts
            .iter()
            .filter(|text| text.contains("past frontier gamma"))
            .count(),
        1
    );
    let incrementally_projected_texts = texts;
    let incremental_provenance = projection_provenance_rows(&tmp).await;
    let incremental_ownership = projection_ownership_rows(&tmp).await;
    let incremental_output_ids = projection_output_ids(&incremental_provenance);

    let rebuilt_empty = store.rebuild_projection(0).await.unwrap();
    assert_eq!(rebuilt_empty.projected_rows(), 0);
    assert_eq!(rebuilt_empty.skipped_observations(), 0);
    assert_eq!(projection_counts(&tmp).await, (1, 0, 0, 1, 0, 3));

    let rebuilt_full = store.rebuild_projection(3).await.unwrap();
    assert_eq!(rebuilt_full.projected_rows(), 3);
    assert_eq!(rebuilt_full.skipped_observations(), 0);
    assert_eq!(projection_counts(&tmp).await, (1, 3, 3, 1, 0, 0));
    assert_eq!(
        projected_message_texts(&tmp).await,
        incrementally_projected_texts
    );
    assert_eq!(
        projection_provenance_rows(&tmp).await,
        incremental_provenance
    );
    assert_eq!(projection_ownership_rows(&tmp).await, incremental_ownership);
    let rebuilt_provenance = projection_provenance_rows(&tmp).await;
    assert_eq!(
        projection_output_ids(&rebuilt_provenance),
        incremental_output_ids
    );
}

#[tokio::test]
async fn canonical_provider_incremental_and_rebuild_projection_converge() {
    const PROVIDERS: [&str; 7] = [
        "codex", "cursor", "hermes", "kiro", "cline", "roo-code", "kilo",
    ];

    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);

    for (index, provider) in PROVIDERS.into_iter().enumerate() {
        let observation = canonical_observation(provider, index as u64 + 1);
        let outcome = store
            .persist_observation(canonical_write(observation))
            .await
            .unwrap();
        assert!(matches!(outcome, ObservationPersistOutcome::Committed(_)));
    }
    while let Some(observation_id) = store.next_queued_observation().await.unwrap() {
        assert!(matches!(
            store.project_observation(&observation_id).await.unwrap(),
            ProjectionPersistOutcome::Projected(_)
        ));
    }

    let incremental_texts = all_projected_message_texts(&tmp).await;
    assert_eq!(incremental_texts.len(), PROVIDERS.len());
    for provider in PROVIDERS {
        assert!(
            incremental_texts
                .iter()
                .any(|text| text == &format!("{provider} convergence canary")),
            "missing projected {provider} message"
        );
    }
    let incremental_provenance = projection_provenance_rows(&tmp).await;
    let incremental_ownership = projection_ownership_rows(&tmp).await;
    let incremental_output_ids = projection_output_ids(&incremental_provenance);

    let cleared = store.rebuild_projection(0).await.unwrap();
    assert_eq!(cleared.projected_rows(), 0);
    let rebuilt = store
        .rebuild_projection(PROVIDERS.len() as u64)
        .await
        .unwrap();
    assert_eq!(rebuilt.projected_rows(), PROVIDERS.len());
    assert_eq!(rebuilt.skipped_observations(), 0);
    assert_eq!(all_projected_message_texts(&tmp).await, incremental_texts);
    let rebuilt_provenance = projection_provenance_rows(&tmp).await;
    assert_eq!(rebuilt_provenance, incremental_provenance);
    assert_eq!(projection_ownership_rows(&tmp).await, incremental_ownership);
    assert_eq!(
        projection_output_ids(&rebuilt_provenance),
        incremental_output_ids
    );
}

#[tokio::test]
async fn generation_rollover_coalesces_same_and_changed_native_output() {
    let tmp = TempDir::new().unwrap();
    let first = observation_in_generation(
        "session-generation",
        GENERATION,
        0,
        100,
        "receipt.generation-1",
        conversational_payload("message-generation", "generation original canary"),
    );
    let same_content = observation_in_generation(
        "session-generation",
        GENERATION + 1,
        0,
        100,
        "receipt.generation-2",
        conversational_payload("message-generation", "generation original canary"),
    );
    let replacement = observation_in_generation(
        "session-generation",
        GENERATION + 2,
        0,
        100,
        "receipt.generation-3",
        conversational_payload("message-generation", "generation replacement canary"),
    );

    {
        let db = open_lcm_db(&tmp).await;
        let store = GlobalDbObservationStore::new(&db);
        persist(&store, first.clone(), None).await;
        store
            .project_observation(first.observation_id())
            .await
            .unwrap();
    }
    reinstall_legacy_projection_provenance_schema(&tmp).await;

    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    persist(
        &store,
        same_content.clone(),
        Some(cursor_in_generation("session-generation", GENERATION, 100)),
    )
    .await;
    persist(
        &store,
        replacement.clone(),
        Some(cursor_in_generation(
            "session-generation",
            GENERATION + 1,
            100,
        )),
    )
    .await;
    drain_projection_queue(&store).await;

    assert_eq!(projection_counts(&tmp).await, (1, 1, 3, 1, 0, 0));
    let provenance = projection_provenance_rows(&tmp).await;
    assert_eq!(provenance.len(), 3);
    assert!(provenance.iter().all(|row| row.4 == "message-generation"));
    assert_eq!(
        provenance
            .iter()
            .map(|row| row.5.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "generation-owned metadata and replacement content retain distinct lineage digests"
    );
    let texts = projected_message_texts(&tmp).await;
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("generation replacement canary"));

    for candidate in [&first, &same_content, &replacement] {
        assert!(matches!(
            store
                .project_observation(candidate.observation_id())
                .await
                .unwrap(),
            ProjectionPersistOutcome::ExactDuplicate(_)
        ));
    }
    store.rebuild_projection(0).await.unwrap();
    assert_eq!(projection_counts(&tmp).await, (1, 0, 0, 1, 0, 3));
    drain_projection_queue(&store).await;
    assert_eq!(projection_counts(&tmp).await, (1, 1, 3, 1, 0, 0));
    let texts = projected_message_texts(&tmp).await;
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("generation replacement canary"));
}

#[tokio::test]
async fn durable_projection_alias_survives_rebuild_without_rewriting_observation() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(
        "session-alias",
        0,
        100,
        "receipt.alias",
        conversational_payload("message-alias", "durable alias canary"),
    );
    persist(&store, candidate.clone(), None).await;

    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute(
            "INSERT INTO observation_projection_aliases
                (projector_version, observation_id, output_provider, output_message_id)
             VALUES (?1, ?2, 'claude', 'consolidated/source/message-alias')",
            libsql::params![
                CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                candidate.observation_id().as_str()
            ],
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    drain_projection_queue(&store).await;
    let provenance = projection_provenance_rows(&tmp).await;
    assert_eq!(provenance[0].4, "consolidated/source/message-alias");
    assert_eq!(table_count(&tmp, "observation_projection_aliases").await, 1);
    assert_eq!(
        store
            .get_observation(candidate.observation_id())
            .await
            .unwrap()
            .unwrap()
            .observation()
            .payload()["message"]["id"],
        "message-alias"
    );

    store.rebuild_projection(0).await.unwrap();
    assert_eq!(table_count(&tmp, "observation_projection_aliases").await, 1);
    drain_projection_queue(&store).await;
    let provenance = projection_provenance_rows(&tmp).await;
    assert_eq!(provenance[0].4, "consolidated/source/message-alias");
    assert_eq!(projected_message_texts(&tmp).await.len(), 1);
}

#[tokio::test]
async fn rebuild_preserves_output_referenced_by_another_projector_version() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(
        "session-shared-output",
        0,
        100,
        "receipt.shared-output",
        conversational_payload("message-shared-output", "shared output canary"),
    );
    persist(&store, candidate.clone(), None).await;
    drain_projection_queue(&store).await;
    add_other_projector_owner(&tmp, candidate.observation_id()).await;

    store.rebuild_projection(0).await.unwrap();
    assert_eq!(table_count(&tmp, "session_messages").await, 1);
    assert_eq!(
        table_count(&tmp, "observation_projection_provenance").await,
        2
    );

    store.rebuild_projection(1).await.unwrap();
    assert_eq!(table_count(&tmp, "session_messages").await, 1);
    assert_eq!(
        table_count(&tmp, "observation_projection_provenance").await,
        2
    );
    assert_eq!(projected_message_texts(&tmp).await.len(), 1);
}

#[tokio::test]
async fn cross_projector_owner_blocks_incompatible_generation_rollover() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let original = observation_in_generation(
        "session-global-owner",
        GENERATION,
        0,
        100,
        "receipt.global-owner-original",
        conversational_payload("message-global-owner", "global owner original"),
    );
    persist(&store, original.clone(), None).await;
    drain_projection_queue(&store).await;

    add_other_projector_owner(&tmp, original.observation_id()).await;

    let replacement = observation_in_generation(
        "session-global-owner",
        GENERATION + 1,
        0,
        100,
        "receipt.global-owner-replacement",
        conversational_payload("message-global-owner", "global owner replacement"),
    );
    persist(
        &store,
        replacement.clone(),
        Some(cursor_in_generation(
            "session-global-owner",
            GENERATION,
            100,
        )),
    )
    .await;
    assert!(matches!(
        store
            .project_observation(replacement.observation_id())
            .await
            .unwrap_err(),
        ProjectionStoreError::OutputCollision { .. }
    ));
    assert!(projected_message_texts(&tmp).await[0].contains("global owner original"));
    assert_eq!(table_count(&tmp, "projection_queue").await, 1);
}

#[tokio::test]
async fn rebuild_freezes_cross_projector_multi_generation_ownership() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let original = observation_in_generation(
        "session-retained-generations",
        GENERATION,
        0,
        100,
        "receipt.retained-generation-original",
        conversational_payload(
            "message-retained-generations",
            "retained generation original",
        ),
    );
    let replacement = observation_in_generation(
        "session-retained-generations",
        GENERATION + 1,
        0,
        100,
        "receipt.retained-generation-replacement",
        conversational_payload(
            "message-retained-generations",
            "retained generation replacement",
        ),
    );
    persist(&store, original.clone(), None).await;
    persist(
        &store,
        replacement.clone(),
        Some(cursor_in_generation(
            "session-retained-generations",
            GENERATION,
            100,
        )),
    )
    .await;
    drain_projection_queue(&store).await;

    add_other_projector_owner(&tmp, replacement.observation_id()).await;

    let rebuilt = store.rebuild_projection(1).await.unwrap();
    assert_eq!(rebuilt.projected_rows(), 1);
    assert!(projected_message_texts(&tmp).await[0].contains("retained generation replacement"));
    assert_eq!(
        table_count(&tmp, "observation_projection_provenance").await,
        3
    );
    drain_projection_queue(&store).await;
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        2
    );
    assert!(projected_message_texts(&tmp).await[0].contains("retained generation replacement"));
}

#[tokio::test]
async fn projection_owner_cache_refreshes_after_another_connection_commits() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let first = observation(
        "session-data-version",
        0,
        100,
        "receipt.data-version-first",
        conversational_payload("message-data-version-first", "data version first"),
    );
    let second = observation(
        "session-data-version",
        100,
        200,
        "receipt.data-version-second",
        conversational_payload("message-data-version-second", "data version second"),
    );
    persist(&store, first.clone(), None).await;
    store
        .project_observation(first.observation_id())
        .await
        .unwrap();
    persist(
        &store,
        second.clone(),
        Some(cursor("session-data-version", 100)),
    )
    .await;

    let other_db = GlobalDb::open_at(&isolated_lcm_db_path(&tmp))
        .await
        .unwrap();
    let other_store = GlobalDbObservationStore::new(&other_db);
    other_store
        .project_observation(second.observation_id())
        .await
        .unwrap();
    assert!(matches!(
        store
            .project_observation(second.observation_id())
            .await
            .unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
}

#[tokio::test]
async fn rebuild_processes_more_than_two_pages_at_one_frozen_frontier() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let mut expected_cursor = None;
    for index in 0..257_u64 {
        let start = index * 100;
        let end = start + 100;
        let candidate = observation(
            "session-paged-rebuild",
            start,
            end,
            &format!("receipt.paged-rebuild-{index}"),
            conversational_payload(
                &format!("message-paged-rebuild-{index}"),
                &format!("paged rebuild canary {index}"),
            ),
        );
        persist(&store, candidate, expected_cursor.clone()).await;
        expected_cursor = Some(cursor("session-paged-rebuild", end));
    }

    let rebuilt = store.rebuild_projection(257).await.unwrap();
    assert_eq!(rebuilt.projected_rows(), 257);
    assert_eq!(rebuilt.skipped_observations(), 0);
    assert_eq!(projection_counts(&tmp).await, (1, 257, 257, 1, 0, 0));
}

#[tokio::test]
async fn high_generation_output_uses_constant_size_owner_state() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let mut expected_cursor = None;
    for generation_offset in 0..257_u64 {
        let generation = GENERATION + generation_offset;
        let candidate = observation_in_generation(
            "session-high-generation",
            generation,
            0,
            100,
            &format!("receipt.high-generation-{generation}"),
            conversational_payload(
                "message-high-generation",
                &format!("high generation canary {generation}"),
            ),
        );
        persist(&store, candidate, expected_cursor.clone()).await;
        expected_cursor = Some(cursor_in_generation(
            "session-high-generation",
            generation,
            100,
        ));
    }
    drain_projection_queue(&store).await;

    assert_eq!(projection_counts(&tmp).await, (1, 1, 257, 1, 0, 0));
    assert!(projected_message_texts(&tmp).await[0].contains("high generation canary 267"));
}

#[tokio::test]
async fn authority_reopen_accepts_historical_generation_after_supersession() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let original = observation_in_generation(
        "session-authority-supersession",
        GENERATION,
        0,
        100,
        "receipt.authority-supersession-original",
        conversational_payload("message-authority-supersession", "superseded body"),
    );
    let replacement = observation_in_generation(
        "session-authority-supersession",
        GENERATION + 1,
        0,
        100,
        "receipt.authority-supersession-replacement",
        conversational_payload("message-authority-supersession", "current body"),
    );
    persist(&store, original, None).await;
    persist(
        &store,
        replacement,
        Some(cursor_in_generation(
            "session-authority-supersession",
            GENERATION,
            100,
        )),
    )
    .await;
    drain_projection_queue(&store).await;
    drop(db);

    let reopened = GlobalDb::open_at(&isolated_lcm_db_path(&tmp))
        .await
        .expect("historical provenance must validate against the current output owner");
    drop(reopened);
    assert!(projected_message_texts(&tmp).await[0].contains("current body"));
    assert_eq!(
        table_count(&tmp, "observation_projection_provenance").await,
        2
    );
}

#[tokio::test]
async fn projected_message_update_invalidates_audit_and_fails_reopen() {
    let tmp = audited_projection_fixture("session-audit-update", "message-audit-update").await;
    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute(
            "UPDATE session_messages SET text = 'tampered projection body'
             WHERE provider = 'claude' AND message_id = 'message-audit-update'",
            (),
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    assert!(
        GlobalDb::open_at(&isolated_lcm_db_path(&tmp))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn projected_message_delete_invalidates_audit_and_fails_reopen() {
    let tmp = audited_projection_fixture("session-audit-delete", "message-audit-delete").await;
    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute(
            "DELETE FROM session_messages
             WHERE provider = 'claude' AND message_id = 'message-audit-delete'",
            (),
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    assert!(
        GlobalDb::open_at(&isolated_lcm_db_path(&tmp))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn unsupported_legacy_provenance_shape_is_rejected_before_drop() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(
        "session-forward-legacy",
        0,
        100,
        "receipt.forward-legacy",
        conversational_payload("message-forward-legacy", "forward legacy canary"),
    );
    persist(&store, candidate, None).await;
    drain_projection_queue(&store).await;
    drop(db);

    reinstall_projection_provenance_schema(&tmp, "forward_owner TEXT,").await;
    assert!(
        GlobalDb::open_at(&isolated_lcm_db_path(&tmp))
            .await
            .is_none()
    );

    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    let mut columns = raw_conn
        .query(
            "SELECT name FROM pragma_table_xinfo('observation_projection_provenance')
             WHERE name = 'forward_owner'",
            (),
        )
        .await
        .unwrap();
    assert!(columns.next().await.unwrap().is_some());
    drop(columns);
    drop(raw_conn);
    drop(raw_db);
    assert_eq!(
        table_count(&tmp, "observation_projection_provenance").await,
        1
    );
    assert_eq!(table_count(&tmp, "session_messages").await, 1);
}

#[tokio::test]
async fn unsupported_legacy_provenance_table_options_are_rejected_before_drop() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    drop(db);
    reinstall_projection_provenance_schema_with_options(&tmp, "", "STRICT").await;

    assert!(
        GlobalDb::open_at(&isolated_lcm_db_path(&tmp))
            .await
            .is_none()
    );

    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    let mut rows = raw_conn
        .query(
            "SELECT strict FROM pragma_table_list
             WHERE name = 'observation_projection_provenance'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
}

#[tokio::test]
async fn supported_legacy_provenance_trigger_survives_table_replacement() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    drop(db);
    reinstall_legacy_projection_provenance_schema(&tmp).await;
    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute_batch(
            "CREATE TRIGGER projection_provenance_message_created_insert_v1
             BEFORE INSERT ON observation_projection_provenance
             WHEN NEW.message_created NOT IN (0, 1)
             BEGIN SELECT RAISE(ABORT, 'invalid projection message_created'); END;",
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    let reopened = GlobalDb::open_at(&isolated_lcm_db_path(&tmp)).await;
    assert!(reopened.is_some());
    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    let mut triggers = raw_conn
        .query(
            "SELECT 1 FROM sqlite_schema
             WHERE type = 'trigger'
               AND name = 'projection_provenance_message_created_insert_v1'",
            (),
        )
        .await
        .unwrap();
    assert!(triggers.next().await.unwrap().is_some());
}

#[tokio::test]
async fn unknown_legacy_provenance_trigger_is_rejected_before_drop() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    drop(db);
    reinstall_legacy_projection_provenance_schema(&tmp).await;
    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute_batch(
            "CREATE TRIGGER unknown_projection_provenance_trigger
             BEFORE DELETE ON observation_projection_provenance
             BEGIN SELECT RAISE(ABORT, 'must survive failed migration'); END;",
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    assert!(
        GlobalDb::open_at(&isolated_lcm_db_path(&tmp))
            .await
            .is_none()
    );
    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    let mut triggers = raw_conn
        .query(
            "SELECT 1 FROM sqlite_schema
             WHERE type = 'trigger' AND name = 'unknown_projection_provenance_trigger'",
            (),
        )
        .await
        .unwrap();
    assert!(triggers.next().await.unwrap().is_some());
}
