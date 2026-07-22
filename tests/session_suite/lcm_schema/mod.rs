#![allow(clippy::collapsible_if)] // test scaffolding
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

mod lcm_migration;
mod temporal_catalog;
mod temporal_constraints;
mod temporal_cursor;
