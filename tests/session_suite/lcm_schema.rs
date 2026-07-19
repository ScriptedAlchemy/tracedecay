use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};
use tracedecay::global_db::GlobalDb;

/// Counts BUSY/LOCKED retries and signals once on the first observation.
struct ContentionProbe {
    busy_retries: AtomicUsize,
    first_busy: Mutex<Option<oneshot::Sender<()>>>,
}

impl ContentionProbe {
    fn new(first_busy: oneshot::Sender<()>) -> Self {
        Self {
            busy_retries: AtomicUsize::new(0),
            first_busy: Mutex::new(Some(first_busy)),
        }
    }

    fn observe_busy(&self) {
        let previous = self.busy_retries.fetch_add(1, Ordering::SeqCst);
        if previous == 0 {
            if let Some(tx) = self
                .first_busy
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                let _ = tx.send(());
            }
        }
    }

    fn busy_retries(&self) -> usize {
        self.busy_retries.load(Ordering::SeqCst)
    }
}

async fn create_legacy_sessions_db(db_path: &Path) {
    create_legacy_sessions_db_with_text(db_path, "legacy text").await;
}

async fn create_legacy_sessions_db_with_text(db_path: &Path, legacy_text: &str) {
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let old_db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = old_db.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project_key TEXT NOT NULL,
            project_path TEXT NOT NULL,
            title TEXT,
            started_at INTEGER,
            ended_at INTEGER,
            transcript_path TEXT,
            metadata_json TEXT,
            PRIMARY KEY(provider, session_id)
        );
        CREATE TABLE session_messages (
            provider TEXT NOT NULL,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            timestamp INTEGER,
            ordinal INTEGER NOT NULL,
            text TEXT NOT NULL,
            kind TEXT,
            model TEXT,
            tool_names TEXT,
            source_path TEXT,
            source_offset INTEGER,
            metadata_json TEXT,
            PRIMARY KEY(provider, message_id)
        );
        INSERT INTO sessions(provider, session_id, project_key, project_path)
        VALUES ('cursor', 'legacy-session', '/tmp/project', '/tmp/project');",
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO session_messages(provider, message_id, session_id, role, ordinal, text)
         VALUES ('cursor', 'legacy-message', 'legacy-session', 'assistant', 1, ?1)",
        libsql::params![legacy_text],
    )
    .await
    .unwrap();
    drop(conn);
    drop(old_db);
}

async fn table_exists(db_path: &Path, table: &str) -> bool {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE name = ?1 AND type IN ('table', 'view')",
            libsql::params![table],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().is_some()
}

async fn row_count(db_path: &Path, table: &str) -> i64 {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let mut rows = conn.query(&sql, ()).await.unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get(0).unwrap()
}

async fn execute_with_busy_retry(
    conn: &libsql::Connection,
    sql: &str,
    probe: Option<&ContentionProbe>,
) -> Result<(), String> {
    const MAX_ATTEMPTS: usize = 20;
    for attempt in 0..MAX_ATTEMPTS {
        match timeout(Duration::from_millis(500), conn.execute(sql, ())).await {
            Ok(Ok(_)) => return Ok(()),
            Ok(Err(error)) => {
                let message = error.to_string();
                let lower = message.to_ascii_lowercase();
                let retryable = lower.contains("busy") || lower.contains("locked");
                if retryable {
                    if let Some(probe) = probe {
                        probe.observe_busy();
                    }
                }
                if !retryable || attempt + 1 == MAX_ATTEMPTS {
                    return Err(message);
                }
            }
            Err(_) if attempt + 1 == MAX_ATTEMPTS => {
                return Err("cursor rotation timed out".to_string());
            }
            Err(_) => {}
        }
        sleep(Duration::from_millis(10)).await;
    }
    Err("cursor rotation exhausted retries".to_string())
}

async fn cursor_key_history(db_path: &Path) -> Vec<(i64, i64, Option<i64>)> {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT key_version, created_at, retired_at
             FROM session_query_cursor_keys
             ORDER BY key_version",
            (),
        )
        .await
        .unwrap();
    let mut history = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        history.push((
            row.get(0).unwrap(),
            row.get(1).unwrap(),
            row.get(2).unwrap(),
        ));
    }
    history
}

fn assert_valid_cursor_chain(history: &[(i64, i64, Option<i64>)]) {
    assert!(!history.is_empty());
    for adjacent in history.windows(2) {
        let (version, created_at, retired_at) = adjacent[0];
        let (successor_version, successor_created_at, _) = adjacent[1];
        assert!(successor_version > version);
        assert!(successor_created_at > created_at);
        assert_eq!(retired_at, Some(successor_created_at));
    }
    assert!(history.last().unwrap().2.is_none());
    assert_eq!(
        history
            .iter()
            .filter(|(_, _, retired_at)| retired_at.is_none())
            .count(),
        1
    );
}

async fn fts_legacy_message_ids(db_path: &Path) -> Vec<String> {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT raw.message_id
             FROM lcm_raw_messages_fts
             JOIN lcm_raw_messages raw ON raw.store_id = lcm_raw_messages_fts.rowid
             WHERE lcm_raw_messages_fts MATCH 'legacy'
             ORDER BY raw.message_id",
            (),
        )
        .await
        .unwrap();
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        ids.push(row.get(0).unwrap());
    }
    ids
}

async fn schema_version(db_path: &Path) -> i64 {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = 'lcm'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get(0).unwrap()
}

async fn migration_applied_at(db_path: &Path) -> i64 {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT applied_at FROM session_schema_migrations WHERE name = 'lcm'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get(0).unwrap()
}

async fn set_migration_applied_at(db_path: &Path, applied_at: i64) {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute(
        "UPDATE session_schema_migrations
         SET applied_at = ?1
         WHERE name = 'lcm'",
        libsql::params![applied_at],
    )
    .await
    .unwrap();
}

async fn set_migration_version(db_path: &Path, version: i64) {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute(
        "UPDATE session_schema_migrations
         SET version = ?1
         WHERE name = 'lcm'",
        libsql::params![version],
    )
    .await
    .unwrap();
}

/// Rewrites the raw-message FTS objects into the pre-v3 shape (role +
/// metadata_json indexed alongside index_text) and stamps the requested
/// schema version, simulating a database written by an older tracedecay.
async fn downgrade_raw_fts_to_v2(db_path: &Path) {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS lcm_raw_messages_fts_insert;
         DROP TRIGGER IF EXISTS lcm_raw_messages_fts_delete;
         DROP TRIGGER IF EXISTS lcm_raw_messages_fts_update;
         DROP TABLE IF EXISTS lcm_raw_messages_fts;
         CREATE VIRTUAL TABLE lcm_raw_messages_fts USING fts5(
             index_text, role, metadata_json,
             content='lcm_raw_messages',
             content_rowid='store_id'
         );
         CREATE TRIGGER lcm_raw_messages_fts_insert
             AFTER INSERT ON lcm_raw_messages BEGIN
                 INSERT INTO lcm_raw_messages_fts(rowid, index_text, role, metadata_json)
                 VALUES (NEW.store_id, NEW.index_text, NEW.role, NEW.metadata_json);
             END;
         CREATE TRIGGER lcm_raw_messages_fts_delete
             AFTER DELETE ON lcm_raw_messages BEGIN
                 INSERT INTO lcm_raw_messages_fts(
                     lcm_raw_messages_fts, rowid, index_text, role, metadata_json
                 )
                 VALUES ('delete', OLD.store_id, OLD.index_text, OLD.role, OLD.metadata_json);
             END;
         CREATE TRIGGER lcm_raw_messages_fts_update
             AFTER UPDATE ON lcm_raw_messages BEGIN
                 INSERT INTO lcm_raw_messages_fts(
                     lcm_raw_messages_fts, rowid, index_text, role, metadata_json
                 )
                 VALUES ('delete', OLD.store_id, OLD.index_text, OLD.role, OLD.metadata_json);
                 INSERT INTO lcm_raw_messages_fts(rowid, index_text, role, metadata_json)
                 VALUES (NEW.store_id, NEW.index_text, NEW.role, NEW.metadata_json);
             END;
         INSERT INTO lcm_raw_messages_fts(lcm_raw_messages_fts) VALUES('rebuild');
         UPDATE session_schema_migrations SET version = 2 WHERE name = 'lcm';",
    )
    .await
    .unwrap();
}

async fn fts_message_ids_matching(db_path: &Path, query: &str) -> Vec<String> {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT raw.message_id
             FROM lcm_raw_messages_fts
             JOIN lcm_raw_messages raw ON raw.store_id = lcm_raw_messages_fts.rowid
             WHERE lcm_raw_messages_fts MATCH ?1
             ORDER BY raw.message_id",
            libsql::params![query],
        )
        .await
        .unwrap();
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        ids.push(row.get(0).unwrap());
    }
    ids
}

async fn raw_fts_object_sql(db_path: &Path) -> Vec<String> {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT sql FROM sqlite_master
             WHERE name IN ('lcm_raw_messages_fts',
                            'lcm_raw_messages_fts_insert',
                            'lcm_raw_messages_fts_delete',
                            'lcm_raw_messages_fts_update')",
            (),
        )
        .await
        .unwrap();
    let mut sqls = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        sqls.push(row.get(0).unwrap());
    }
    sqls
}

async fn normalized_trigger_sql(db_path: &Path, trigger: &str) -> String {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
            libsql::params![trigger],
        )
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<String>()
}

