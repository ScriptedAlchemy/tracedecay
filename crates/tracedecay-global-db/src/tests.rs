#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use super::{AnalyticsEventInsert, ParseOffset, RegisteredGlobalDb};

pub mod harness;

#[doc(hidden)]
pub fn registered_schema_fixture_fingerprint() -> String {
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    for source in [
        include_str!("schema_stages.rs"),
        include_str!("schema_contract/definitions.rs"),
        include_str!("schema_contract/invariants/triggers.rs"),
    ] {
        digest.update(source.as_bytes());
    }
    hex::encode(&digest.finalize()[..8])
}

#[cfg(test)]
use harness::RegisteredGlobalDbHarness;

#[cfg(test)]
async fn table_exists(db: &RegisteredGlobalDb, table: &str) -> bool {
    let snapshot = db.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            tracedecay_runtime_core::db::engine::params![table],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().is_some()
}

#[cfg(test)]
async fn row_count(db: &RegisteredGlobalDb, table: &str) -> i64 {
    let snapshot = db.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

#[tokio::test]
async fn registered_mount_publishes_complete_migrated_schema() {
    let harness = RegisteredGlobalDbHarness::open("complete-migrated-schema").await;
    let snapshot = harness.registered.read_snapshot().await.unwrap();

    super::schema_contract::validate_authority_schema_contract(&snapshot)
        .await
        .expect("registered runtime must publish the complete authority schema");
    assert!(
        table_exists(&harness.registered, "authorized_scope_sets_v1").await,
        "registered runtime omitted canonical scope-set CAS schema"
    );

    for (table, column) in [
        ("code_projects", "primary_root_platform"),
        ("code_projects", "primary_root_bytes"),
        ("code_projects", "primary_root_last_seen_at"),
        ("parse_offsets", "file_id"),
        ("sessions", "parent_session_id"),
        ("sessions", "is_subagent"),
        ("sessions", "agent_id"),
        ("sessions", "parent_tool_use_id"),
    ] {
        let mut rows = snapshot
            .query(
                "SELECT 1 FROM pragma_table_xinfo(?1) WHERE name = ?2",
                tracedecay_runtime_core::db::engine::params![table, column],
            )
            .await
            .unwrap();
        assert!(
            rows.next().await.unwrap().is_some(),
            "registered migration omitted {table}.{column}"
        );
    }
}

#[tokio::test]
async fn registered_schema_validation_rejects_incomplete_authority_schema() {
    let harness = RegisteredGlobalDbHarness::open("reject-incomplete-authority-schema").await;
    harness
        .registered
        .writer_connection()
        .unwrap()
        .execute_batch("DROP INDEX idx_project_aliases_project_id")
        .await
        .unwrap();
    let snapshot = harness.registered.read_snapshot().await.unwrap();

    let error = super::schema_contract::validate_authority_schema_contract(&snapshot)
        .await
        .expect_err("incomplete registered authority schema unexpectedly validated");
    assert!(
        error
            .to_string()
            .contains("table 'project_aliases' is missing required index on (project_id)"),
        "{error}"
    );
}

#[tokio::test]
async fn concurrent_registered_mounts_singleflight_to_one_runtime() {
    let harness = RegisteredGlobalDbHarness::open("concurrent-registered-mounts").await;
    let (first, second, third, fourth) = tokio::join!(
        harness.mount(),
        harness.mount(),
        harness.mount(),
        harness.mount(),
    );
    for mounted in [&second, &third, &fourth] {
        assert!(Arc::ptr_eq(&first, mounted));
        assert_eq!(first.binding(), mounted.binding());
    }
}

#[tokio::test]
async fn queued_registered_write_rechecks_authority_when_dequeued() {
    let mut harness = RegisteredGlobalDbHarness::open("queued-authority-loss").await;
    let db = Arc::clone(&harness.registered);
    let transaction = db.begin_write_transaction().await.unwrap();

    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let queued_db = Arc::clone(&db);
    let queued = tokio::spawn(async move {
        started_tx.send(()).unwrap();
        queued_db
            .writer_connection()
            .unwrap()
            .execute(
                "CREATE TABLE stale_queued_writer_must_not_persist (value INTEGER)",
                (),
            )
            .await
    });
    started_rx.await.unwrap();
    tokio::task::yield_now().await;
    assert!(
        !queued.is_finished(),
        "write was not queued behind transaction"
    );

    harness.revoke();
    transaction.rollback().await.unwrap();
    let error = queued
        .await
        .unwrap()
        .expect_err("queued write did not recheck authority");
    assert!(error.to_string().contains("active daemon"), "{error}");
    assert!(!table_exists(&db, "stale_queued_writer_must_not_persist").await);
}

#[tokio::test]
async fn cancelled_authoritative_transaction_isolated_from_reads_and_cleans_payload() {
    let harness = RegisteredGlobalDbHarness::open("cancelled-authoritative-transaction").await;
    let db = Arc::clone(&harness.registered);
    let storage_root = harness.storage_root().to_path_buf();
    let (created_tx, created_rx) = tokio::sync::oneshot::channel();
    let task_db = Arc::clone(&db);
    let task_storage_root = storage_root.clone();
    let task = tokio::spawn(async move {
        let transaction = task_db.begin_write_transaction().await.unwrap();
        transaction
            .execute(
                "INSERT INTO sessions (provider, session_id, project_key, project_path)
                 VALUES ('codex', 'cancelled-transaction', 'project', '/project')",
                (),
            )
            .await
            .unwrap();
        let mut payload_rollback =
            tracedecay_sessions::runtime::lcm::payload::PayloadFileRollback::begin_cancellation_safe(
                &task_storage_root,
            );
        let payload = tracedecay_sessions::runtime::lcm::payload::write_external_payload_tracked(
            &task_storage_root,
            tracedecay_sessions::runtime::lcm::payload::ExternalPayloadWrite {
                provider: "codex",
                session_id: "cancelled-transaction",
                message_id: "cancelled-message",
                kind: "tool_output",
                content: "payload created inside a transaction that will be cancelled",
                metadata_json: None,
            },
            &mut payload_rollback,
        )
        .unwrap();
        created_tx.send(payload.payload_ref).unwrap();
        std::future::pending::<()>().await;
    });

    let payload_ref = created_rx.await.expect("payload creation signal");
    let payload_path =
        tracedecay_sessions::runtime::lcm::payload::payload_dir(&storage_root).join(&payload_ref);
    assert!(payload_path.is_file());

    let snapshot = db.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT 1 FROM sessions WHERE provider = ?1 AND session_id = ?2",
            tracedecay_runtime_core::db::engine::params!["codex", "cancelled-transaction"],
        )
        .await
        .expect("retained read must not join the uncommitted transaction");
    assert!(rows.next().await.unwrap().is_none());
    drop(rows);
    drop(snapshot);

    let queued_db = Arc::clone(&db);
    let queued_write = tokio::spawn(async move {
        queued_db
            .writer_connection()
            .unwrap()
            .execute(
                "INSERT INTO sessions (provider, session_id, project_key, project_path)
                 VALUES ('codex', 'queued-after-cancellation', 'project', '/project')",
                (),
            )
            .await
    });
    tokio::pin!(queued_write);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut queued_write)
            .await
            .is_err(),
        "queued writer bypassed the active transaction"
    );

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut queued_write)
            .await
            .expect("queued writer remained blocked after cancellation")
            .expect("queued writer task failed")
            .is_ok()
    );
    assert!(!payload_path.exists());

    let snapshot = db.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT session_id FROM sessions WHERE provider = 'codex' ORDER BY session_id",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "queued-after-cancellation"
    );
    assert!(rows.next().await.unwrap().is_none());
}

