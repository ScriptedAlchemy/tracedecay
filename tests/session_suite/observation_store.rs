use std::collections::BTreeMap;

use serde_json::json;
use tempfile::TempDir;
use tracedecay::store::GlobalDbObservationStore;
use tracedecay_domain::{
    ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1,
    ClaudeSourceCursorV1, ClaudeSourceIdentityV1, ComponentVersion, DurableClaudeObservationV1,
    ObservationCollisionOutcomeV1, ObservationScopeV1, PayloadReferenceV1, RetentionClass,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1, SessionId,
};
use tracedecay_store::{
    ObservationPersistOutcome, ObservationProjectionStatus, ObservationReplayRequest,
    ObservationStore, ObservationStoreError, ObservationWrite,
};

use crate::common::{isolated_lcm_db_path, open_lcm_db};

const GENERATION: u64 = 7;

fn source() -> ClaudeSourceIdentityV1 {
    ClaudeSourceIdentityV1::new(SessionId::new("session.observation-store").unwrap()).unwrap()
}

fn scope() -> ObservationScopeV1 {
    ObservationScopeV1::Profile
}

fn cursor(byte_offset: u64) -> ClaudeSourceCursorV1 {
    ClaudeSourceCursorV1::new(
        source(),
        scope(),
        ClaudeFileGenerationV1::new(GENERATION).unwrap(),
        byte_offset,
    )
    .unwrap()
}

fn observation(start: u64, end: u64, receipt_id: &str, body: &str) -> DurableClaudeObservationV1 {
    let payload = json!({
        "kind": "assistant_message",
        "body": body,
    });
    let payload_reference = PayloadReferenceV1::for_payload(&payload).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            ComponentVersion::new("sanitizer.test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(payload_reference),
    )
    .unwrap();
    let identity = ClaudeObservationIdentityMaterialV1::new(
        source(),
        scope(),
        ClaudeFileGenerationV1::new(GENERATION).unwrap(),
        ClaudeByteRangeV1::new(start, end).unwrap(),
    )
    .unwrap();

    DurableClaudeObservationV1::new(
        identity,
        receipt,
        RetentionClass::new("retention.test").unwrap(),
        payload,
    )
    .unwrap()
}

fn write(
    observation: DurableClaudeObservationV1,
    expected_cursor: Option<ClaudeSourceCursorV1>,
) -> ObservationWrite {
    let next_cursor = cursor(observation.identity().position().end());
    ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap()
}

async fn user_table_counts(tmp: &TempDir) -> BTreeMap<String, i64> {
    let db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT name
             FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
            (),
        )
        .await
        .unwrap();
    let mut tables = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        tables.push(row.get::<String>(0).unwrap());
    }
    drop(rows);

    let mut counts = BTreeMap::new();
    for table in tables {
        let quoted = table.replace('"', "\"\"");
        let mut rows = conn
            .query(&format!("SELECT COUNT(*) FROM \"{quoted}\""), ())
            .await
            .unwrap();
        let count = rows.next().await.unwrap().unwrap().get(0).unwrap();
        counts.insert(table, count);
    }
    counts
}

fn table_deltas(
    before: &BTreeMap<String, i64>,
    after: &BTreeMap<String, i64>,
) -> BTreeMap<String, i64> {
    after
        .iter()
        .filter_map(|(table, after_count)| {
            let delta = after_count - before.get(table).copied().unwrap_or_default();
            (delta != 0).then(|| (table.clone(), delta))
        })
        .collect()
}