const TEMPORAL_SCHEMA_OBJECTS: &[(&str, &str)] = &[
    ("index", "idx_session_agent_hierarchy_edges_child"),
    ("index", "idx_session_assertion_supersession_successor"),
    ("index", "idx_session_assertions_generation_order"),
    ("index", "idx_session_assertions_kind_order"),
    ("index", "idx_session_assertions_object_order"),
    ("index", "idx_session_assertions_subject"),
    ("index", "idx_session_current_entities_assertion"),
    ("index", "idx_session_current_entities_occurrence"),
    ("index", "idx_session_external_payload_manifests_session"),
    ("index", "idx_session_logical_copy_edges_target"),
    ("index", "idx_session_occurrences_agent"),
    ("index", "idx_session_occurrences_anchor_order"),
    ("index", "idx_session_occurrences_generation_order"),
    ("index", "idx_session_occurrences_message"),
    ("index", "idx_session_occurrences_root_generation_order"),
    ("index", "idx_session_occurrences_session_time"),
    ("index", "idx_session_occurrences_thread"),
    ("index", "idx_session_occurrences_turn"),
    ("index", "idx_session_query_cursor_keys_active"),
    ("index", "idx_session_refresh_operations_join"),
    ("index", "idx_session_refresh_operations_one_running"),
    ("index", "idx_session_refresh_operations_state"),
    ("index", "idx_session_refresh_receipts_session"),
    ("index", "idx_session_summary_availability_generation"),
    ("index", "idx_session_summary_nodes_root_created_order"),
    ("index", "idx_session_summary_nodes_session_created"),
    ("index", "idx_session_summary_sources_anchor"),
    ("index", "idx_session_summary_sources_summary"),
    ("index", "idx_session_summary_successors_successor"),
    ("index", "idx_session_temporal_generations_one_active"),
    ("index", "idx_session_temporal_generations_session_state"),
    ("index", "idx_session_temporal_migration_dispositions_kind"),
    ("index", "idx_session_temporal_migration_dispositions_row"),
    ("index", "idx_session_temporal_migration_receipts_source"),
    ("index", "idx_session_temporal_observation_effects_session"),
    ("index", "idx_session_thread_hierarchy_edges_child"),
    ("index", "idx_session_turn_members_occurrence"),
    ("table", "session_agent_hierarchy_edges"),
    ("table", "session_agents"),
    ("table", "session_assertion_supersession"),
    ("table", "session_assertions"),
    ("table", "session_current_entities"),
    ("table", "session_external_payload_manifests"),
    ("table", "session_logical_copy_edges"),
    ("table", "session_occurrences"),
    ("table", "session_occurrences_fts"),
    ("table", "session_query_cursor_keys"),
    ("table", "session_refresh_batch_bindings"),
    ("table", "session_refresh_bindings"),
    ("table", "session_refresh_operations"),
    ("table", "session_refresh_progress"),
    ("table", "session_refresh_receipts"),
    ("table", "session_summary_availability"),
    ("table", "session_summary_nodes"),
    ("table", "session_summary_nodes_fts"),
    ("table", "session_summary_sources"),
    ("table", "session_summary_successors"),
    ("table", "session_temporal_generations"),
    ("table", "session_temporal_migration_dispositions"),
    ("table", "session_temporal_migration_receipts"),
    ("table", "session_temporal_observation_effects"),
    ("table", "session_temporal_projection_receipts"),
    ("table", "session_temporal_schema_migrations"),
    ("table", "session_thread_hierarchy_edges"),
    ("table", "session_threads"),
    ("table", "session_turn_members"),
    ("table", "session_turns"),
    (
        "trigger",
        "session_external_payload_manifests_immutable_delete_v1",
    ),
    (
        "trigger",
        "session_external_payload_manifests_immutable_update_v1",
    ),
    ("trigger", "session_occurrences_fts_delete_v1"),
    ("trigger", "session_occurrences_fts_insert_v1"),
    ("trigger", "session_occurrences_fts_update_v1"),
    ("trigger", "session_query_cursor_keys_immutable_delete_v1"),
    ("trigger", "session_query_cursor_keys_insert_guard_v1"),
    ("trigger", "session_query_cursor_keys_retire_update_v1"),
    ("trigger", "session_query_cursor_keys_rotate_insert_v1"),
    (
        "trigger",
        "session_refresh_batch_bindings_immutable_delete_v1",
    ),
    (
        "trigger",
        "session_refresh_batch_bindings_immutable_update_v1",
    ),
    ("trigger", "session_refresh_batch_bindings_insert_guard_v1"),
    ("trigger", "session_refresh_bindings_immutable_delete_v1"),
    ("trigger", "session_refresh_bindings_immutable_update_v1"),
    ("trigger", "session_refresh_bindings_insert_guard_v1"),
    ("trigger", "session_refresh_operations_delete_guard_v1"),
    ("trigger", "session_refresh_operations_insert_guard_v1"),
    ("trigger", "session_refresh_operations_state_guard_v1"),
    ("trigger", "session_refresh_progress_insert_guard_v1"),
    ("trigger", "session_refresh_progress_immutable_delete_v1"),
    ("trigger", "session_refresh_progress_immutable_update_v1"),
    ("trigger", "session_refresh_receipts_insert_guard_v1"),
    ("trigger", "session_refresh_receipts_immutable_delete_v1"),
    ("trigger", "session_refresh_receipts_immutable_update_v1"),
    ("trigger", "session_summary_nodes_fts_delete_v1"),
    ("trigger", "session_summary_nodes_fts_insert_v1"),
    ("trigger", "session_summary_nodes_fts_update_v1"),
    ("trigger", "session_summary_nodes_immutable_delete_v1"),
    ("trigger", "session_summary_nodes_immutable_update_v1"),
    ("trigger", "session_summary_availability_owner_insert_v1"),
    ("trigger", "session_summary_availability_owner_update_v1"),
    (
        "trigger",
        "session_external_payload_manifests_owner_guard_v1",
    ),
    ("trigger", "session_summary_sources_immutable_delete_v1"),
    ("trigger", "session_summary_sources_immutable_update_v1"),
    ("trigger", "session_summary_sources_owner_guard_v1"),
    ("trigger", "session_summary_successors_immutable_delete_v1"),
    ("trigger", "session_summary_successors_immutable_update_v1"),
    ("trigger", "session_summary_successors_owner_guard_v1"),
    ("trigger", "session_temporal_generations_delete_guard_v1"),
    ("trigger", "session_temporal_generations_insert_guard_v1"),
    (
        "trigger",
        "session_temporal_generations_single_active_insert_v1",
    ),
    (
        "trigger",
        "session_temporal_generations_single_active_update_v1",
    ),
    ("trigger", "session_temporal_generations_state_guard_v1"),
    (
        "trigger",
        "session_temporal_migration_dispositions_immutable_delete_v1",
    ),
    (
        "trigger",
        "session_temporal_migration_dispositions_immutable_update_v1",
    ),
    (
        "trigger",
        "session_temporal_migration_receipts_immutable_delete_v1",
    ),
    (
        "trigger",
        "session_temporal_migration_receipts_immutable_update_v1",
    ),
    (
        "trigger",
        "session_temporal_observation_effects_immutable_delete_v1",
    ),
    (
        "trigger",
        "session_temporal_observation_effects_immutable_update_v1",
    ),
    (
        "trigger",
        "session_temporal_observation_effects_insert_guard_v1",
    ),
    (
        "trigger",
        "session_temporal_projection_receipts_immutable_delete_v1",
    ),
    (
        "trigger",
        "session_temporal_projection_receipts_immutable_update_v1",
    ),
    (
        "trigger",
        "session_temporal_projection_receipts_insert_guard_v1",
    ),
];

async fn temporal_schema_object_catalog(db_path: &Path) -> Vec<(String, String)> {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT type, name, tbl_name
             FROM sqlite_master
             WHERE type IN ('index', 'table', 'trigger')
               AND sql IS NOT NULL
             ORDER BY type, name",
            (),
        )
        .await
        .unwrap();
    let mut objects = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        let object_type: String = row.get(0).unwrap();
        let object_name: String = row.get(1).unwrap();
        let table_name: String = row.get(2).unwrap();
        let temporal_namespace = table_name.starts_with("session_agent")
            || table_name.starts_with("session_assertion")
            || table_name.starts_with("session_current_entit")
            || table_name.starts_with("session_external_payload")
            || table_name.starts_with("session_logical_copy")
            || table_name.starts_with("session_occurrence")
            || table_name.starts_with("session_query_cursor")
            || table_name.starts_with("session_refresh")
            || table_name.starts_with("session_summary_")
            || table_name.starts_with("session_temporal_")
            || table_name.starts_with("session_thread")
            || table_name.starts_with("session_turn");
        let fts_shadow = object_type == "table"
            && (object_name.starts_with("session_occurrences_fts_")
                || object_name.starts_with("session_summary_nodes_fts_"));
        if temporal_namespace && !fts_shadow {
            objects.push((object_type, object_name));
        }
    }
    objects
}

async fn explain_query_plan(conn: &libsql::Connection, sql: &str) -> Vec<String> {
    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
        .await
        .unwrap();
    let mut details = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        details.push(row.get::<String>(3).unwrap());
    }
    details
}

async fn index_key_columns(conn: &libsql::Connection, index: &str) -> Vec<(String, i64)> {
    let mut rows = conn
        .query(&format!("PRAGMA index_xinfo('{index}')"), ())
        .await
        .unwrap();
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        let is_key: i64 = row.get(5).unwrap();
        if is_key != 0 {
            columns.push((
                row.get::<i64>(0).unwrap(),
                row.get::<Option<String>>(2)
                    .unwrap()
                    .unwrap_or_else(|| "<expression>".to_string()),
                row.get::<i64>(3).unwrap(),
            ));
        }
    }
    columns.sort_by_key(|(sequence, _, _)| *sequence);
    columns
        .into_iter()
        .map(|(_, name, descending)| (name, descending))
        .collect()
}

async fn table_index_names(conn: &libsql::Connection, table: &str) -> Vec<String> {
    let mut rows = conn
        .query(&format!("PRAGMA index_list('{table}')"), ())
        .await
        .unwrap();
    let mut names = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        names.push(row.get(1).unwrap());
    }
    names
}

async fn temporal_schema_version(db_path: &Path) -> i64 {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT version
             FROM session_temporal_schema_migrations
             WHERE name = 'session-temporal'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get(0).unwrap()
}

async fn copy_database_for_temporal_restart(source: &Path, target: &Path) {
    let db = libsql::Builder::new_local(source).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .await
        .unwrap();
    drop(conn);
    drop(db);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::copy(source, target).unwrap();
}

#[tokio::test]
async fn temporal_schema_complete_object_catalog() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");

    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let mut expected = TEMPORAL_SCHEMA_OBJECTS
        .iter()
        .map(|(object_type, object_name)| ((*object_type).to_string(), (*object_name).to_string()))
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(temporal_schema_object_catalog(&db_path).await, expected);
    assert!(
        table_exists(&db_path, "lcm_raw_messages").await,
        "the additive temporal schema must preserve legacy LCM tables"
    );
}

#[tokio::test]
async fn temporal_payload_manifest_schema_is_payload_global() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT name FROM pragma_table_info('session_external_payload_manifests')
             ORDER BY cid",
            (),
        )
        .await
        .unwrap();
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        columns.push(row.get::<String>(0).unwrap());
    }
    assert_eq!(
        columns,
        [
            "payload_ref",
            "session_id",
            "payload_digest",
            "manifest_json",
            "receipt_id",
            "created_at",
        ]
    );

    let mut rows = conn
        .query(
            "SELECT \"from\", \"table\", \"to\"
             FROM pragma_foreign_key_list('session_external_payload_manifests')
             ORDER BY \"from\"",
            (),
        )
        .await
        .unwrap();
    let mut foreign_keys = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        foreign_keys.push((
            row.get::<String>(0).unwrap(),
            row.get::<String>(1).unwrap(),
            row.get::<String>(2).unwrap(),
        ));
    }
    assert_eq!(
        foreign_keys,
        [(
            "receipt_id".to_string(),
            "sanitization_receipts".to_string(),
            "receipt_id".to_string(),
        )]
    );

    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         INSERT INTO sessions(provider, session_id, project_key, project_path)
         VALUES ('cursor', 'manifest-owner', '/tmp/project', '/tmp/project');
         INSERT INTO sanitization_receipts (
             receipt_id, sanitizer_version, payload_digest, receipt_json
         ) VALUES ('manifest-receipt', 'test', 'digest', '{}');
         INSERT INTO lcm_external_payloads (
             payload_ref, provider, session_id, message_id, kind, content_hash,
             byte_count, char_count, created_at
         ) VALUES
             ('payload-owned', 'cursor', 'manifest-owner', 'message-owned',
              'tool', 'digest', 1, 1, 100),
             ('payload-cross-session', 'cursor', 'manifest-owner', 'message-cross',
              'tool', 'digest', 1, 1, 100);
         INSERT INTO session_external_payload_manifests (
             payload_ref, session_id, payload_digest, manifest_json, receipt_id, created_at
         ) VALUES (
             'payload-owned', 'manifest-owner', 'digest', '{}', 'manifest-receipt', 100
         );",
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO session_external_payload_manifests (
                 payload_ref, session_id, payload_digest, manifest_json, receipt_id, created_at
             ) VALUES (
                 'payload-cross-session', 'different-owner', 'digest', '{}',
                 'manifest-receipt', 100
             )",
            (),
        )
        .await
        .is_err(),
        "a payload manifest owner must match raw payload authority"
    );
    for sql in [
        "UPDATE session_external_payload_manifests SET payload_digest = 'rewrite'
         WHERE payload_ref = 'payload-owned'",
        "DELETE FROM session_external_payload_manifests WHERE payload_ref = 'payload-owned'",
    ] {
        assert!(
            conn.execute(sql, ()).await.is_err(),
            "payload-global authority must remain immutable: {sql}"
        );
    }
}

#[tokio::test]
async fn temporal_schema_migration_is_atomic_and_idempotent() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute_batch("CREATE TABLE session_temporal_generations (wrong_column TEXT);")
        .await
        .unwrap();
    drop(conn);
    drop(raw_db);

    assert!(
        GlobalDb::try_open_at(&db_path).await.is_err(),
        "an incompatible temporal table must reject the whole additive migration"
    );
    assert!(
        !table_exists(&db_path, "session_temporal_schema_migrations").await,
        "a rejected temporal migration must not leave its version marker behind"
    );
    assert!(
        !table_exists(&db_path, "session_summary_nodes").await,
        "a rejected temporal migration must not leave partially-created authority tables"
    );

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute("DROP TABLE session_temporal_generations", ())
        .await
        .unwrap();
    drop(conn);
    drop(raw_db);

    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("fresh temporal migration should succeed")
        .expect("global database should open");
    drop(db);
    let initial_catalog = temporal_schema_object_catalog(&db_path).await;
    let initial_version = temporal_schema_version(&db_path).await;

    let restart_path = tmp.path().join(".tracedecay").join("restart.db");
    copy_database_for_temporal_restart(&db_path, &restart_path).await;
    let reopened = GlobalDb::try_open_at(&restart_path)
        .await
        .expect("idempotent temporal reopen should succeed")
        .expect("global database should reopen");
    drop(reopened);
    assert_eq!(
        temporal_schema_version(&restart_path).await,
        initial_version
    );
    assert_eq!(
        temporal_schema_object_catalog(&restart_path).await,
        initial_catalog
    );
}

#[tokio::test]
async fn temporal_schema_replaces_stale_refresh_guards_on_every_reopen() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let triggers = [
        "session_refresh_progress_insert_guard_v1",
        "session_refresh_receipts_insert_guard_v1",
    ];
    let mut canonical = Vec::new();
    for trigger in triggers {
        canonical.push((trigger, normalized_trigger_sql(&db_path, trigger).await));
    }

    for marker_version in [1_i64, 2_i64] {
        let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
        let conn = raw_db.connect().unwrap();
        conn.execute_batch(
            "DROP TRIGGER session_refresh_progress_insert_guard_v1;
             DROP TRIGGER session_refresh_receipts_insert_guard_v1;
             CREATE TRIGGER session_refresh_progress_insert_guard_v1
             BEFORE INSERT ON session_refresh_progress BEGIN SELECT 1; END;
             CREATE TRIGGER session_refresh_receipts_insert_guard_v1
             BEFORE INSERT ON session_refresh_receipts BEGIN SELECT 1; END;",
        )
        .await
        .unwrap();
        conn.execute(
            "UPDATE session_temporal_schema_migrations
             SET version = ?1
             WHERE name = 'session-temporal'",
            libsql::params![marker_version],
        )
        .await
        .unwrap();
        drop(conn);
        drop(raw_db);

        let reopened = GlobalDb::try_open_at(&db_path)
            .await
            .expect("stale refresh guards should be replaced")
            .expect("global database should reopen");
        drop(reopened);
        for (trigger, expected) in &canonical {
            assert_eq!(
                normalized_trigger_sql(&db_path, trigger).await,
                *expected,
                "{trigger} must converge at marker version {marker_version}"
            );
        }
    }
}