#[tokio::test]
async fn cancelled_lcm_lifecycle_mutation_rolls_back_and_releases_writer() {
    let harness = RegisteredGlobalDbHarness::open("cancelled-lcm-lifecycle").await;
    let db = Arc::clone(&harness.registered);
    let update = tracedecay_sessions::runtime::lcm::LcmLifecycleUpdate {
        provider: "cursor".to_string(),
        conversation_id: "cancelled-lifecycle".to_string(),
        current_session_id: "cancelled-lifecycle".to_string(),
        current_frontier_store_id: None,
        last_finalized_session_id: None,
        last_finalized_frontier_store_id: None,
        maintenance_debt: vec![
            tracedecay_sessions::runtime::lcm::LcmMaintenanceDebt::RawBacklog {
                from_store_id: 1,
                to_store_id: 2,
            },
        ],
    };
    let (written_tx, written_rx) = tokio::sync::oneshot::channel();
    let task_db = Arc::clone(&db);
    let task_update = update.clone();
    let task = tokio::spawn(async move {
        let transaction = task_db.begin_write_transaction().await.unwrap();
        tracedecay_sessions::runtime::lcm::compression::update_lifecycle(&transaction, task_update)
            .await
            .unwrap();
        written_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });

    written_rx.await.expect("lifecycle write signal");
    let snapshot = db.read_snapshot().await.unwrap();
    assert!(
        tracedecay_sessions::runtime::lcm::compression::lifecycle_state(
            &snapshot,
            "cursor",
            "cancelled-lifecycle",
        )
        .await
        .is_err(),
        "retained reader observed uncommitted lifecycle state"
    );
    drop(snapshot);

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let snapshot = db.read_snapshot().await.unwrap();
    assert!(
        tracedecay_sessions::runtime::lcm::compression::lifecycle_state(
            &snapshot,
            "cursor",
            "cancelled-lifecycle",
        )
        .await
        .is_err(),
        "cancellation persisted lifecycle state or maintenance debt"
    );
    drop(snapshot);

    let transaction = db.begin_write_transaction().await.unwrap();
    let state = tracedecay_sessions::runtime::lcm::compression::update_lifecycle(
        &transaction,
        update.clone(),
    )
    .await
    .unwrap();
    transaction.commit().await.unwrap();
    assert_eq!(state.provider, update.provider);
    assert_eq!(state.conversation_id, update.conversation_id);
    assert_eq!(state.maintenance_debt, update.maintenance_debt);
}