#[tokio::test]
async fn persist_commits_receipt_observation_cursor_and_one_projection_queue_row_atomically() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(0, 100, "receipt.atomic", "first sanitized payload");
    let expected_cursor = cursor(100);
    let before = user_table_counts(&tmp).await;

    let outcome = store
        .persist_observation(write(candidate.clone(), None))
        .await
        .unwrap();
    let receipt = match outcome {
        ObservationPersistOutcome::Committed(receipt) => receipt,
        other => panic!("first persistence must commit, got {other:?}"),
    };

    assert_eq!(receipt.observation(), &candidate);
    assert_eq!(receipt.sanitization_receipt(), candidate.receipt());
    assert_eq!(receipt.committed_cursor(), &expected_cursor);
    assert_eq!(
        store
            .get_source_cursor(candidate.source(), candidate.scope())
            .await
            .unwrap(),
        Some(expected_cursor.clone())
    );
    let stored = store
        .get_observation(candidate.observation_id())
        .await
        .unwrap()
        .expect("committed observation must be point-readable");
    assert_eq!(stored.sequence(), receipt.sequence());
    assert_eq!(stored.observation(), &candidate);
    assert_eq!(stored.sanitization_receipt(), candidate.receipt());
    assert_eq!(stored.committed_cursor(), &expected_cursor);
    assert_eq!(
        stored.projection_status(),
        ObservationProjectionStatus::Queued
    );

    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    assert!(
        raw_conn
            .execute(
                "UPDATE observations SET observation_json = '{}' WHERE observation_id = ?1",
                libsql::params![candidate.observation_id().as_str()],
            )
            .await
            .is_err(),
        "immutable observations must reject updates"
    );
    assert!(
        raw_conn
            .execute(
                "DELETE FROM observations WHERE observation_id = ?1",
                libsql::params![candidate.observation_id().as_str()],
            )
            .await
            .is_err(),
        "immutable observations must reject deletes"
    );

    let deltas = table_deltas(&before, &user_table_counts(&tmp).await);
    assert_eq!(
        deltas.len(),
        4,
        "one receipt, observation, cursor, and queue row must be the only committed rows: {deltas:?}"
    );
    assert!(
        deltas.values().all(|delta| *delta == 1),
        "each authoritative component must be inserted exactly once: {deltas:?}"
    );
    assert_eq!(
        deltas.get("projection_queue"),
        Some(&1),
        "the commit must enqueue exactly one unique projection job"
    );
}

#[tokio::test]
async fn exact_duplicate_returns_original_receipt_without_mutating_cursor_or_store() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(0, 100, "receipt.duplicate", "stable sanitized payload");

    let first = store
        .persist_observation(write(candidate.clone(), None))
        .await
        .unwrap();
    let original_receipt = match first {
        ObservationPersistOutcome::Committed(receipt) => receipt,
        other => panic!("first persistence must commit, got {other:?}"),
    };
    let cursor_before = store
        .get_source_cursor(candidate.source(), candidate.scope())
        .await
        .unwrap();
    let counts_before = user_table_counts(&tmp).await;

    let duplicate = store
        .persist_observation(write(candidate, None))
        .await
        .unwrap();
    let duplicate_receipt = match duplicate {
        ObservationPersistOutcome::ExactDuplicate(receipt) => receipt,
        other => panic!("exact retry must be reported as a duplicate, got {other:?}"),
    };

    assert_eq!(duplicate_receipt, original_receipt);
    assert_eq!(
        store
            .get_source_cursor(
                original_receipt.observation().source(),
                original_receipt.observation().scope(),
            )
            .await
            .unwrap(),
        cursor_before
    );
    assert_eq!(user_table_counts(&tmp).await, counts_before);
}

#[tokio::test]
async fn identity_collision_is_typed_and_leaves_all_authoritative_state_unchanged() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let original = observation(0, 100, "receipt.collision.original", "original payload");
    let colliding = observation(0, 100, "receipt.collision.candidate", "different payload");

    store
        .persist_observation(write(original.clone(), None))
        .await
        .unwrap();
    let stored_before = store
        .get_observation(original.observation_id())
        .await
        .unwrap();
    let cursor_before = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();
    let replay_before = store
        .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
        .await
        .unwrap();
    let counts_before = user_table_counts(&tmp).await;

    let error = store
        .persist_observation(write(colliding.clone(), None))
        .await
        .expect_err("same canonical identity with another payload must collide");
    match error {
        ObservationStoreError::ObservationCollision {
            observation_id,
            existing_digest,
            candidate_digest,
            outcome,
        } => {
            assert_eq!(&observation_id, original.observation_id());
            assert_eq!(&existing_digest, original.payload_reference().digest());
            assert_eq!(&candidate_digest, colliding.payload_reference().digest());
            assert_eq!(outcome, ObservationCollisionOutcomeV1::IdentityCollision);
        }
        other => panic!("expected typed observation collision, got {other:?}"),
    }

    assert_eq!(
        store
            .get_observation(original.observation_id())
            .await
            .unwrap(),
        stored_before
    );
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap(),
        cursor_before
    );
    assert_eq!(
        store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap(),
        replay_before
    );
    assert_eq!(user_table_counts(&tmp).await, counts_before);
}

