use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::store::GlobalDbObservationStore;
use tracedecay_domain::{
    ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1,
    ClaudeSourceCursorV1, ClaudeSourceIdentityV1, ComponentVersion, DurableClaudeObservationV1,
    ObservationScopeV1, PayloadDigestV1, PayloadReferenceV1, RetentionClass, SanitizationReceiptId,
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
    ClaudeSourceCursorV1::new(
        source(session_id),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(GENERATION).unwrap(),
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
    DurableClaudeObservationV1::new(
        ClaudeObservationIdentityMaterialV1::new(
            source(session_id),
            ObservationScopeV1::Profile,
            ClaudeFileGenerationV1::new(GENERATION).unwrap(),
            ClaudeByteRangeV1::new(start, end).unwrap(),
        )
        .unwrap(),
        receipt(receipt_id, &payload),
        RetentionClass::new("retention.projection-test").unwrap(),
        payload,
    )
    .unwrap()
}

fn write(
    observation: DurableClaudeObservationV1,
    expected_cursor: Option<ClaudeSourceCursorV1>,
) -> ObservationWrite {
    let next_cursor = cursor(
        observation.source().session_id().as_str(),
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
    let db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT text FROM session_messages WHERE provider = 'claude' ORDER BY message_id",
            (),
        )
        .await
        .unwrap();
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
async fn projection_failure_rolls_back_effect_fts_provenance_checkpoint_and_queue() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(
        "session-rollback",
        0,
        100,
        "receipt.rollback-projection",
        conversational_payload("message-rollback", "rollback searchable canary"),
    );
    persist(&store, candidate.clone(), None).await;

    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute_batch(
            "CREATE TRIGGER fail_projection_provenance
             BEFORE INSERT ON observation_projection_provenance BEGIN
                SELECT RAISE(ABORT, 'injected projection provenance failure');
             END;",
        )
        .await
        .unwrap();

    let error = store
        .project_observation(candidate.observation_id())
        .await
        .expect_err("injected projection failure must abort the transaction");
    assert!(matches!(error, ProjectionStoreError::Storage { .. }));
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        0
    );
    assert_eq!(projection_counts(&tmp).await, (0, 0, 0, 0, 0, 1));
    assert!(
        db.search_session_messages("claude", Some("user"), "rollback searchable", 10)
            .await
            .is_empty()
    );
    assert_eq!(
        store
            .get_observation(candidate.observation_id())
            .await
            .unwrap()
            .unwrap()
            .projection_status(),
        ObservationProjectionStatus::Queued
    );

    raw_conn
        .execute_batch("DROP TRIGGER fail_projection_provenance;")
        .await
        .unwrap();
    store
        .project_observation(candidate.observation_id())
        .await
        .unwrap();
    assert_eq!(projection_counts(&tmp).await, (1, 1, 1, 1, 0, 0));
    assert_eq!(
        db.search_session_messages("claude", Some("user"), "rollback searchable", 10)
            .await
            .len(),
        1
    );
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

    let rebuilt_empty = store.rebuild_projection(0).await.unwrap();
    assert_eq!(rebuilt_empty.projected_rows(), 0);
    assert_eq!(rebuilt_empty.skipped_observations(), 0);
    assert_eq!(projection_counts(&tmp).await, (1, 0, 0, 1, 0, 3));

    let rebuilt_full = store.rebuild_projection(3).await.unwrap();
    assert_eq!(rebuilt_full.projected_rows(), 3);
    assert_eq!(rebuilt_full.skipped_observations(), 0);
    assert_eq!(projection_counts(&tmp).await, (1, 3, 3, 1, 0, 0));
}