#[tokio::test]
async fn temporal_schema_trigger_installation_is_atomic() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute_batch("CREATE TABLE authority_audit_checkpoints (wrong_column TEXT);")
        .await
        .unwrap();
    drop(conn);
    drop(raw_db);

    assert!(
        GlobalDb::try_open_at(&db_path).await.is_err(),
        "an invariant-installation failure must reject the temporal migration"
    );
    assert!(
        !table_exists(&db_path, "session_temporal_schema_migrations").await,
        "the temporal marker must not commit before invariant triggers install"
    );
    assert!(
        !table_exists(&db_path, "session_summary_nodes").await,
        "temporal authority tables and invariant triggers must share one transaction"
    );
}

#[tokio::test]
async fn temporal_schema_refuses_future_version_without_mutation() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);
    assert!(
        table_exists(&db_path, "session_temporal_schema_migrations").await,
        "the temporal schema must install a version marker before a future version is tested"
    );

    let before_catalog = temporal_schema_object_catalog(&db_path).await;
    let future_version = temporal_schema_version(&db_path).await + 97;
    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "UPDATE session_temporal_schema_migrations
         SET version = ?1
         WHERE name = 'session-temporal'",
        libsql::params![future_version],
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let restart_path = tmp.path().join(".tracedecay").join("future.db");
    copy_database_for_temporal_restart(&db_path, &restart_path).await;
    assert!(
        GlobalDb::try_open_at(&restart_path).await.is_err(),
        "a newer temporal schema must be refused instead of treated as current"
    );
    assert_eq!(temporal_schema_version(&restart_path).await, future_version);
    assert_eq!(
        temporal_schema_object_catalog(&restart_path).await,
        before_catalog
    );
}

#[tokio::test]
async fn temporal_schema_persists_cursor_keys_without_read_creation() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);
    assert!(
        table_exists(&db_path, "session_query_cursor_keys").await,
        "the temporal schema must create the cursor-key authority table"
    );
    assert_eq!(row_count(&db_path, "session_query_cursor_keys").await, 0);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "INSERT INTO session_query_cursor_keys (
            key_id, key_version, key_material, created_at, retired_at
         )
         VALUES ('key-1', 1, X'01', 100, NULL)",
        (),
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let restart_path = tmp.path().join(".tracedecay").join("restart.db");
    copy_database_for_temporal_restart(&db_path, &restart_path).await;
    let reopened = GlobalDb::try_open_at(&restart_path)
        .await
        .expect("writer reopen should preserve a persisted cursor key")
        .expect("global database should reopen");
    drop(reopened);
    assert_eq!(
        row_count(&restart_path, "session_query_cursor_keys").await,
        1
    );

    let missing_path = tmp.path().join(".tracedecay").join("missing.db");
    assert!(GlobalDb::open_read_only_at(&missing_path).await.is_none());
    assert!(
        !missing_path.exists(),
        "a read-only open must not create an absent store"
    );

    let reader = GlobalDb::open_read_only_at(&restart_path)
        .await
        .expect("existing temporal schema should open read-only");
    drop(reader);
    assert_eq!(
        row_count(&restart_path, "session_query_cursor_keys").await,
        1,
        "read-only opens must not create or rotate cursor keys"
    );
}

#[tokio::test]
async fn temporal_schema_rejects_cross_session_and_generation_rows() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);
    assert!(
        table_exists(&db_path, "session_temporal_generations").await,
        "the temporal generation owner table must exist before ownership checks"
    );

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .await
        .unwrap();
    conn.execute_batch(
        "INSERT INTO sanitization_receipts (
            receipt_id, sanitizer_version, payload_digest, receipt_json
         )
         VALUES ('receipt-one', 'test', 'digest-one', '{}');
         INSERT INTO observations (
            observation_id, payload_digest, receipt_id, observation_json, committed_cursor_json
         )
         VALUES ('observation-one', 'digest-one', 'receipt-one', '{}', '{}');
         INSERT INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         )
         VALUES ('anchor-one', '{}', '{}', 'test');
         INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         )
         VALUES
            ('session-one', 1, 'building', '{}', 100),
            ('session-one', 2, 'building', '{}', 100),
            ('session-two', 1, 'building', '{}', 100);
         INSERT INTO session_turns (
            session_id, generation, turn_id, ordinal, grouping_provenance, created_at
         )
         VALUES ('session-one', 1, 'turn-one', 0, 'provider', 100);
         INSERT INTO session_occurrences (
            session_id, generation, occurrence_id, source_observation_id,
            projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
            valid_time_json, evidence_json, snippet_text, index_text
         )
         VALUES
            ('session-one', 1, 'occurrence-one', 'observation-one',
             0, 'anchor-one', 'assistant', 100,
             json_object('kind', 'unknown'), '{}', 'one', 'one'),
            ('session-one', 2, 'occurrence-two', 'observation-one',
             0, 'anchor-one', 'assistant', 100,
             json_object('kind', 'unknown'), '{}', 'two', 'two'),
            ('session-two', 1, 'occurrence-three', 'observation-one',
             0, 'anchor-one', 'assistant', 100,
             json_object('kind', 'unknown'), '{}', 'three', 'three');",
    )
    .await
    .unwrap();

    let cross_session = conn
        .execute(
            "INSERT INTO session_turn_members (
                session_id, generation, turn_id, occurrence_id, ordinal
             )
             VALUES ('session-one', 1, 'turn-one', 'occurrence-three', 0)",
            (),
        )
        .await;
    assert!(
        cross_session.is_err(),
        "a Turn cannot own an occurrence from another session"
    );

    let cross_generation = conn
        .execute(
            "INSERT INTO session_turn_members (
                session_id, generation, turn_id, occurrence_id, ordinal
             )
             VALUES ('session-one', 1, 'turn-one', 'occurrence-two', 0)",
            (),
        )
        .await;
    assert!(
        cross_generation.is_err(),
        "a Turn cannot own an occurrence from another generation"
    );

    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'ready', ready_at = 101
         WHERE session_id = 'session-one' AND generation = 1",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'active', activated_at = 102
         WHERE session_id = 'session-one' AND generation = 1",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'ready', ready_at = 101
         WHERE session_id = 'session-one' AND generation = 2",
        (),
    )
    .await
    .unwrap();
    let second_active = conn
        .execute(
            "UPDATE session_temporal_generations
             SET state = 'active'
             WHERE session_id = 'session-one' AND generation = 2",
            (),
        )
        .await;
    assert!(
        second_active.is_err(),
        "only one active temporal generation is allowed per session"
    );
}

#[tokio::test]
async fn temporal_schema_rejects_invalid_current_assertion_and_valid_time_rows() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         INSERT INTO sanitization_receipts (
            receipt_id, sanitizer_version, payload_digest, receipt_json
         )
         VALUES ('receipt-one', 'test', 'digest-one', '{}');
         INSERT INTO observations (
            observation_id, payload_digest, receipt_id, observation_json, committed_cursor_json
         )
         VALUES ('observation-one', 'digest-one', 'receipt-one', '{}', '{}');
         INSERT INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         )
         VALUES
            ('anchor-subject', '{}', '{}', 'test'),
            ('anchor-object', '{}', '{}', 'test');
         INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         )
         VALUES ('session-one', 1, 'building', '{}', 100);
         INSERT INTO session_occurrences (
            session_id, generation, occurrence_id, source_observation_id,
            projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
            valid_time_json, evidence_json, snippet_text, index_text
         )
         VALUES (
            'session-one', 1, 'occurrence-one', 'observation-one',
            0, 'anchor-subject', 'assistant', 100,
            json_object('kind', 'known', 'valid_at', 100), '{}', 'one', 'one'
         );
         INSERT INTO session_assertions (
            session_id, generation, assertion_id, assertion_kind,
            subject_anchor_id, object_anchor_id, knowledge_at,
            valid_time_json, evidence_json
         )
         VALUES (
            'session-one', 1, 'assertion-one', 'corrects',
            'anchor-subject', 'anchor-object', 100,
            json_object('kind', 'known', 'valid_at', 100), '{}'
         );
         INSERT INTO session_current_entities (
            session_id, generation, entity_kind, entity_id,
            current_occurrence_id, coverage_json
         )
         VALUES (
            'session-one', 1, 'occurrence_anchor', 'anchor-subject',
            'occurrence-one', '{}'
         );",
    )
    .await
    .unwrap();

    for (sql, description) in [
        (
            "INSERT INTO session_current_entities (
                 session_id, generation, entity_kind, entity_id,
                 current_assertion_id, coverage_json
             )
             VALUES (
                 'session-one', 1, 'unsupported', 'anchor-subject',
                 'assertion-one', '{}'
             )",
            "current entities must use a typed entity kind",
        ),
        (
            "INSERT INTO session_current_entities (
                 session_id, generation, entity_kind, entity_id,
                 current_assertion_id, current_occurrence_id, coverage_json
             )
             VALUES (
                 'session-one', 1, 'occurrence_anchor', 'anchor-both',
                 'assertion-one', 'occurrence-one', '{}'
             )",
            "current entities must point to exactly one typed target",
        ),
        (
            "INSERT INTO session_assertions (
                 session_id, generation, assertion_id, assertion_kind,
                 subject_anchor_id, object_anchor_id, knowledge_at,
                 valid_time_json, evidence_json
             )
             VALUES (
                 'session-one', 1, 'assertion-invalid-kind', 'unsupported',
                 'anchor-subject', 'anchor-object', 100,
                 json_object('kind', 'known', 'valid_at', 100), '{}'
             )",
            "assertions must use a typed assertion kind",
        ),
        (
            "INSERT INTO session_assertions (
                 session_id, generation, assertion_id, assertion_kind,
                 subject_anchor_id, object_anchor_id, knowledge_at,
                 valid_time_json, evidence_json
             )
             VALUES (
                 'session-one', 1, 'assertion-invalid-time', 'corrects',
                 'anchor-subject', 'anchor-object', 100,
                 json_object('kind', 'known'), '{}'
             )",
            "known assertion valid time must include an integer valid_at",
        ),
        (
            "INSERT INTO session_occurrences (
                 session_id, generation, occurrence_id, source_observation_id,
                 projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
                 valid_time_json, evidence_json, snippet_text, index_text
             )
             VALUES (
                 'session-one', 1, 'occurrence-invalid-time', 'observation-one',
                 1, 'anchor-subject', 'assistant', 101,
                 json_object('kind', 'unknown', 'valid_at', 101), '{}', 'bad', 'bad'
             )",
            "unknown occurrence valid time must not include valid_at",
        ),
    ] {
        assert!(
            conn.execute(sql, ()).await.is_err(),
            "schema accepted an invalid row: {description}"
        );
    }
}