#[tokio::test]
async fn analytics_batch_error_rolls_back_prior_rows_and_releases_writer() {
    let harness = RegisteredGlobalDbHarness::open("analytics-batch-rollback").await;
    let db = &harness.registered;
    db.writer_connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_analytics_batch
             BEFORE INSERT ON analytics_events
             WHEN NEW.event_kind = 'force_failure'
             BEGIN
                 SELECT RAISE(ABORT, 'forced analytics failure');
             END;",
        )
        .await
        .unwrap();

    let event = |event_kind: &str| AnalyticsEventInsert {
        provider: "codex".to_string(),
        project_id: "project".to_string(),
        session_id: Some("session".to_string()),
        timestamp: 1,
        event_kind: event_kind.to_string(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: None,
        metadata_json: None,
    };
    assert!(
        db.append_analytics_events(&[event("valid"), event("force_failure")])
            .await
            .is_err()
    );
    assert_eq!(row_count(db, "analytics_events").await, 0);

    db.writer_connection()
        .unwrap()
        .execute("DROP TRIGGER fail_analytics_batch", ())
        .await
        .unwrap();
    assert_eq!(
        db.append_analytics_events(&[event("after_failure")])
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn analytics_import_cursor_failure_rolls_back_events() {
    let harness = RegisteredGlobalDbHarness::open("analytics-cursor-rollback").await;
    let db = &harness.registered;
    db.writer_connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_analytics_cursor
             BEFORE INSERT ON parse_offsets
             WHEN NEW.file_path = 'hook_analytics:fixture'
             BEGIN
                 SELECT RAISE(ABORT, 'forced cursor failure');
             END;",
        )
        .await
        .unwrap();
    let event = AnalyticsEventInsert {
        provider: "codex".to_string(),
        project_id: "project".to_string(),
        session_id: Some("session".to_string()),
        timestamp: 1,
        event_kind: "hook_route".to_string(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: None,
        metadata_json: None,
    };

    assert!(
        db.append_analytics_events_with_cursor(
            &[event],
            "hook_analytics:fixture",
            ParseOffset {
                byte_offset: 42,
                mtime: 7,
                file_id: 0,
            },
        )
        .await
        .is_err()
    );
    assert_eq!(row_count(db, "analytics_events").await, 0);
    assert_eq!(db.get_parse_offset("hook_analytics:fixture").await, None);
}

#[tokio::test]
async fn turn_batch_error_rolls_back_prior_rows_and_releases_writer() {
    let harness = RegisteredGlobalDbHarness::open("turn-batch-rollback").await;
    let db = &harness.registered;
    db.writer_connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_turn_batch
             BEFORE INSERT ON turns
             WHEN NEW.message_id = 'force-failure'
             BEGIN
                 SELECT RAISE(ABORT, 'forced turn failure');
             END;",
        )
        .await
        .unwrap();

    let turn = |message_id: &str| tracedecay_domain::observability::CostTurn {
        message_id: message_id.to_string(),
        project_hash: "project".to_string(),
        session_id: "session".to_string(),
        model: "test-model".to_string(),
        timestamp: 1,
        input_tokens: 1,
        output_tokens: 1,
        cache_write_tokens: 0,
        cache_read_tokens: 0,
        cost_usd: 0.01,
        category: "test".to_string(),
        tool_names: String::new(),
    };
    assert_eq!(
        db.insert_turns(&[turn("valid"), turn("force-failure")])
            .await,
        0
    );
    assert_eq!(row_count(db, "turns").await, 0);

    db.writer_connection()
        .unwrap()
        .execute("DROP TRIGGER fail_turn_batch", ())
        .await
        .unwrap();
    assert_eq!(db.insert_turns(&[turn("after-failure")]).await, 1);
}

#[tokio::test]
async fn accounting_import_cursor_failure_rolls_back_turns() {
    let harness = RegisteredGlobalDbHarness::open("accounting-cursor-rollback").await;
    let db = &harness.registered;
    db.writer_connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_accounting_cursor
             BEFORE INSERT ON parse_offsets
             WHEN NEW.file_path = 'accounting:fixture'
             BEGIN
                 SELECT RAISE(ABORT, 'forced cursor failure');
             END;",
        )
        .await
        .unwrap();
    let turn = tracedecay_domain::observability::CostTurn {
        message_id: "accounting-cursor-turn".to_string(),
        project_hash: "project".to_string(),
        session_id: "session".to_string(),
        model: "test-model".to_string(),
        timestamp: 1,
        input_tokens: 1,
        output_tokens: 1,
        cache_write_tokens: 0,
        cache_read_tokens: 0,
        cost_usd: 0.01,
        category: "test".to_string(),
        tool_names: String::new(),
    };

    assert!(
        db.insert_turns_with_cursor(
            &[turn],
            "accounting:fixture",
            ParseOffset {
                byte_offset: 42,
                mtime: 7,
                file_id: 0,
            },
        )
        .await
        .is_err()
    );
    assert_eq!(row_count(db, "turns").await, 0);
    assert_eq!(db.get_parse_offset("accounting:fixture").await, None);
}