#[tokio::test]
async fn duplicate_identity_with_a_different_receipt_is_rejected_without_mutation() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let original = observation(0, 100, "receipt.retry.original", "stable payload");
    let mismatched_receipt = observation(0, 100, "receipt.retry.changed", "stable payload");

    store
        .persist_observation(write(original.clone(), None))
        .await
        .unwrap();
    let stored_before = store
        .get_observation(original.observation_id())
        .await
        .unwrap();
    let cursor_before = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();
    let counts_before = user_table_counts(&tmp).await;

    let error = store
        .persist_observation(write(mismatched_receipt, None))
        .await
        .expect_err("an exact payload retry must preserve its receipt identity and policy");
    assert!(matches!(
        error,
        ObservationStoreError::SanitizationReceiptCollision
    ));
    assert_eq!(
        store
            .get_observation(original.observation_id())
            .await
            .unwrap(),
        stored_before
    );
    assert_eq!(
        store
            .get_source_cursor(original.source(), original.scope())
            .await
            .unwrap(),
        cursor_before
    );
    assert_eq!(user_table_counts(&tmp).await, counts_before);
}

#[tokio::test]
async fn every_observation_statement_failure_rolls_back_the_authoritative_transaction() {
    for (stage, table) in [
        ("receipt", "sanitization_receipts"),
        ("observation", "observations"),
        ("cursor", "source_cursors"),
        ("enqueue", "projection_queue"),
    ] {
        let tmp = TempDir::new().unwrap();
        let db = open_lcm_db(&tmp).await;
        let store = GlobalDbObservationStore::new(&db);
        let candidate = observation(
            0,
            100,
            &format!("receipt.fault.{stage}"),
            "rollback payload",
        );
        let counts_before = user_table_counts(&tmp).await;

        let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
            .build()
            .await
            .unwrap();
        let raw_conn = raw_db.connect().unwrap();
        raw_conn
            .execute_batch(&format!(
                "CREATE TRIGGER fail_observation_{stage}
                 BEFORE INSERT ON {table} BEGIN
                    SELECT RAISE(ABORT, 'injected {stage} statement failure');
                 END;"
            ))
            .await
            .unwrap();

        let error = store
            .persist_observation(write(candidate.clone(), None))
            .await
            .expect_err("the injected statement fault must fail persistence");
        assert!(
            matches!(error, ObservationStoreError::Storage { .. }),
            "{stage} fault must surface as a storage error, got {error:?}"
        );
        assert!(
            store
                .get_observation(candidate.observation_id())
                .await
                .unwrap()
                .is_none(),
            "{stage} fault leaked an observation"
        );
        assert_eq!(
            store
                .get_source_cursor(candidate.source(), candidate.scope())
                .await
                .unwrap(),
            None,
            "{stage} fault advanced the cursor"
        );
        assert!(
            store
                .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
                .await
                .unwrap()
                .is_empty(),
            "{stage} fault leaked replay state"
        );
        assert_eq!(
            user_table_counts(&tmp).await,
            counts_before,
            "{stage} fault did not roll back every table"
        );
    }
}