#[tokio::test]
async fn temporal_schema_query_indexes_cover_exact_lookup_shapes() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    for (sql, index) in [
        (
            "SELECT occurrence_id
             FROM session_occurrences
             WHERE session_id = 'session-one'
               AND generation = 1
               AND retrieval_anchor_id = 'anchor-one'
               AND knowledge_at >= 0
             ORDER BY knowledge_at, occurrence_id",
            "idx_session_occurrences_anchor_order",
        ),
        (
            "SELECT occurrence_id
             FROM session_occurrences
             WHERE session_id = 'session-one'
               AND generation = 1
               AND message_id = 'message-one'
               AND knowledge_at >= 0
             ORDER BY knowledge_at, occurrence_id",
            "idx_session_occurrences_message",
        ),
        (
            "SELECT occurrence_id
             FROM session_occurrences
             WHERE session_id = 'session-one'
               AND generation = 1
               AND thread_id = 'thread-one'
               AND knowledge_at >= 0
             ORDER BY knowledge_at, occurrence_id",
            "idx_session_occurrences_thread",
        ),
        (
            "SELECT occurrence_id
             FROM session_occurrences
             WHERE session_id = 'session-one'
               AND generation = 1
               AND turn_id = 'turn-one'
               AND knowledge_at >= 0
             ORDER BY knowledge_at, occurrence_id",
            "idx_session_occurrences_turn",
        ),
        (
            "SELECT occurrence_id
             FROM session_occurrences
             WHERE session_id = 'session-one'
               AND generation = 1
               AND agent_id = 'agent-one'
               AND knowledge_at >= 0
             ORDER BY knowledge_at, occurrence_id",
            "idx_session_occurrences_agent",
        ),
        (
            "SELECT entity_id
             FROM session_current_entities
             WHERE session_id = 'session-one'
               AND generation = 1
               AND current_occurrence_id = 'occurrence-one'",
            "idx_session_current_entities_occurrence",
        ),
        (
            "SELECT assertion_id
             FROM session_assertions
             WHERE session_id = 'session-one'
               AND generation = 1
               AND object_anchor_id = 'anchor-object'
               AND knowledge_at >= 0
             ORDER BY knowledge_at, assertion_id",
            "idx_session_assertions_object_order",
        ),
        (
            "SELECT assertion_id
             FROM session_assertions
             WHERE session_id = 'session-one'
               AND generation = 1
               AND assertion_kind = 'corrects'
               AND knowledge_at >= 0
             ORDER BY knowledge_at, assertion_id",
            "idx_session_assertions_kind_order",
        ),
        (
            "SELECT assertion_id
             FROM session_assertions
             WHERE session_id = 'session-one'
               AND generation = 1
               AND knowledge_at >= 0
             ORDER BY knowledge_at, assertion_id",
            "idx_session_assertions_generation_order",
        ),
        (
            "SELECT summary_id
             FROM session_summary_sources
             WHERE source_summary_id = 'summary-one'
             ORDER BY summary_id",
            "idx_session_summary_sources_summary",
        ),
        (
            "SELECT predecessor_summary_id
             FROM session_summary_successors
             WHERE successor_summary_id = 'summary-one'
             ORDER BY created_at DESC, predecessor_summary_id",
            "idx_session_summary_successors_successor",
        ),
        (
            "SELECT payload_ref
             FROM session_external_payload_manifests
             WHERE session_id = 'session-one'",
            "idx_session_external_payload_manifests_session",
        ),
    ] {
        let details = explain_query_plan(&conn, sql).await;
        assert!(
            details.iter().any(|detail| detail.contains(index)),
            "EXPLAIN did not use {index} for `{sql}`: {details:?}"
        );
    }
}

#[tokio::test]
async fn temporal_schema_root_retrieval_indexes_cover_catalog_and_large_query_shapes() {
    const OCCURRENCE_ROOT_INDEX: &str = "idx_session_occurrences_root_generation_order";
    const SUMMARY_ROOT_INDEX: &str = "idx_session_summary_nodes_root_created_order";

    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    for (table, index, expected_columns, root_prefix) in [
        (
            "session_occurrences",
            OCCURRENCE_ROOT_INDEX,
            &["knowledge_at", "session_id", "occurrence_id", "generation"][..],
            "idx_session_occurrences_root_",
        ),
        (
            "session_summary_nodes",
            SUMMARY_ROOT_INDEX,
            &["created_at", "session_id", "summary_id"][..],
            "idx_session_summary_nodes_root_",
        ),
    ] {
        let expected = expected_columns
            .iter()
            .map(|column| ((*column).to_owned(), 0))
            .collect::<Vec<_>>();
        assert_eq!(
            index_key_columns(&conn, index).await,
            expected,
            "{index} must retain its exact ascending key contract"
        );

        let index_names = table_index_names(&conn, table).await;
        let mut matching_keysets = Vec::new();
        for candidate in &index_names {
            if index_key_columns(&conn, candidate).await == expected {
                matching_keysets.push(candidate.clone());
            }
        }
        assert_eq!(
            matching_keysets,
            [index.to_owned()],
            "{table} must not retain a redundant index with the root keyset"
        );

        let root_indexes = index_names
            .into_iter()
            .filter(|candidate| candidate.starts_with(root_prefix))
            .collect::<Vec<_>>();
        assert_eq!(
            root_indexes,
            [index.to_owned()],
            "{table} must not retain a conflicting root index variant"
        );
    }

    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         INSERT INTO sanitization_receipts (
            receipt_id, sanitizer_version, payload_digest, receipt_json
         ) VALUES ('root-receipt', 'test', 'root-digest', '{}');
         INSERT INTO observations (
            observation_id, payload_digest, receipt_id, observation_json, committed_cursor_json
         ) VALUES (
            'root-observation',
            'root-digest',
            'root-receipt',
            '{\"identity\":{\"source\":{\"provider\":\"claude\"}}}',
            '{}'
         );
         INSERT INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES ('root-anchor', '{}', '{}', 'test');
         WITH RECURSIVE sequence(value) AS (
            VALUES(0)
            UNION ALL
            SELECT value + 1 FROM sequence WHERE value < 7
         )
         INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         )
         SELECT printf('root-session-%02d', value), 1, 'building', '{}', 0
         FROM sequence;
         UPDATE session_temporal_generations
         SET state = 'ready', ready_at = 1;
         UPDATE session_temporal_generations
         SET state = 'active', activated_at = 2;
         WITH RECURSIVE sequence(value) AS (
            VALUES(0)
            UNION ALL
            SELECT value + 1 FROM sequence WHERE value < 99999
         )
         INSERT INTO session_occurrences (
            session_id, generation, occurrence_id, source_observation_id,
            projection_output_ordinal, retrieval_anchor_id, role, knowledge_at,
            valid_time_json, evidence_json, snippet_text, index_text
         )
         SELECT
            printf('root-session-%02d', value % 8),
            1,
            printf('root-occurrence-%06d', value),
            'root-observation',
            value,
            'root-anchor',
            'assistant',
            value / 8,
            json_object('kind', 'unknown'),
            '{}',
            'root occurrence',
            'root occurrence'
         FROM sequence;
         WITH RECURSIVE sequence(value) AS (
            VALUES(0)
            UNION ALL
            SELECT value + 1 FROM sequence WHERE value < 99999
         )
         INSERT INTO session_summary_nodes (
            summary_id, session_id, summary_anchor_id, summary_text, index_text,
            source_horizon_json, created_at
         )
         SELECT
            printf('root-summary-%06d', value),
            printf('root-session-%02d', value % 8),
            'root-anchor',
            'root summary',
            'root summary',
            '{}',
            value / 8
         FROM sequence;
         ANALYZE;",
    )
    .await
    .unwrap();

    for table in ["session_occurrences", "session_summary_nodes"] {
        let mut rows = conn
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .unwrap();
        let count: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(count, 100_000, "{table} fixture must stay planner-scale");
    }

    for (shape, sql, index) in [
        (
            "root occurrence candidate",
            "SELECT o.occurrence_id
             FROM session_temporal_generations AS frozen
             JOIN session_occurrences AS o
               INDEXED BY idx_session_occurrences_root_generation_order
               ON o.session_id = frozen.session_id
              AND o.generation = frozen.generation
             JOIN observations AS provider_observation
               ON provider_observation.observation_id = o.source_observation_id
             WHERE frozen.state = 'active'
               AND (NULL IS NULL OR json_extract(
                   provider_observation.observation_json, '$.identity.source.provider'
               ) = NULL)
               AND o.knowledge_at >= 0
               AND o.knowledge_at < 12500
               AND (
                   o.knowledge_at < 9223372036854775807
                   OR (
                       o.knowledge_at = 9223372036854775807
                       AND (
                           o.session_id > ''
                           OR (o.session_id = '' AND o.occurrence_id > '')
                       )
                   )
               )
             ORDER BY o.knowledge_at DESC, o.session_id, o.occurrence_id
             LIMIT 38",
            OCCURRENCE_ROOT_INDEX,
        ),
        (
            "root occurrence pagination",
            "SELECT o.occurrence_id
             FROM session_temporal_generations AS frozen
             JOIN session_occurrences AS o
               INDEXED BY idx_session_occurrences_root_generation_order
               ON o.session_id = frozen.session_id
              AND o.generation = frozen.generation
             JOIN observations AS provider_observation
               ON provider_observation.observation_id = o.source_observation_id
             WHERE frozen.state = 'active'
               AND (NULL IS NULL OR json_extract(
                   provider_observation.observation_json, '$.identity.source.provider'
               ) = NULL)
               AND o.knowledge_at >= 0
               AND o.knowledge_at < 12500
               AND (
                   o.knowledge_at < 7111
                   OR (
                       o.knowledge_at = 7111
                       AND (
                           o.session_id > 'root-session-03'
                           OR (
                               o.session_id = 'root-session-03'
                               AND o.occurrence_id > 'root-occurrence-057000'
                           )
                       )
                   )
               )
             ORDER BY o.knowledge_at DESC, o.session_id, o.occurrence_id
             LIMIT 38",
            OCCURRENCE_ROOT_INDEX,
        ),
        (
            "root occurrence provider filter",
            "SELECT o.occurrence_id
             FROM session_temporal_generations AS frozen
             JOIN session_occurrences AS o
               INDEXED BY idx_session_occurrences_root_generation_order
               ON o.session_id = frozen.session_id
              AND o.generation = frozen.generation
             JOIN observations AS provider_observation
               ON provider_observation.observation_id = o.source_observation_id
             WHERE frozen.state = 'active'
               AND ('claude' IS NULL OR json_extract(
                   provider_observation.observation_json, '$.identity.source.provider'
               ) = 'claude')
               AND o.knowledge_at >= 0
               AND o.knowledge_at < 12500
               AND (
                   o.knowledge_at < 7111
                   OR (
                       o.knowledge_at = 7111
                       AND (
                           o.session_id > 'root-session-03'
                           OR (
                               o.session_id = 'root-session-03'
                               AND o.occurrence_id > 'root-occurrence-057000'
                           )
                       )
                   )
               )
             ORDER BY o.knowledge_at DESC, o.session_id, o.occurrence_id
             LIMIT 38",
            OCCURRENCE_ROOT_INDEX,
        ),
        (
            "root summary candidate",
            "SELECT summary_id
             FROM session_summary_nodes
             WHERE created_at >= 0
               AND created_at < 12500
               AND (
                   created_at < 7111
                   OR (
                       created_at = 7111
                       AND (
                           session_id > 'root-session-03'
                           OR (
                               session_id = 'root-session-03'
                               AND summary_id > 'root-summary-057000'
                           )
                       )
                   )
               )
             ORDER BY created_at DESC, session_id, summary_id
             LIMIT 38",
            SUMMARY_ROOT_INDEX,
        ),
    ] {
        let details = explain_query_plan(&conn, sql).await;
        assert!(
            details.iter().any(|detail| detail.contains(index)),
            "EXPLAIN did not use {index} for {shape}: {details:?}"
        );
    }
}

#[tokio::test]
async fn temporal_schema_drops_redundant_receipt_and_progress_indexes() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_session_refresh_progress_operation
             ON session_refresh_progress(session_id, operation_id, progress_ordinal);
         CREATE INDEX IF NOT EXISTS idx_session_temporal_projection_receipts_digest
             ON session_temporal_projection_receipts(session_id, generation, batch_digest);",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let reopened = GlobalDb::try_open_at(&db_path)
        .await
        .expect("current-version temporal schema should reopen");
    drop(reopened);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    for index in [
        "idx_session_refresh_progress_operation",
        "idx_session_temporal_projection_receipts_digest",
    ] {
        let mut rows = conn
            .query(
                "SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1",
                libsql::params![index],
            )
            .await
            .unwrap();
        assert!(
            rows.next().await.unwrap().is_none(),
            "{index} duplicates an exact primary-key or unique-key prefix"
        );
    }
}

#[tokio::test]
async fn temporal_schema_rejects_malformed_fts_atomically() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE session_occurrences_fts (
            index_text TEXT NOT NULL,
            snippet_text TEXT NOT NULL
        );",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    assert!(
        GlobalDb::try_open_at(&db_path).await.is_err(),
        "matching columns on an ordinary table must not impersonate the temporal FTS contract"
    );
    assert!(
        !table_exists(&db_path, "session_temporal_schema_migrations").await,
        "FTS validation failure must roll back the temporal marker"
    );
    assert!(
        !table_exists(&db_path, "session_summary_nodes").await,
        "FTS validation failure must roll back every newly-created temporal authority table"
    );
}