#[tokio::test]
async fn registered_handles_share_one_serialized_writer() {
    let harness = RegisteredGlobalDbHarness::open("shared-registered-writer").await;
    let first = Arc::clone(&harness.registered);
    let second = Arc::clone(&harness.registered);
    let savings = Arc::clone(&harness.registered);

    let transaction = first.begin_write_transaction().await.unwrap();
    let event = AnalyticsEventInsert {
        provider: "daemon_hook".to_string(),
        project_id: "project".to_string(),
        session_id: Some("session".to_string()),
        timestamp: 1,
        event_kind: "hook_route".to_string(),
        hook_name: Some("runtime".to_string()),
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: Some("observed".to_string()),
        metadata_json: None,
    };
    let analytics = second.append_analytics_event(&event);
    tokio::pin!(analytics);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut analytics)
            .await
            .is_err(),
        "analytics bypassed the active transaction"
    );

    let savings_write = savings.record_savings("/project", "tracedecay_runtime", 100, 50, 1);
    tokio::pin!(savings_write);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut savings_write)
            .await
            .is_err(),
        "accounting bypassed the active transaction"
    );

    transaction.commit().await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut analytics)
            .await
            .expect("analytics append timed out")
            .is_ok()
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), &mut savings_write)
        .await
        .expect("savings insert timed out");
    assert_eq!(first.sum_savings(None, 0).await.calls, 1);
}