#[tokio::test]
async fn concurrent_exact_retry_commits_one_sequence_and_returns_one_duplicate() {
    let tmp = TempDir::new().unwrap();
    let db_left = open_lcm_db(&tmp).await;
    let db_right =
        tracedecay::global_db::GlobalDb::open_at_assuming_schema(&isolated_lcm_db_path(&tmp))
            .await
            .unwrap();
    let store_left = GlobalDbObservationStore::new(&db_left);
    let store_right = GlobalDbObservationStore::new(&db_right);
    let candidate = observation(0, 100, "receipt.concurrent", "concurrent payload");
    let counts_before = user_table_counts(&tmp).await;

    let left = store_left.persist_observation(write(candidate.clone(), None));
    let right = store_right.persist_observation(write(candidate.clone(), None));
    let (left, right) = tokio::join!(left, right);
    let outcomes = [left.unwrap(), right.unwrap()];
    let committed = outcomes
        .iter()
        .find_map(|outcome| match outcome {
            ObservationPersistOutcome::Committed(receipt) => Some(receipt),
            ObservationPersistOutcome::ExactDuplicate(_) => None,
        })
        .expect("one concurrent writer must commit");
    let duplicate = outcomes
        .iter()
        .find_map(|outcome| match outcome {
            ObservationPersistOutcome::ExactDuplicate(receipt) => Some(receipt),
            ObservationPersistOutcome::Committed(_) => None,
        })
        .expect("the other concurrent writer must observe the duplicate");

    assert_eq!(committed, duplicate);
    let replay = store_left
        .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
        .await
        .unwrap();
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].sequence(), committed.sequence());
    assert_eq!(replay[0].observation(), &candidate);
    let deltas = table_deltas(&counts_before, &user_table_counts(&tmp).await);
    assert_eq!(deltas.len(), 4);
    assert!(deltas.values().all(|delta| *delta == 1));
}

#[tokio::test]
async fn stale_exact_cas_cursor_conflict_rolls_back_every_candidate_write() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let first = observation(0, 100, "receipt.cas.first", "first payload");
    let stale_candidate = observation(100, 200, "receipt.cas.stale", "stale payload");

    store
        .persist_observation(write(first.clone(), None))
        .await
        .unwrap();
    let durable_cursor = cursor(100);
    let replay_before = store
        .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
        .await
        .unwrap();
    let counts_before = user_table_counts(&tmp).await;

    let error = store
        .persist_observation(write(stale_candidate.clone(), None))
        .await
        .expect_err("a stale exact-CAS owner must lose");
    assert!(matches!(
        error,
        ObservationStoreError::CursorConflict {
            expected: None,
            actual: Some(actual),
        } if actual == durable_cursor
    ));

    assert_eq!(
        store
            .get_source_cursor(first.source(), first.scope())
            .await
            .unwrap(),
        Some(durable_cursor)
    );
    assert!(
        store
            .get_observation(stale_candidate.observation_id())
            .await
            .unwrap()
            .is_none(),
        "the stale observation must roll back"
    );
    assert_eq!(
        store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap(),
        replay_before
    );
    assert_eq!(user_table_counts(&tmp).await, counts_before);
}

#[tokio::test]
async fn point_read_and_replay_follow_authoritative_sequence_order() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let observations = [
        observation(0, 10, "receipt.replay.1", "payload one"),
        observation(10, 20, "receipt.replay.2", "payload two"),
        observation(20, 30, "receipt.replay.3", "payload three"),
    ];
    let mut sequences = Vec::new();
    let mut expected_cursor = None;

    for candidate in &observations {
        let outcome = store
            .persist_observation(write(candidate.clone(), expected_cursor.clone()))
            .await
            .unwrap();
        let receipt = match outcome {
            ObservationPersistOutcome::Committed(receipt) => receipt,
            other => panic!("new observation must commit, got {other:?}"),
        };
        sequences.push(receipt.sequence());
        expected_cursor = Some(receipt.committed_cursor().clone());
    }

    assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    let point_read = store
        .get_observation(observations[1].observation_id())
        .await
        .unwrap()
        .expect("middle observation must be point-readable");
    assert_eq!(point_read.sequence(), sequences[1]);
    assert_eq!(point_read.observation(), &observations[1]);

    let replay = store
        .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
        .await
        .unwrap();
    assert_eq!(replay.len(), observations.len());
    assert_eq!(
        replay
            .iter()
            .map(|stored| stored.sequence())
            .collect::<Vec<_>>(),
        sequences
    );
    assert_eq!(
        replay
            .iter()
            .map(|stored| stored.observation())
            .collect::<Vec<_>>(),
        observations.iter().collect::<Vec<_>>()
    );

    let page = store
        .replay_observations(ObservationReplayRequest::new(sequences[0], 1).unwrap())
        .await
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].sequence(), sequences[1]);
    assert_eq!(page[0].observation(), &observations[1]);
}