#[tokio::test]
async fn temporal_schema_rebuilds_existing_rows_into_exact_fts_contracts() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute_batch(
        "DROP TRIGGER session_summary_nodes_fts_insert_v1;
         DROP TRIGGER session_summary_nodes_fts_delete_v1;
         DROP TRIGGER session_summary_nodes_fts_update_v1;
         DROP TABLE session_summary_nodes_fts;
         INSERT INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES ('fts-anchor', '{}', '{}', 'test');
         INSERT INTO session_summary_nodes (
            summary_id, session_id, summary_anchor_id, summary_text, index_text,
            source_horizon_json, created_at
         ) VALUES (
            'fts-summary', 'fts-session', 'fts-anchor',
            'existing summary', 'migration-search summary', '{}', 100
         );",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let reopened = GlobalDb::try_open_at(&db_path)
        .await
        .expect("missing temporal FTS objects should be rebuilt")
        .expect("global database should reopen");
    drop(reopened);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    for (table, expected_content) in [("session_summary_nodes_fts", "session_summary_nodes")] {
        let mut rows = conn
            .query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                libsql::params![table],
            )
            .await
            .unwrap();
        let sql: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let normalized = sql.to_ascii_lowercase().replace(char::is_whitespace, "");
        assert!(normalized.contains("createvirtualtable"));
        assert!(normalized.contains("usingfts5("));
        assert!(normalized.contains(&format!("content='{expected_content}'")));
        assert!(normalized.contains("content_rowid='rowid'"));

        let query = format!("SELECT COUNT(*) FROM {table} WHERE {table} MATCH 'migration'");
        let mut matches = conn.query(&query, ()).await.unwrap();
        let count: i64 = matches.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(count, 1, "migration must rebuild existing rows for {table}");
    }
}

#[tokio::test]
async fn temporal_schema_enforces_refresh_progress_and_terminal_receipts() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute_batch(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('refresh-session', 1, 'building', '{}', 100);
         INSERT INTO session_refresh_operations (
            session_id, operation_id, request_digest, target_frontier_json,
            state, created_at, updated_at
         ) VALUES (
            'refresh-session', 'refresh-one',
            'sha256:0000000000000000000000000000000000000000000000000000000000000000',
            '{\"observed_through\":10,\"committed_through\":4}',
            'running', 100, 100
         );
         INSERT INTO session_refresh_bindings (
            session_id, operation_id, scope_kind, source_frontier, target_frontier,
            projector_version, config_digest, generation, frozen_watermarks_json,
            binding_digest, created_at
         ) VALUES (
            'refresh-session', 'refresh-one', 'session_store', 4, 10,
            'session-temporal-projector.v1',
            'sha256:0000000000000000000000000000000000000000000000000000000000000000',
            1, '{}',
            'sha256:0000000000000000000000000000000000000000000000000000000000000000',
            100
         );",
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_operations (
                session_id, operation_id, request_digest, target_frontier_json,
                state, created_at, updated_at, terminal_at
             ) VALUES (
                'refresh-session', 'bad-start', 'digest',
                '{\"observed_through\":10,\"committed_through\":10}',
                'complete', 100, 101, 101
             )",
            (),
        )
        .await
        .is_err(),
        "refresh operations must start in running"
    );
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_progress (
                session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
                committed_batches, committed_records, recorded_at
             ) VALUES (
                'refresh-session', 'refresh-one', 0,
                '{\"observed_through\":10,\"committed_through\":4}',
                '{\"visible\":1,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
                1, 1, 99
             )",
            (),
        )
        .await
        .is_err(),
        "first progress cannot predate its owning operation"
    );
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_progress (
                session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
                committed_batches, committed_records, recorded_at
             ) VALUES (
                'refresh-session', 'refresh-one', 0,
                '{\"observed_through\":10,\"committed_through\":4}',
                '{\"visible\":1,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
                1, 1, 101
             )",
            (),
        )
        .await
        .is_err(),
        "progress without the operation generation's projection receipt must be rejected"
    );
    conn.execute(
        "INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'refresh-session', 1, 0,
            'sha256:1000000000000000000000000000000000000000000000000000000000000000',
            '{}', 4, 4,
            0, 'sha256:1100000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1200000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1300000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1400000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1500000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1600000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:1700000000000000000000000000000000000000000000000000000000000000',
            101
         )",
        (),
    )
    .await
    .unwrap();
    for (label, frontier, coverage) in [
        (
            "source minus one",
            "{\"observed_through\":10,\"committed_through\":3}",
            "{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}",
        ),
        (
            "target plus one",
            "{\"observed_through\":10,\"committed_through\":11}",
            "{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}",
        ),
        (
            "missing coverage fields",
            "{\"observed_through\":10,\"committed_through\":4}",
            "{}",
        ),
    ] {
        let sql = format!(
            "INSERT INTO session_refresh_progress (
                session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
                committed_batches, committed_records, recorded_at
             ) VALUES (
                'refresh-session', 'refresh-one', 0, '{frontier}', '{coverage}', 1, 0, 101
             )"
        );
        assert!(
            conn.execute(&sql, ()).await.is_err(),
            "{label} must be rejected"
        );
    }
    conn.execute(
        "INSERT INTO session_refresh_progress (
            session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
            committed_batches, committed_records, recorded_at
         ) VALUES (
            'refresh-session', 'refresh-one', 0,
            '{\"observed_through\":10,\"committed_through\":4}',
            '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            1, 0, 101
         )",
        (),
    )
    .await
    .expect("first progress may commit at the binding source frontier (noop endpoint)");
    conn.execute(
        "INSERT INTO session_refresh_batch_bindings (
            session_id, operation_id, progress_ordinal, generation, batch_ordinal
         ) VALUES ('refresh-session', 'refresh-one', 0, 1, 0)",
        (),
    )
    .await
    .unwrap();
    conn.execute_batch(
        "INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'refresh-session', 1, 1,
            'sha256:2000000000000000000000000000000000000000000000000000000000000000',
            '{}', 4, 10,
            0, 'sha256:2100000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2200000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2300000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2400000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2500000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2600000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2700000000000000000000000000000000000000000000000000000000000000',
            102
         );
         INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'refresh-session', 1, 2,
            'sha256:2900000000000000000000000000000000000000000000000000000000000000',
            '{}', 10, 10,
            0, 'sha256:2910000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2920000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2930000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2940000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2950000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2960000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:2970000000000000000000000000000000000000000000000000000000000000',
            103
         );",
    )
    .await
    .unwrap();

    for (label, values) in [
        (
            "batch regression",
            "1, '{\"observed_through\":10,\"committed_through\":5}',
             '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}', 1, 0, 102",
        ),
        (
            "record forge",
            "1, '{\"observed_through\":10,\"committed_through\":5}',
             '{\"visible\":1,\"hidden\":0,\"unknown\":0,\"redacted\":0}', 2, 1, 102",
        ),
        (
            "frontier regression",
            "1, '{\"observed_through\":10,\"committed_through\":3}',
             '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}', 2, 0, 102",
        ),
        (
            "timestamp regression",
            "1, '{\"observed_through\":10,\"committed_through\":5}',
             '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}', 2, 0, 100",
        ),
        (
            "ordinal gap",
            "2, '{\"observed_through\":10,\"committed_through\":10}',
             '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}', 3, 0, 103",
        ),
    ] {
        let sql = format!(
            "INSERT INTO session_refresh_progress (
                session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
                committed_batches, committed_records, recorded_at
             ) VALUES ('refresh-session', 'refresh-one', {values})"
        );
        assert!(
            conn.execute(&sql, ()).await.is_err(),
            "{label} must be rejected"
        );
    }
    conn.execute(
        "INSERT INTO session_refresh_progress (
            session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
            committed_batches, committed_records, recorded_at
         ) VALUES (
            'refresh-session', 'refresh-one', 1,
            '{\"observed_through\":10,\"committed_through\":10}',
            '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            2, 0, 102
         )",
        (),
    )
    .await
    .expect(
        "subsequent progress may keep receipt.source_through at the prior committed endpoint while projection_through advances",
    );
    conn.execute_batch(
        "INSERT INTO session_refresh_batch_bindings (
            session_id, operation_id, progress_ordinal, generation, batch_ordinal
         ) VALUES ('refresh-session', 'refresh-one', 1, 1, 1);
         UPDATE session_temporal_generations
         SET state = 'ready', ready_at = 103
         WHERE session_id = 'refresh-session' AND generation = 1;
         UPDATE session_temporal_generations
         SET state = 'active', activated_at = 104
         WHERE session_id = 'refresh-session' AND generation = 1;
         UPDATE session_refresh_operations
         SET state = 'complete', updated_at = 104, terminal_at = 104
         WHERE session_id = 'refresh-session' AND operation_id = 'refresh-one';",
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_progress (
                session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
                committed_batches, committed_records, recorded_at
             ) VALUES (
                'refresh-session', 'refresh-one', 2,
                '{\"observed_through\":10,\"committed_through\":10}',
                '{\"visible\":2,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
                3, 2, 105
             )",
            (),
        )
        .await
        .is_err(),
        "terminal operations cannot append progress"
    );

    for (label, terminal_state, frontier, coverage, terminal_at, failure_code) in [
        (
            "state mismatch",
            "failed",
            "{\"observed_through\":10,\"committed_through\":10}",
            "{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}",
            104,
            Some("boom"),
        ),
        (
            "frontier mismatch",
            "complete",
            "{\"observed_through\":10,\"committed_through\":9}",
            "{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}",
            104,
            None,
        ),
        (
            "coverage mismatch",
            "complete",
            "{\"observed_through\":10,\"committed_through\":10}",
            "{\"visible\":1,\"hidden\":0,\"unknown\":0,\"redacted\":0}",
            104,
            None,
        ),
        (
            "timestamp mismatch",
            "complete",
            "{\"observed_through\":10,\"committed_through\":10}",
            "{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}",
            105,
            None,
        ),
    ] {
        let failure_code = failure_code
            .map(|code| format!("'{code}'"))
            .unwrap_or_else(|| "NULL".to_string());
        let sql = format!(
            "INSERT INTO session_refresh_receipts (
                session_id, operation_id, terminal_state, frontier_json,
                coverage_json, failure_code, terminal_at
             ) VALUES (
                'refresh-session', 'refresh-one', '{terminal_state}',
                '{frontier}', '{coverage}', {failure_code}, {terminal_at}
             )"
        );
        assert!(
            conn.execute(&sql, ()).await.is_err(),
            "{label} must be rejected"
        );
    }
    conn.execute(
        "INSERT INTO session_refresh_receipts (
            session_id, operation_id, terminal_state, frontier_json,
            coverage_json, failure_code, terminal_at
         ) VALUES (
            'refresh-session', 'refresh-one', 'complete',
            '{\"observed_through\":10,\"committed_through\":10}',
            '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            NULL, 104
         )",
        (),
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_refresh_receipts SET terminal_at = 104
             WHERE session_id = 'refresh-session' AND operation_id = 'refresh-one'",
            (),
        )
        .await
        .is_err()
    );
    assert!(
        conn.execute(
            "DELETE FROM session_refresh_receipts
             WHERE session_id = 'refresh-session' AND operation_id = 'refresh-one'",
            (),
        )
        .await
        .is_err()
    );

    conn.execute_batch(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('refresh-session', 2, 'building', '{}', 200);
         INSERT INTO session_refresh_operations (
            session_id, operation_id, request_digest, target_frontier_json,
            state, created_at, updated_at
         ) VALUES (
            'refresh-session', 'refresh-failed',
            'sha256:3000000000000000000000000000000000000000000000000000000000000000',
            '{\"observed_through\":10,\"committed_through\":4}',
            'running', 200, 200
         );
         INSERT INTO session_refresh_bindings (
            session_id, operation_id, scope_kind, source_frontier, target_frontier,
            projector_version, config_digest, generation, frozen_watermarks_json,
            binding_digest, created_at
         ) VALUES (
            'refresh-session', 'refresh-failed', 'session_store', 4, 10,
            'session-temporal-projector.v1',
            'sha256:3000000000000000000000000000000000000000000000000000000000000000',
            2, '{}',
            'sha256:3000000000000000000000000000000000000000000000000000000000000000',
            200
         );",
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_progress (
                session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
                committed_batches, committed_records, recorded_at
             ) VALUES (
                'refresh-session', 'refresh-failed', 0,
                '{\"observed_through\":10,\"committed_through\":4}',
                '{\"visible\":1,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
                1, 1, 200
             )",
            (),
        )
        .await
        .is_err(),
        "a progress row cannot borrow another operation's generation receipt"
    );
    conn.execute(
        "INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'refresh-session', 2, 0,
            'sha256:4000000000000000000000000000000000000000000000000000000000000000',
            '{}', 4, 4,
            0, 'sha256:4100000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:4200000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:4300000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:4400000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:4500000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:4600000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:4700000000000000000000000000000000000000000000000000000000000000',
            200
         )",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO session_refresh_progress (
            session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
            committed_batches, committed_records, recorded_at
         ) VALUES (
            'refresh-session', 'refresh-failed', 0,
            '{\"observed_through\":10,\"committed_through\":4}',
            '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            1, 0, 200
         )",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO session_refresh_batch_bindings (
            session_id, operation_id, progress_ordinal, generation, batch_ordinal
         ) VALUES ('refresh-session', 'refresh-failed', 0, 2, 0)",
        (),
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_refresh_operations
             SET state = 'cancelled', updated_at = 201, terminal_at = 201,
                 failure_code = 'must-not-survive'
             WHERE session_id = 'refresh-session' AND operation_id = 'refresh-failed'",
            (),
        )
        .await
        .is_err(),
        "cancelled operations cannot carry a failure code"
    );
    conn.execute_batch(
        "UPDATE session_temporal_generations
         SET state = 'failed', completed_at = 201
         WHERE session_id = 'refresh-session' AND generation = 2;
         UPDATE session_refresh_operations
         SET state = 'failed', updated_at = 201, terminal_at = 201, failure_code = 'boom'
         WHERE session_id = 'refresh-session' AND operation_id = 'refresh-failed';",
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_receipts (
                session_id, operation_id, terminal_state, frontier_json,
                coverage_json, failure_code, terminal_at
             ) VALUES (
                'refresh-session', 'refresh-failed', 'failed',
                '{\"observed_through\":11,\"committed_through\":4}',
                '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
                'boom', 201
             )",
            (),
        )
        .await
        .is_err(),
        "terminal receipt frontiers cannot exceed the owning target frontier"
    );
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_receipts (
                session_id, operation_id, terminal_state, frontier_json,
                coverage_json, failure_code, terminal_at
             ) VALUES (
                'refresh-session', 'refresh-failed', 'failed',
                '{\"observed_through\":10,\"committed_through\":4}',
                '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
                'other', 201
             )",
            (),
        )
        .await
        .is_err(),
        "terminal receipt failure codes must match the owning operation"
    );
    conn.execute(
        "INSERT INTO session_refresh_receipts (
            session_id, operation_id, terminal_state, frontier_json,
            coverage_json, failure_code, terminal_at
         ) VALUES (
            'refresh-session', 'refresh-failed', 'failed',
            '{\"observed_through\":10,\"committed_through\":4}',
            '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            'boom', 201
         )",
        (),
    )
    .await
    .unwrap();

    conn.execute_batch(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('refresh-zero', 1, 'building', '{}', 240);
         INSERT INTO session_refresh_operations (
            session_id, operation_id, request_digest, target_frontier_json,
            state, created_at, updated_at
         ) VALUES (
            'refresh-zero', 'zero-noop',
            'sha256:3200000000000000000000000000000000000000000000000000000000000000',
            '{\"observed_through\":0,\"committed_through\":0}',
            'running', 240, 240
         );
         INSERT INTO session_refresh_bindings (
            session_id, operation_id, scope_kind, source_frontier, target_frontier,
            projector_version, config_digest, generation, frozen_watermarks_json,
            binding_digest, created_at
         ) VALUES (
            'refresh-zero', 'zero-noop', 'session_store', 0, 0,
            'session-temporal-projector.v1',
            'sha256:3200000000000000000000000000000000000000000000000000000000000000',
            1, '{}',
            'sha256:3200000000000000000000000000000000000000000000000000000000000000',
            240
         );
         INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'refresh-zero', 1, 0,
            'sha256:3300000000000000000000000000000000000000000000000000000000000000',
            '{}', 0, 0,
            0, 'sha256:3310000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3320000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3330000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3340000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3350000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3360000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3370000000000000000000000000000000000000000000000000000000000000',
            240
         );",
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO session_refresh_progress (
            session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
            committed_batches, committed_records, recorded_at
         ) VALUES (
            'refresh-zero', 'zero-noop', 0,
            '{\"observed_through\":0,\"committed_through\":0}',
            '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            1, 0, 240
         )",
        (),
    )
    .await
    .expect("zero-frontier empty first progress is a legal noop endpoint");

    conn.execute_batch(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('refresh-over-source', 1, 'building', '{}', 245);
         INSERT INTO session_refresh_operations (
            session_id, operation_id, request_digest, target_frontier_json,
            state, created_at, updated_at
         ) VALUES (
            'refresh-over-source', 'over-source',
            'sha256:3400000000000000000000000000000000000000000000000000000000000000',
            '{\"observed_through\":2,\"committed_through\":0}',
            'running', 245, 245
         );
         INSERT INTO session_refresh_bindings (
            session_id, operation_id, scope_kind, source_frontier, target_frontier,
            projector_version, config_digest, generation, frozen_watermarks_json,
            binding_digest, created_at
         ) VALUES (
            'refresh-over-source', 'over-source', 'session_store', 0, 2,
            'session-temporal-projector.v1',
            'sha256:3400000000000000000000000000000000000000000000000000000000000000',
            1, '{}',
            'sha256:3400000000000000000000000000000000000000000000000000000000000000',
            245
         );
         INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest, frozen_watermarks_json,
            source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'refresh-over-source', 1, 0,
            'sha256:3500000000000000000000000000000000000000000000000000000000000000',
            '{}', 1, 0,
            0, 'sha256:3510000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3520000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3530000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3540000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3550000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3560000000000000000000000000000000000000000000000000000000000000',
            0, 'sha256:3570000000000000000000000000000000000000000000000000000000000000',
            245
         );",
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO session_refresh_progress (
                session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
                committed_batches, committed_records, recorded_at
             ) VALUES (
                'refresh-over-source', 'over-source', 0,
                '{\"observed_through\":2,\"committed_through\":0}',
                '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
                1, 0, 245
             )",
            (),
        )
        .await
        .is_err(),
        "first progress must reject receipt.source_through past the committed endpoint"
    );

    conn.execute(
        "INSERT INTO session_refresh_operations (
            session_id, operation_id, request_digest, target_frontier_json,
            state, created_at, updated_at
         ) VALUES (
            'refresh-orphan', 'orphan-terminal', 'orphan-digest',
            '{\"observed_through\":1,\"committed_through\":0}',
            'running', 250, 250
         )",
        (),
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_refresh_operations
             SET state = 'failed', updated_at = 251, terminal_at = 251,
                 failure_code = 'forged'
             WHERE session_id = 'refresh-orphan' AND operation_id = 'orphan-terminal'",
            (),
        )
        .await
        .is_err(),
        "terminal operations must own a generation binding"
    );

    conn.execute_batch(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('refresh-forged-zero', 1, 'building', '{}', 300);
         INSERT INTO session_refresh_operations (
            session_id, operation_id, request_digest, target_frontier_json,
            state, created_at, updated_at
         ) VALUES (
            'refresh-forged-zero', 'forged-complete',
            'sha256:5000000000000000000000000000000000000000000000000000000000000000',
            '{\"observed_through\":5,\"committed_through\":5}',
            'running', 300, 300
         );
         INSERT INTO session_refresh_bindings (
            session_id, operation_id, scope_kind, source_frontier, target_frontier,
            projector_version, config_digest, generation, frozen_watermarks_json,
            binding_digest, created_at
         ) VALUES (
            'refresh-forged-zero', 'forged-complete', 'session_store', 5, 5,
            'session-temporal-projector.v1',
            'sha256:5000000000000000000000000000000000000000000000000000000000000000',
            1, '{}',
            'sha256:5000000000000000000000000000000000000000000000000000000000000000',
            300
         );
         INSERT INTO session_refresh_progress (
            session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
            committed_batches, committed_records, recorded_at
         ) VALUES (
            'refresh-forged-zero', 'forged-complete', 0,
            '{\"observed_through\":5,\"committed_through\":5}',
            '{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}',
            0, 0, 300
         );",
    )
    .await
    .unwrap();
    conn.execute_batch(
        "UPDATE session_temporal_generations
         SET state = 'ready', ready_at = 301
         WHERE session_id = 'refresh-forged-zero' AND generation = 1;
         UPDATE session_temporal_generations
         SET state = 'active', activated_at = 302
         WHERE session_id = 'refresh-forged-zero' AND generation = 1;",
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_refresh_operations
             SET state = 'complete', updated_at = 302, terminal_at = 302
             WHERE session_id = 'refresh-forged-zero' AND operation_id = 'forged-complete'",
            (),
        )
        .await
        .is_err(),
        "completion cannot be forged from the failure/cancellation zero-progress seed",
    );
}

