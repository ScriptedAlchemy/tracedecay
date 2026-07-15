use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::store::GlobalDbObservationStore;
use tracedecay_domain::{
    ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1,
    ClaudeSourceCursorV1, ClaudeSourceIdentityV1, ComponentVersion, DurableClaudeObservationV1,
    ObservationContractError, ObservationScopeV1, PayloadReferenceV1, RetentionClass,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1, SessionId,
};
use tracedecay_store::{
    CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION, ObservationPersistOutcome,
    ObservationProjectionStatus, ObservationProjectionStore, ObservationStore, ObservationWrite,
    ProjectionPersistOutcome, ProjectionStoreError, project_claude_observation,
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
        "timestamp": 1_750_000_000_i64,
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

async fn projection_counts(tmp: &TempDir) -> (i64, i64, i64, i64, i64) {
    (
        table_count(tmp, "sessions").await,
        table_count(tmp, "session_messages").await,
        table_count(tmp, "observation_projection_provenance").await,
        table_count(tmp, "observation_projection_checkpoints").await,
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

#[test]
fn pure_mapper_preserves_legacy_identity_and_stable_receipt_provenance() {
    let payload = json!({
        "type": "assistant",
        "uuid": "record-map",
        "timestamp": 1_750_000_123_i64,
        "message": {
            "id": "message-map",
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "private-reasoning-canary"},
                {"type": "text", "text": "searchable mapping canary"},
                {"type": "tool_use", "name": "Read", "input": {"path": "README.md"}}
            ],
            "model": "claude-sonnet-4"
        }
    });
    let candidate = observation("session-map", 40, 90, "receipt.map", payload);

    let first = project_claude_observation(&candidate).unwrap();
    let second = project_claude_observation(&candidate).unwrap();

    assert_eq!(first, second);
    assert_eq!(first.session().provider, "claude");
    assert_eq!(first.session().session_id, "session-map");
    assert_eq!(first.session().project_key, "user");
    assert_eq!(first.message().message_id, "message-map");
    assert_eq!(first.message().session_id, "session-map");
    assert_eq!(first.message().role, "assistant");
    assert_eq!(first.message().model.as_deref(), Some("claude-sonnet-4"));
    assert_eq!(first.message().ordinal, 40);
    assert_eq!(first.message().source_offset, Some(40));
    assert!(first.message().text.contains("searchable mapping canary"));
    assert!(!first.message().text.contains("private-reasoning-canary"));
    assert_eq!(first.message().tool_names.as_deref(), Some("Read"));
    assert_eq!(
        first.provenance().observation_id(),
        candidate.observation_id()
    );
    assert_eq!(first.provenance().receipt_id(), "receipt.map");
    assert_eq!(
        first.provenance().projector_version(),
        CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION
    );
    assert!(!first.output_digest().as_str().is_empty());

    let mismatched_payload = conversational_payload("message-map", "different payload");
    let error = DurableClaudeObservationV1::new(
        candidate.identity().clone(),
        candidate.receipt().clone(),
        candidate.retention_class().clone(),
        mismatched_payload,
    )
    .unwrap_err();
    assert_eq!(error, ObservationContractError::ReceiptPayloadMismatch);
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
        conversational_payload("message-atomic", "atomic searchable canary"),
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
    assert_eq!(projection_counts(&tmp).await, (1, 1, 1, 1, 0));
    let mapped = project_claude_observation(&candidate).unwrap();
    assert_eq!(
        projection_provenance_rows(&tmp).await,
        vec![(
            CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION.to_string(),
            candidate.observation_id().as_str().to_string(),
            "receipt.atomic-projection".to_string(),
            "claude".to_string(),
            "message-atomic".to_string(),
            mapped.output_digest().as_str().to_string(),
        )]
    );

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
    assert_eq!(projection_counts(&tmp).await, (0, 0, 0, 0, 1));
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
    assert_eq!(projection_counts(&tmp).await, (1, 1, 1, 1, 0));
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
    assert_eq!(projection_counts(&tmp).await, (0, 0, 0, 0, 3));

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
    assert_eq!(counts_before, (1, 2, 2, 1, 1));

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
}