#[tokio::test]
async fn optional_accounting_exposes_database_failures_to_callers() {
    let mut harness = RegisteredGlobalDbHarness::open("accounting-write-errors").await;
    let database = Arc::clone(&harness.registered);
    harness.revoke();

    let upsert_error = database
        .try_upsert_project_tokens(std::path::Path::new("/project"), 10)
        .await
        .expect_err("revoked authority must be visible to optional token accounting");
    let savings_error = database
        .try_record_savings("/project", "tracedecay_runtime", 10, 5, 1)
        .await
        .expect_err("revoked authority must be visible to optional savings accounting");

    assert!(upsert_error.to_string().contains("active daemon"));
    assert!(savings_error.to_string().contains("active daemon"));
}

#[tokio::test]
async fn concurrent_registered_writes_remain_isolated() {
    let harness = RegisteredGlobalDbHarness::open("concurrent-registered-writes").await;
    let handles = (0..12)
        .map(|_| Arc::clone(&harness.registered))
        .collect::<Vec<_>>();

    let mut writes = tokio::task::JoinSet::new();
    for (index, db) in handles.iter().cloned().enumerate() {
        writes.spawn(async move {
            db.record_savings(
                "/shared/project",
                &format!("writer-{index}"),
                10,
                5,
                index as i64,
            )
            .await;
        });
    }
    while let Some(result) = writes.join_next().await {
        result.unwrap();
    }

    assert_eq!(handles[0].sum_savings(None, 0).await.calls, 12);
}