#[tokio::test]
async fn temporal_schema_enforces_generation_state_machine_and_durability() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    assert!(
        conn.execute(
            "INSERT INTO session_temporal_generations (
                session_id, generation, state, frozen_watermarks_json, created_at, ready_at
             ) VALUES ('generation-session', 1, 'ready', '{}', 100, 101)",
            (),
        )
        .await
        .is_err(),
        "generation rows must start in building"
    );
    conn.execute(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES
            ('generation-session', 1, 'building', '{}', 100),
            ('generation-session', 2, 'building', '{}', 100)",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'ready', ready_at = 101
         WHERE session_id = 'generation-session' AND generation = 1",
        (),
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_temporal_generations
             SET ready_at = 102
             WHERE session_id = 'generation-session' AND generation = 1",
            (),
        )
        .await
        .is_err(),
        "same-state timestamp rewrites must be rejected"
    );
    assert!(
        conn.execute(
            "UPDATE session_temporal_generations
             SET state = 'superseded', activated_at = 102, completed_at = 103
             WHERE session_id = 'generation-session' AND generation = 1",
            (),
        )
        .await
        .is_err(),
        "ready cannot skip active"
    );
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'active', activated_at = 102
         WHERE session_id = 'generation-session' AND generation = 1",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'ready', ready_at = 101
         WHERE session_id = 'generation-session' AND generation = 2",
        (),
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_temporal_generations
             SET state = 'active', activated_at = 102
             WHERE session_id = 'generation-session' AND generation = 2",
            (),
        )
        .await
        .is_err(),
        "only one active generation is allowed"
    );
    assert!(
        conn.execute(
            "DELETE FROM session_temporal_generations
             WHERE session_id = 'generation-session' AND generation = 2",
            (),
        )
        .await
        .is_err(),
        "all generation rows are durable, including building generations"
    );
}

#[tokio::test]
async fn temporal_schema_keeps_append_only_authority_immutable() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute_batch(
        "INSERT INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES ('append-anchor', '{}', '{}', 'test');
         INSERT INTO session_summary_nodes (
            summary_id, session_id, summary_anchor_id, summary_text, index_text,
            source_horizon_json, created_at
         ) VALUES (
            'append-summary', 'append-session', 'append-anchor',
            'summary', 'summary', '{}', 100
         );
         INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('append-session', 1, 'building', '{}', 100);
         INSERT INTO session_temporal_migration_receipts (
            session_id, generation, batch_ordinal, source_digest,
            frozen_watermarks_json, imported_items, committed_at
         ) VALUES ('append-session', 1, 0, 'source', '{}', 1, 100);
         INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest,
            frozen_watermarks_json, source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            'append-session', 1, 0, 'batch', '{}', 0, 0,
            0, 'occurrence', 0, 'dimension', 0, 'copy', 0, 'assertion',
            0, 'supersession', 0, 'current', 0, 'fts', 100
         );",
    )
    .await
    .unwrap();
    for sql in [
        "UPDATE session_summary_nodes SET summary_text = 'rewrite'
         WHERE summary_id = 'append-summary'",
        "DELETE FROM session_summary_nodes WHERE summary_id = 'append-summary'",
        "UPDATE session_temporal_migration_receipts SET imported_items = 2
         WHERE session_id = 'append-session' AND generation = 1 AND batch_ordinal = 0",
        "DELETE FROM session_temporal_migration_receipts
         WHERE session_id = 'append-session' AND generation = 1 AND batch_ordinal = 0",
        "UPDATE session_temporal_projection_receipts SET fts_digest = 'rewrite'
         WHERE session_id = 'append-session' AND generation = 1 AND batch_ordinal = 0",
        "DELETE FROM session_temporal_projection_receipts
         WHERE session_id = 'append-session' AND generation = 1 AND batch_ordinal = 0",
    ] {
        assert!(
            conn.execute(sql, ()).await.is_err(),
            "append-only authority mutation must be rejected: {sql}"
        );
    }
}

#[tokio::test]
async fn temporal_schema_rejects_direct_cursor_retirement() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "INSERT INTO session_query_cursor_keys (
            key_id, key_version, key_material, created_at, retired_at
         ) VALUES ('cursor-key-only', 5, X'0102', 100, NULL)",
        (),
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_query_cursor_keys
             SET retired_at = 200
             WHERE key_id = 'cursor-key-only'",
            (),
        )
        .await
        .is_err(),
        "the sole active cursor key cannot be retired directly"
    );
}

#[tokio::test]
async fn temporal_schema_rotates_cursor_keys_atomically_and_survives_restart() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "INSERT INTO session_query_cursor_keys (
            key_id, key_version, key_material, created_at, retired_at
         ) VALUES ('cursor-key-1', 5, X'0102', 100, NULL)",
        (),
    )
    .await
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE session_query_cursor_keys SET key_material = X'0304'
             WHERE key_id = 'cursor-key-1'",
            (),
        )
        .await
        .is_err(),
        "key material must be immutable"
    );
    assert!(
        conn.execute(
            "INSERT INTO session_query_cursor_keys (
                key_id, key_version, key_material, created_at, retired_at
             ) VALUES ('cursor-version-regression', 4, X'03', 200, NULL)",
            (),
        )
        .await
        .is_err(),
        "cursor key versions must strictly increase"
    );
    assert!(
        conn.execute(
            "INSERT INTO session_query_cursor_keys (
                key_id, key_version, key_material, created_at, retired_at
             ) VALUES ('cursor-time-regression', 6, X'03', 100, NULL)",
            (),
        )
        .await
        .is_err(),
        "cursor key creation time must strictly increase"
    );
    conn.execute(
        "INSERT INTO session_query_cursor_keys (
            key_id, key_version, key_material, created_at, retired_at
         ) VALUES ('cursor-key-2', 6, X'0304', 200, NULL)",
        (),
    )
    .await
    .expect("one insert must atomically activate the new key and retire the prior key");
    assert!(
        conn.execute(
            "UPDATE session_query_cursor_keys SET retired_at = 300
             WHERE key_id = 'cursor-key-2'",
            (),
        )
        .await
        .is_err(),
        "the newly active key cannot be retired without a newer replacement"
    );
    assert!(
        conn.execute(
            "UPDATE session_query_cursor_keys SET retired_at = 201
             WHERE key_id = 'cursor-key-1'",
            (),
        )
        .await
        .is_err(),
        "retirement is one-way and cannot be rewritten"
    );
    assert!(
        conn.execute(
            "DELETE FROM session_query_cursor_keys WHERE key_id = 'cursor-key-1'",
            (),
        )
        .await
        .is_err(),
        "cursor key history is durable"
    );

    let mut active = conn
        .query(
            "SELECT COUNT(*) FROM session_query_cursor_keys WHERE retired_at IS NULL",
            (),
        )
        .await
        .unwrap();
    let active_count: i64 = active.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(active_count, 1);
    let mut key_rows = conn
        .query(
            "SELECT key_id, key_version, created_at, retired_at
             FROM session_query_cursor_keys
             ORDER BY key_version",
            (),
        )
        .await
        .unwrap();
    let retired = key_rows.next().await.unwrap().unwrap();
    assert_eq!(retired.get::<String>(0).unwrap(), "cursor-key-1");
    assert_eq!(retired.get::<i64>(3).unwrap(), 200);
    let active = key_rows.next().await.unwrap().unwrap();
    assert_eq!(active.get::<String>(0).unwrap(), "cursor-key-2");
    assert_eq!(active.get::<i64>(1).unwrap(), 6);
    assert!(active.get::<Option<i64>>(3).unwrap().is_none());
    drop(conn);
    drop(raw_db);

    let restart_path = tmp.path().join(".tracedecay").join("cursor-restart.db");
    copy_database_for_temporal_restart(&db_path, &restart_path).await;
    let reopened = GlobalDb::try_open_at(&restart_path)
        .await
        .expect("rotated cursor key authority must pass restart validation")
        .expect("global database should reopen");
    drop(reopened);
    assert_eq!(
        row_count(&restart_path, "session_query_cursor_keys").await,
        2
    );
}

#[tokio::test]
async fn temporal_schema_cursor_audit_rejects_nonmax_active_key() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "INSERT INTO session_query_cursor_keys (
            key_id, key_version, key_material, created_at, retired_at
         ) VALUES ('audit-key-1', 1, X'01', 100, NULL)",
        (),
    )
    .await
    .unwrap();
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS session_query_cursor_keys_insert_guard_v1;
         DROP TRIGGER IF EXISTS session_query_cursor_keys_retire_update_v1;
         DROP TRIGGER IF EXISTS session_query_cursor_keys_rotate_insert_v1;
         UPDATE session_query_cursor_keys SET retired_at = 200 WHERE key_id = 'audit-key-1';
         INSERT INTO session_query_cursor_keys (
            key_id, key_version, key_material, created_at, retired_at
         ) VALUES ('audit-key-2', 2, X'02', 200, NULL);
         UPDATE session_query_cursor_keys SET retired_at = NULL WHERE key_id = 'audit-key-1';
         UPDATE session_query_cursor_keys SET retired_at = 300 WHERE key_id = 'audit-key-2';",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let restart_path = tmp.path().join(".tracedecay").join("cursor-audit.db");
    copy_database_for_temporal_restart(&db_path, &restart_path).await;
    assert!(
        GlobalDb::try_open_at(&restart_path).await.is_err(),
        "restart audit must reject an active key that is not the monotonic maximum"
    );
}

#[tokio::test]
async fn temporal_schema_cursor_audit_rejects_skipped_successor_chain() {
    let tmp = TempDir::new().unwrap();
    for (fixture, versions) in [
        ("contiguous", [1_i64, 2, 3]),
        ("version-gaps", [1_i64, 3, 7]),
    ] {
        let db_path = tmp
            .path()
            .join(fixture)
            .join(".tracedecay")
            .join("sessions.db");
        let db = GlobalDb::try_open_at(&db_path)
            .await
            .expect("temporal schema initialization should not error")
            .expect("global database should open");
        drop(db);

        let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
        let conn = raw_db.connect().unwrap();
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS session_query_cursor_keys_insert_guard_v1;
             DROP TRIGGER IF EXISTS session_query_cursor_keys_retire_update_v1;
             DROP TRIGGER IF EXISTS session_query_cursor_keys_rotate_insert_v1;",
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO session_query_cursor_keys (
                key_id, key_version, key_material, created_at, retired_at
             ) VALUES
                ('broken-v1', ?1, X'01', 100, 300),
                ('broken-v2', ?2, X'02', 200, 300),
                ('broken-v3', ?3, X'03', 300, NULL)",
            libsql::params![versions[0], versions[1], versions[2]],
        )
        .await
        .unwrap();
        drop(conn);
        drop(raw_db);

        let restart_path = tmp
            .path()
            .join(fixture)
            .join(".tracedecay")
            .join("restart.db");
        copy_database_for_temporal_restart(&db_path, &restart_path).await;
        assert!(
            GlobalDb::try_open_at(&restart_path).await.is_err(),
            "{fixture}: a later key must not satisfy a skipped immediate-successor retirement"
        );
    }
}

#[tokio::test]
async fn temporal_schema_cursor_audit_accepts_valid_successor_chains() {
    let tmp = TempDir::new().unwrap();
    for (fixture, versions) in [
        ("contiguous", [1_i64, 2, 3]),
        ("version-gaps", [1_i64, 3, 7]),
    ] {
        let db_path = tmp
            .path()
            .join(fixture)
            .join(".tracedecay")
            .join("sessions.db");
        let db = GlobalDb::try_open_at(&db_path)
            .await
            .expect("temporal schema initialization should not error")
            .expect("global database should open");
        drop(db);

        let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
        let conn = raw_db.connect().unwrap();
        for (ordinal, version) in versions.into_iter().enumerate() {
            let created_at = ((ordinal + 1) * 100) as i64;
            conn.execute(
                "INSERT INTO session_query_cursor_keys (
                    key_id, key_version, key_material, created_at, retired_at
                 ) VALUES (?1, ?2, X'01', ?3, NULL)",
                libsql::params![format!("{fixture}-key-{version}"), version, created_at],
            )
            .await
            .unwrap();
        }
        drop(conn);
        drop(raw_db);
        assert_valid_cursor_chain(&cursor_key_history(&db_path).await);

        let restart_path = tmp
            .path()
            .join(fixture)
            .join(".tracedecay")
            .join("valid-restart.db");
        copy_database_for_temporal_restart(&db_path, &restart_path).await;
        let reopened = GlobalDb::try_open_at(&restart_path)
            .await
            .expect("valid immediate-successor chain must pass restart audit")
            .expect("global database should reopen");
        drop(reopened);
        assert_valid_cursor_chain(&cursor_key_history(&restart_path).await);
    }
}

#[tokio::test]
async fn temporal_schema_concurrent_cursor_rotations_serialize_safely() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::try_open_at(&db_path)
        .await
        .expect("temporal schema initialization should not error")
        .expect("global database should open");
    drop(db);

    let seed_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let initial = seed_db.connect().unwrap();
    initial
        .execute(
            "INSERT INTO session_query_cursor_keys (
                key_id, key_version, key_material, created_at, retired_at
             ) VALUES ('concurrent-key-1', 1, X'01', 100, NULL)",
            (),
        )
        .await
        .unwrap();
    drop(initial);
    drop(seed_db);

    // Separate Database/Connection handles so the holder and competitors contend
    // at the SQLite file lock, not within a shared in-process writer queue.
    let holder_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let holder = holder_db.connect().unwrap();
    let lower_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let higher_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let lower_conn = lower_db.connect().unwrap();
    let higher_conn = higher_db.connect().unwrap();
    lower_conn
        .busy_timeout(Duration::from_millis(1))
        .expect("competitor busy_timeout");
    higher_conn
        .busy_timeout(Duration::from_millis(1))
        .expect("competitor busy_timeout");

    let (lock_held_tx, lock_held_rx) = oneshot::channel::<()>();
    let (contention_tx, contention_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let probe = Arc::new(ContentionProbe::new(contention_tx));

    let lower_sql = "INSERT INTO session_query_cursor_keys (
        key_id, key_version, key_material, created_at, retired_at
     ) VALUES ('concurrent-key-2', 2, X'02', 200, NULL)";
    let higher_sql = "INSERT INTO session_query_cursor_keys (
        key_id, key_version, key_material, created_at, retired_at
     ) VALUES ('concurrent-key-3', 3, X'03', 300, NULL)";

    let holder_fut = async {
        holder
            .execute("BEGIN IMMEDIATE", ())
            .await
            .expect("holder must acquire a write transaction");
        // Prove the reserved lock is live with a no-op write under the txn.
        holder
            .execute(
                "UPDATE session_temporal_schema_migrations
                 SET version = version
                 WHERE name = 'session-temporal'",
                (),
            )
            .await
            .expect("holder must keep the write lock with an in-txn mutation");
        let _ = lock_held_tx.send(());
        match timeout(Duration::from_secs(5), release_rx).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => panic!("release signal dropped before holder cleanup"),
            Err(_) => panic!("timed out waiting to release holder after contention"),
        }
        holder
            .execute("ROLLBACK", ())
            .await
            .expect("holder must release the write lock");
    };

    let competitors_fut = async {
        timeout(Duration::from_secs(2), lock_held_rx)
            .await
            .expect("timed out waiting for holder write lock")
            .expect("lock-held signal dropped");
        let lower_probe = Arc::clone(&probe);
        let higher_probe = Arc::clone(&probe);
        tokio::join!(
            execute_with_busy_retry(&lower_conn, lower_sql, Some(lower_probe.as_ref())),
            execute_with_busy_retry(&higher_conn, higher_sql, Some(higher_probe.as_ref()))
        )
    };

    let coordinator_fut = async {
        timeout(Duration::from_secs(3), contention_rx)
            .await
            .expect("must observe at least one BUSY/LOCKED retry under held write lock")
            .expect("contention signal dropped");
        assert!(
            probe.busy_retries() >= 1,
            "BUSY/LOCKED retry path must run at least once before release, got {}",
            probe.busy_retries()
        );
        release_tx
            .send(())
            .expect("holder must still be waiting for release");
    };

    let ((), (lower_result, higher_result), ()) = timeout(Duration::from_secs(10), async {
        tokio::join!(holder_fut, competitors_fut, coordinator_fut)
    })
    .await
    .expect("cursor-key contention test deadlocked or exceeded bound");

    assert!(
        higher_result.is_ok(),
        "highest monotonic rotation must commit after bounded serialization: {higher_result:?}"
    );
    if let Err(error) = lower_result {
        assert!(
            error.contains("strictly monotonic") || error.contains("UNIQUE"),
            "lower rotation may fail only after a higher rotation commits: {error}"
        );
    }
    assert!(
        probe.busy_retries() >= 1,
        "BUSY/LOCKED retry path must have run, got {}",
        probe.busy_retries()
    );

    drop(lower_conn);
    drop(higher_conn);
    drop(holder);
    drop(lower_db);
    drop(higher_db);
    drop(holder_db);

    let history = cursor_key_history(&db_path).await;
    assert_eq!(history.last().unwrap().0, 3);
    assert_valid_cursor_chain(&history);
    assert_eq!(
        history
            .iter()
            .filter(|(_, _, retired_at)| retired_at.is_none())
            .count(),
        1,
        "exactly one active cursor key maximum must remain"
    );

    let restart_path = tmp.path().join(".tracedecay").join("concurrent-restart.db");
    copy_database_for_temporal_restart(&db_path, &restart_path).await;
    let reopened = GlobalDb::try_open_at(&restart_path)
        .await
        .expect("serialized concurrent rotations must pass restart audit")
        .expect("global database should reopen");
    drop(reopened);
    assert_valid_cursor_chain(&cursor_key_history(&restart_path).await);
}