#[tokio::test]
async fn observability_append_is_idempotent_and_rejects_changed_input() {
    let harness = RegisteredGlobalDbHarness::open("observability-idempotency").await;
    let event = AnalyticsEventInsert {
        provider: "tracedecay-observability".to_string(),
        project_id: "scope:fixture".to_string(),
        session_id: None,
        timestamp: 1,
        event_kind: "retrieval.query.observed.v1".to_string(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: Some("idempotency:fixture".to_string()),
        outcome: Some("succeeded".to_string()),
        metadata_json: Some("{\"canonical\":true}".to_string()),
    };
    let first = harness
        .registered
        .append_observability_event(&event)
        .await
        .expect("first append");
    let replay = harness
        .registered
        .append_observability_event(&event)
        .await
        .expect("idempotent replay");
    assert_eq!(first, replay);

    let mut changed = event;
    changed.metadata_json = Some("{\"canonical\":false}".to_string());
    let error = harness
        .registered
        .append_observability_event(&changed)
        .await
        .expect_err("changed canonical input must conflict");
    assert!(error.contains("idempotency conflict"), "{error}");
    assert_eq!(
        harness
            .registered
            .count_analytics_events(Some("scope:fixture"), 0)
            .await
            .expect("event count"),
        1
    );
}

#[tokio::test]
async fn analytics_query_honors_upper_horizon_and_row_cursor() {
    let harness = RegisteredGlobalDbHarness::open("analytics-query-bounds").await;
    let event = |timestamp: i64| AnalyticsEventInsert {
        provider: "codex".to_string(),
        project_id: "project".to_string(),
        session_id: None,
        timestamp,
        event_kind: "fixture".to_string(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: None,
        metadata_json: None,
    };
    for timestamp in [1, 2, 3] {
        harness
            .registered
            .append_analytics_event(&event(timestamp))
            .await
            .expect("append event");
    }
    let bounded = harness
        .registered
        .query_analytics_events(&super::AnalyticsEventQuery {
            project_id: Some("project".to_string()),
            until: Some(3),
            limit: 10,
            ..Default::default()
        })
        .await
        .expect("bounded query");
    assert_eq!(
        bounded
            .iter()
            .map(|event| event.timestamp)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let cursor = bounded.last().expect("cursor row").id;
    let older = harness
        .registered
        .query_analytics_events(&super::AnalyticsEventQuery {
            project_id: Some("project".to_string()),
            before_id: Some(cursor),
            limit: 10,
            ..Default::default()
        })
        .await
        .expect("cursor query");
    assert_eq!(
        older
            .iter()
            .map(|event| event.timestamp)
            .collect::<Vec<_>>(),
        vec![1]
    );
}

#[tokio::test]
async fn queued_registered_writes_preserve_fifo_fairness() {
    let harness = RegisteredGlobalDbHarness::open("registered-writer-fairness").await;
    let db = Arc::clone(&harness.registered);
    db.writer_connection()
        .unwrap()
        .execute_batch(
            "CREATE TABLE writer_fairness (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                label TEXT NOT NULL UNIQUE
             )",
        )
        .await
        .unwrap();
    let transaction = db.begin_write_transaction().await.unwrap();

    let mut queued = Vec::new();
    for label in ["first", "second", "third"] {
        let queued_db = Arc::clone(&db);
        queued.push(tokio::spawn(async move {
            queued_db
                .writer_connection()
                .unwrap()
                .execute(
                    "INSERT INTO writer_fairness(label) VALUES (?1)",
                    tracedecay_runtime_core::db::engine::params![label],
                )
                .await
        }));
        tokio::task::yield_now().await;
    }
    assert!(queued.iter().all(|write| !write.is_finished()));

    transaction.commit().await.unwrap();
    for write in queued {
        write.await.unwrap().unwrap();
    }

    let snapshot = db.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query("SELECT label FROM writer_fairness ORDER BY sequence", ())
        .await
        .unwrap();
    let mut labels = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        labels.push(row.get::<String>(0).unwrap());
    }
    assert_eq!(labels, ["first", "second", "third"]);
}

#[tokio::test]
async fn deferred_read_snapshot_observes_old_or_new_never_partial() {
    async fn read_tokens(
        connection: &impl tracedecay_runtime_core::db::engine::QueryExecutor,
        project_key: &str,
    ) -> i64 {
        let mut rows = connection
            .query(
                "SELECT tokens_saved FROM projects WHERE path = ?1",
                tracedecay_runtime_core::db::engine::params![project_key],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    let harness = RegisteredGlobalDbHarness::open("deferred-read-snapshot").await;
    let db = &harness.registered;
    let project = harness.storage_root().join("snapshot-project");
    let project_key = project.to_string_lossy().into_owned();
    db.writer_connection()
        .unwrap()
        .execute(
            "INSERT INTO projects(path, tokens_saved) VALUES (?1, 1)",
            tracedecay_runtime_core::db::engine::params![project_key.as_str()],
        )
        .await
        .unwrap();

    let snapshot = db.read_snapshot().await.unwrap();
    assert_eq!(read_tokens(&snapshot, &project_key).await, 1);

    db.writer_connection()
        .unwrap()
        .execute(
            "UPDATE projects SET tokens_saved = 2 WHERE path = ?1",
            tracedecay_runtime_core::db::engine::params![project_key.as_str()],
        )
        .await
        .unwrap();

    assert_eq!(read_tokens(&snapshot, &project_key).await, 1);
    let fresh = db.read_snapshot().await.unwrap();
    assert_eq!(read_tokens(&fresh, &project_key).await, 2);
}

/// The batched canonical-key migration merges multiple drifted rows that
/// canonicalize to the same target via `MAX(tokens_saved)`, exactly as the
/// prior per-row `INSERT ... ON CONFLICT DO UPDATE` + `DELETE` loop did.
/// Regression coverage for `project_registry::migrate_project_rows_to_canonical_keys`.
#[tokio::test]
async fn migrate_project_rows_to_canonical_keys_merges_drifted_collisions() {
    let harness = RegisteredGlobalDbHarness::open("canonical-key-migration-merge").await;
    let db = &harness.registered;
    let root = harness.storage_root().join("canon-merge-project");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    let canonical = root.canonicalize().unwrap().to_string_lossy().into_owned();
    let drifted_via_parent = root.join("sub").join("..").to_string_lossy().into_owned();
    let drifted_via_dot = root.join(".").to_string_lossy().into_owned();
    assert_ne!(drifted_via_parent, canonical);
    assert_ne!(drifted_via_dot, canonical);
    assert_ne!(drifted_via_parent, drifted_via_dot);

    {
        let writer = db.writer_connection().unwrap();
        // Pre-existing canonical row holds the lowest value; two drifted
        // rows collapse onto it with higher values in scan order.
        writer
            .execute(
                "INSERT INTO projects(path, tokens_saved) VALUES (?1, ?2)",
                tracedecay_runtime_core::db::engine::params![canonical.as_str(), 3_i64],
            )
            .await
            .unwrap();
        writer
            .execute(
                "INSERT INTO projects(path, tokens_saved) VALUES (?1, ?2)",
                tracedecay_runtime_core::db::engine::params![drifted_via_parent.as_str(), 7_i64],
            )
            .await
            .unwrap();
        writer
            .execute(
                "INSERT INTO projects(path, tokens_saved) VALUES (?1, ?2)",
                tracedecay_runtime_core::db::engine::params![drifted_via_dot.as_str(), 5_i64],
            )
            .await
            .unwrap();
    }

    let transaction = db.begin_write_transaction().await.unwrap();
    super::project_registry::migrate_project_rows_to_canonical_keys(&transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let snapshot = db.read_snapshot().await.unwrap();
    let mut total_rows = snapshot
        .query("SELECT path, tokens_saved FROM projects", ())
        .await
        .unwrap();
    let mut remaining = std::collections::BTreeMap::new();
    while let Some(row) = total_rows.next().await.unwrap() {
        remaining.insert(row.get::<String>(0).unwrap(), row.get::<i64>(1).unwrap());
    }
    drop(total_rows);

    assert_eq!(
        remaining.get(canonical.as_str()),
        Some(&7),
        "canonical row keeps MAX(tokens_saved) across every collapsed drifted row: {remaining:?}"
    );
    assert!(
        !remaining.contains_key(drifted_via_parent.as_str()),
        "drifted row via `..` must be removed after migration: {remaining:?}"
    );
    assert!(
        !remaining.contains_key(drifted_via_dot.as_str()),
        "drifted row via `.` must be removed after migration: {remaining:?}"
    );
}

#[tokio::test]
async fn retained_registered_database_rejects_new_writer_after_scope_drop() {
    let mut harness = RegisteredGlobalDbHarness::open("retained-registered-writer").await;
    let db = Arc::clone(&harness.registered);
    harness.revoke();

    let error = match db.writer_connection() {
        Ok(_) => panic!("retained database outlived daemon write authority"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("active daemon"), "{error}");
    assert!(!table_exists(&db, "stale_retained_writer").await);
}

#[tokio::test]
async fn issued_registered_writer_rejects_autocommit_after_scope_drop() {
    let mut harness = RegisteredGlobalDbHarness::open("issued-registered-writer").await;
    let db = Arc::clone(&harness.registered);
    let writer = db.writer_connection().expect("acquire registered writer");
    harness.revoke();

    let error = writer
        .execute(
            "CREATE TABLE stale_daemon_writer_must_not_persist (value INTEGER)",
            (),
        )
        .await
        .expect_err("issued writer outlived daemon write authority");
    assert!(error.to_string().contains("active daemon"), "{error}");
    assert!(
        !table_exists(&db, "stale_daemon_writer_must_not_persist").await,
        "rejected autocommit write persisted"
    );
}

#[tokio::test]
async fn begun_registered_transaction_rolls_back_when_scope_drops_before_commit() {
    let mut harness = RegisteredGlobalDbHarness::open("begun-registered-transaction").await;
    let db = Arc::clone(&harness.registered);
    let transaction = db.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "CREATE TABLE stale_daemon_transaction_must_not_persist (value INTEGER)",
            (),
        )
        .await
        .unwrap();
    harness.revoke();

    let error = transaction
        .commit()
        .await
        .expect_err("transaction commit did not recheck authority");
    assert!(error.to_string().contains("active daemon"), "{error}");
    assert!(
        !table_exists(&db, "stale_daemon_transaction_must_not_persist").await,
        "failed commit did not roll back staged writes"
    );
}

#[tokio::test]
async fn begun_registered_transaction_can_roll_back_after_scope_drop() {
    let mut harness = RegisteredGlobalDbHarness::open("registered-rollback").await;
    let db = Arc::clone(&harness.registered);
    let transaction = db.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "CREATE TABLE rolled_back_daemon_transaction (value INTEGER)",
            (),
        )
        .await
        .unwrap();
    harness.revoke();

    transaction
        .rollback()
        .await
        .expect("rollback must remain allowed after authority loss");
    assert!(!table_exists(&db, "rolled_back_daemon_transaction").await);
}

#[cfg(unix)]
#[tokio::test]
async fn registered_runtime_rejects_database_file_replacement() {
    let harness = RegisteredGlobalDbHarness::open("database-file-replacement").await;
    let db_path = harness.registered.db_path().to_path_buf();
    let replaced_path = db_path.with_extension("replaced");
    std::fs::rename(&db_path, &replaced_path).unwrap();
    std::fs::File::create(&db_path).unwrap();

    let error = harness
        .registered
        .writer_connection()
        .unwrap()
        .execute("CREATE TABLE replacement_write (value INTEGER)", ())
        .await
        .expect_err("registered runtime followed a replaced database path");
    assert!(
        error.to_string().contains("identity changed")
            || error.to_string().contains("file identity"),
        "{error}"
    );
    assert_eq!(std::fs::metadata(&db_path).unwrap().len(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn registered_runtime_does_not_recreate_a_missing_database() {
    let harness = RegisteredGlobalDbHarness::open("missing-database-no-create").await;
    let db_path = harness.registered.db_path().to_path_buf();
    let moved_path = db_path.with_extension("moved");
    std::fs::rename(&db_path, &moved_path).unwrap();
    assert!(!db_path.exists());

    let error = harness
        .registered
        .writer_connection()
        .unwrap()
        .execute("CREATE TABLE forbidden_recreated_store (value INTEGER)", ())
        .await
        .expect_err("registered runtime recreated a missing database");
    assert!(
        error.to_string().contains("identity")
            || error.to_string().contains("verify")
            || error.to_string().contains("database file"),
        "{error}"
    );
    assert!(
        !db_path.exists(),
        "failed write recreated the database path"
    );
}

#[tokio::test]
async fn project_tokens_separate_a_genuine_zero_from_a_failed_read() {
    let harness = RegisteredGlobalDbHarness::open("project-tokens-failed-read").await;
    let project = std::path::Path::new("/tmp/tracedecay-project-tokens");
    let unregistered = std::path::Path::new("/tmp/tracedecay-never-registered");
    harness.registered.upsert(project, 4_242).await;

    assert_eq!(
        harness.registered.try_get_project_tokens(project).await,
        Ok(4_242)
    );
    assert_eq!(
        harness
            .registered
            .try_get_project_tokens(unregistered)
            .await,
        Ok(0),
        "a project with no registry row has genuinely saved nothing"
    );

    // A stored total that cannot be a token count is a corrupt row, not a zero.
    harness
        .registered
        .writer_connection()
        .unwrap()
        .execute_batch("UPDATE projects SET tokens_saved = -1")
        .await
        .unwrap();
    let error = harness
        .registered
        .try_get_project_tokens(project)
        .await
        .expect_err("a negative stored total must not be reported as a measurement");
    assert!(error.contains("cannot be negative"), "{error}");

    harness
        .registered
        .writer_connection()
        .unwrap()
        .execute_batch("DROP TABLE projects")
        .await
        .unwrap();
    let error = harness
        .registered
        .try_get_project_tokens(project)
        .await
        .expect_err("a failed query must not be reported as a token total");
    assert!(
        error.contains("failed to query project tokens saved"),
        "{error}"
    );
    assert_eq!(
        harness.registered.get_project_tokens(project).await,
        None,
        "the optional form reports unavailable rather than zero"
    );
}