#[tokio::test]
async fn lcm_schema_migrates_legacy_sessions_db_in_place() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let db = GlobalDb::open_at(&db_path).await.expect("global db open");

    assert!(table_exists(&db_path, "session_schema_migrations").await);
    assert!(table_exists(&db_path, "lcm_raw_messages").await);
    assert!(table_exists(&db_path, "lcm_raw_messages_fts").await);
    assert_eq!(
        db.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );

    let legacy = db
        .lcm_load_raw_message("cursor", "legacy-message")
        .await
        .expect("legacy message should be carried into raw store");
    assert_eq!(legacy.provider, "cursor");
    assert_eq!(legacy.message_id, "legacy-message");
    assert_eq!(legacy.session_id, "legacy-session");
    assert_eq!(legacy.role, "assistant");
    assert_eq!(legacy.ordinal, 1);
    assert_eq!(legacy.content, "legacy text");
    assert_eq!(
        legacy.storage_kind,
        tracedecay::sessions::lcm::LcmStorageKind::Inline
    );
    assert!(legacy.legacy_source);
    assert!(!legacy.legacy_truncated);
    assert_eq!(
        fts_legacy_message_ids(&db_path).await,
        vec!["legacy-message".to_string()]
    );
}

#[tokio::test]
async fn lcm_schema_marks_legacy_truncated_messages() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let legacy_text = "legacy text\n[truncated by tracedecay]";
    create_legacy_sessions_db_with_text(&db_path, legacy_text).await;

    let db = GlobalDb::open_at(&db_path).await.expect("global db open");
    let legacy = db
        .lcm_load_raw_message("cursor", "legacy-message")
        .await
        .expect("legacy message should be carried into raw store");

    assert_eq!(legacy.content, legacy_text);
    assert!(legacy.legacy_source);
    assert!(legacy.legacy_truncated);
}

#[tokio::test]
async fn lcm_schema_migration_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let db = GlobalDb::open_at(&db_path).await.expect("global db open");
    assert_eq!(
        db.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    drop(db);

    let reopened = GlobalDb::open_at(&db_path).await.expect("global db reopen");
    assert_eq!(
        reopened.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    assert_eq!(
        schema_version(&db_path).await,
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    assert_eq!(row_count(&db_path, "lcm_raw_messages").await, 1);
    assert_eq!(
        fts_legacy_message_ids(&db_path).await,
        vec!["legacy-message".to_string()]
    );
}

#[tokio::test]
async fn lcm_schema_v6_migrates_bounded_codex_pending_queue_indexes() {
    const SESSION_QUERY: &str = "
        SELECT candidate.node_id, candidate.session_id
        FROM lcm_summary_nodes AS candidate
        JOIN session_summary_nodes AS authority
          ON authority.summary_id = candidate.node_id
         AND authority.session_id = candidate.session_id
        WHERE candidate.provider = 'codex'
          AND CASE
                WHEN json_valid(candidate.metadata_json) THEN
                  json_extract(candidate.metadata_json, '$.source') =
                    'codex_context_compacted'
                  AND COALESCE(
                        json_extract(
                          candidate.metadata_json,
                          '$.tracedecay_summary_source'
                        ),
                        ''
                      ) <> 'codex_app_server'
                ELSE 0
              END = 1
          AND NOT EXISTS (
                SELECT 1
                FROM session_summary_successors AS lineage
                WHERE lineage.predecessor_summary_id = candidate.node_id
              )
          AND EXISTS (
                SELECT 1
                FROM lcm_summary_sources AS source
                JOIN lcm_raw_messages AS raw
                  ON source.source_kind = 'raw_message'
                 AND CAST(source.source_id AS INTEGER) = raw.store_id
                 AND raw.provider = candidate.provider
                 AND raw.session_id = candidate.session_id
                WHERE source.node_id = candidate.node_id
              )
          AND candidate.session_id = 'session-one'
        ORDER BY candidate.depth DESC, candidate.created_at DESC, candidate.node_id
        LIMIT 10";
    const ROOT_QUERY: &str = "
        SELECT candidate.node_id, candidate.session_id
        FROM lcm_summary_nodes AS candidate
        JOIN session_summary_nodes AS authority
          ON authority.summary_id = candidate.node_id
         AND authority.session_id = candidate.session_id
        WHERE candidate.provider = 'codex'
          AND CASE
                WHEN json_valid(candidate.metadata_json) THEN
                  json_extract(candidate.metadata_json, '$.source') =
                    'codex_context_compacted'
                  AND COALESCE(
                        json_extract(
                          candidate.metadata_json,
                          '$.tracedecay_summary_source'
                        ),
                        ''
                      ) <> 'codex_app_server'
                ELSE 0
              END = 1
          AND NOT EXISTS (
                SELECT 1
                FROM session_summary_successors AS lineage
                WHERE lineage.predecessor_summary_id = candidate.node_id
              )
          AND EXISTS (
                SELECT 1
                FROM lcm_summary_sources AS source
                JOIN lcm_raw_messages AS raw
                  ON source.source_kind = 'raw_message'
                 AND CAST(source.source_id AS INTEGER) = raw.store_id
                 AND raw.provider = candidate.provider
                 AND raw.session_id = candidate.session_id
                WHERE source.node_id = candidate.node_id
              )
        ORDER BY candidate.created_at DESC, candidate.depth DESC, candidate.node_id
        LIMIT 10";

    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let db = GlobalDb::open_at(&db_path).await.expect("global db open");
    drop(db);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_lcm_summary_nodes_codex_pending_session_order;
         DROP INDEX IF EXISTS idx_lcm_summary_nodes_codex_pending_root_order;
         CREATE INDEX idx_lcm_summary_nodes_codex_pending_session_order
             ON lcm_summary_nodes(session_id, depth DESC, created_at DESC, node_id)
             WHERE provider = 'codex'
               AND CASE
                     WHEN json_valid(metadata_json) THEN
                       json_extract(metadata_json, '$.source') = 'codex_context_compacted'
                       AND COALESCE(
                             json_extract(metadata_json, '$.tracedecay_summary_source'),
                             ''
                           ) <> 'codex_app_server'
                     ELSE 0
                   END;
         CREATE INDEX idx_lcm_summary_nodes_codex_pending_root_order
             ON lcm_summary_nodes(created_at DESC, depth DESC, node_id, session_id)
             WHERE provider = 'codex'
               AND CASE
                     WHEN json_valid(metadata_json) THEN
                       json_extract(metadata_json, '$.source') = 'codex_context_compacted'
                       AND COALESCE(
                             json_extract(metadata_json, '$.tracedecay_summary_source'),
                             ''
                           ) <> 'codex_app_server'
                     ELSE 0
                   END;
         UPDATE session_schema_migrations SET version = 6 WHERE name = 'lcm';",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    assert!(tracedecay::sessions::lcm::LCM_SCHEMA_VERSION > 6);
    let migrated = GlobalDb::open_at(&db_path)
        .await
        .expect("v6 database should migrate");
    assert_eq!(
        migrated.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    drop(migrated);

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    assert_eq!(
        index_key_columns(&conn, "idx_lcm_summary_nodes_codex_pending_session_order").await,
        vec![
            ("session_id".to_string(), 0),
            ("<expression>".to_string(), 0),
            ("depth".to_string(), 1),
            ("created_at".to_string(), 1),
            ("node_id".to_string(), 0),
        ]
    );
    assert_eq!(
        index_key_columns(&conn, "idx_lcm_summary_nodes_codex_pending_root_order").await,
        vec![
            ("<expression>".to_string(), 0),
            ("created_at".to_string(), 1),
            ("depth".to_string(), 1),
            ("node_id".to_string(), 0),
            ("session_id".to_string(), 0),
        ]
    );

    for (query, expected_index) in [
        (
            SESSION_QUERY,
            "idx_lcm_summary_nodes_codex_pending_session_order",
        ),
        (ROOT_QUERY, "idx_lcm_summary_nodes_codex_pending_root_order"),
    ] {
        let details = explain_query_plan(&conn, query).await;
        assert!(
            details.iter().any(|detail| detail.contains(expected_index)),
            "EXPLAIN did not use {expected_index}: {details:?}"
        );
        assert!(
            details
                .iter()
                .all(|detail| !detail.contains("USE TEMP B-TREE FOR ORDER BY")),
            "pending query must not sort through a temporary B-tree: {details:?}"
        );
        assert!(
            details.iter().any(|detail| {
                detail.contains("sqlite_autoindex_session_summary_successors_1")
                    && detail.contains("predecessor_summary_id=?")
            }),
            "leaf anti-join must use the successor primary key: {details:?}"
        );
    }
}

// Schema v3 narrows the raw-message FTS index to index_text only, matching
// hermes-lcm `build_message_fts_spec` (store.py:173-204) which indexes the
// content column alone. Migrating a v2 database must restructure the FTS
// objects, carry the searchable rows forward, and stop role/metadata text
// from satisfying unqualified MATCH queries.
#[tokio::test]
async fn lcm_schema_v3_migration_restructures_raw_fts_and_preserves_search() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    // Establish the schema, then rewrite the FTS objects into the pre-v3
    // shape with the version marker set back to 2.
    let db = GlobalDb::open_at(&db_path).await.expect("global db open");
    drop(db);
    downgrade_raw_fts_to_v2(&db_path).await;
    assert_eq!(schema_version(&db_path).await, 2);
    assert_eq!(
        fts_message_ids_matching(&db_path, "assistant").await,
        vec!["legacy-message".to_string()],
        "v2 fixture must over-match via the indexed role column"
    );

    let migrated = GlobalDb::open_at(&db_path).await.expect("global db reopen");
    assert_eq!(
        migrated.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    drop(migrated);

    // The restructured objects no longer index role/metadata_json.
    let sqls = raw_fts_object_sql(&db_path).await;
    assert_eq!(sqls.len(), 4, "FTS table and three triggers must exist");
    for sql in &sqls {
        assert!(
            !sql.contains("metadata_json"),
            "migrated FTS object still references metadata_json: {sql}"
        );
    }

    // Search results carried forward; role text no longer matches.
    assert_eq!(
        fts_message_ids_matching(&db_path, "legacy").await,
        vec!["legacy-message".to_string()],
        "content search results must survive the migration"
    );
    assert!(
        fts_message_ids_matching(&db_path, "assistant")
            .await
            .is_empty(),
        "role text must not match after the v3 restructure"
    );

    // Idempotent re-open: structure and results are stable.
    let reopened = GlobalDb::open_at(&db_path)
        .await
        .expect("idempotent reopen");
    assert_eq!(
        reopened.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    drop(reopened);
    assert_eq!(
        fts_message_ids_matching(&db_path, "legacy").await,
        vec!["legacy-message".to_string()]
    );
    assert!(
        fts_message_ids_matching(&db_path, "assistant")
            .await
            .is_empty()
    );
}

// Mirrors hermes-lcm `run_versioned_migrations` (db_bootstrap.py:580-601):
// version steps are monotonic and `set_schema_version(conn, current_version)`
// never lowers a marker written by a newer release. Opening a database whose
// LCM schema version is newer than this binary must not downgrade the marker
// or re-run the legacy carry-forward against data the newer schema owns.
#[tokio::test]
async fn lcm_schema_future_version_is_preserved_without_remigration() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let db = GlobalDb::open_at(&db_path).await.expect("global db open");
    assert_eq!(
        db.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    drop(db);

    // Simulate a database last touched by a newer tracedecay: bump the version
    // marker past this binary and have the newer schema relocate carried rows
    // out of lcm_raw_messages.
    let future_version = tracedecay::sessions::lcm::LCM_SCHEMA_VERSION + 97;
    set_migration_version(&db_path, future_version).await;
    set_migration_applied_at(&db_path, 456).await;
    {
        let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
        let conn = raw_db.connect().unwrap();
        conn.execute("DELETE FROM lcm_raw_messages", ())
            .await
            .unwrap();
    }
    assert_eq!(row_count(&db_path, "lcm_raw_messages").await, 0);

    let reopened = GlobalDb::open_at(&db_path).await.expect("global db reopen");
    assert_eq!(
        reopened.lcm_schema_version().await.unwrap(),
        future_version,
        "future schema version marker must not be downgraded"
    );
    drop(reopened);
    assert_eq!(schema_version(&db_path).await, future_version);
    assert_eq!(migration_applied_at(&db_path).await, 456);
    assert_eq!(
        row_count(&db_path, "lcm_raw_messages").await,
        0,
        "legacy carry-forward must not re-run against a newer schema's data"
    );
}

#[tokio::test]
async fn lcm_schema_current_version_reopen_skips_migration_update() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let db = GlobalDb::open_at(&db_path).await.expect("global db open");
    assert_eq!(
        db.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    drop(db);

    set_migration_applied_at(&db_path, 123).await;
    assert_eq!(migration_applied_at(&db_path).await, 123);

    let reopened = GlobalDb::open_at(&db_path).await.expect("global db reopen");
    assert_eq!(
        reopened.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    assert_eq!(migration_applied_at(&db_path).await, 123);
}
