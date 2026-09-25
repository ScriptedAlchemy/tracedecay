#![allow(clippy::collapsible_if)] // test scaffolding
use std::path::Path;
use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::time::timeout;

use tracedecay_runtime_core::db::engine::{
    Connection, TestConnection, TransactionBehavior, params,
};
use tracedecay_runtime_core::db::{
    Database, DatabaseAuthority, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
};

use crate::tests::harness::open_registered_test_database_fixture;

async fn open_global_db(db_path: &Path) -> tracedecay_domain::errors::Result<TestConnection> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    drop(
        open_registered_test_database_fixture(db_path, TestDatabaseRuntimeScope::ProfileSessions)
            .await?,
    );
    Ok(TestConnection::open(db_path))
}

async fn open_read_only_global_db(
    db_path: &Path,
) -> tracedecay_domain::errors::Result<Option<(DatabaseAuthority, Database)>> {
    if !db_path.try_exists()? {
        return Ok(None);
    }
    // Publishing a runtime materialises a profile-scoped sidecar shard, which
    // the kernel initialises through its fail-closed registered-schema port.
    // Idempotent, the port keeps the first registration.
    crate::register_registered_schema_installer();
    let authority = DatabaseAuthority::acquire_test(db_path, "LCM schema read-only fixture")?;
    let (database, _) = Database::publish_registered_test_runtime(
        db_path,
        &authority,
        TestDatabaseRuntimeMode::ReadOnly,
        TestDatabaseRuntimeScope::ProfileSessions,
    )
    .await?;
    Ok(Some((authority, database)))
}

async fn create_legacy_sessions_db(db_path: &Path) {
    create_legacy_sessions_db_with_text(db_path, "legacy text").await;
}

async fn create_legacy_sessions_db_with_text(db_path: &Path, legacy_text: &str) {
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let old_db = TestConnection::open(db_path);
    let conn = (*old_db).clone();
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
        params![legacy_text],
    )
    .await
    .unwrap();
    drop(conn);
    drop(old_db);
}

async fn table_exists(db_path: &Path, table: &str) -> bool {
    let db = TestConnection::open(db_path);
    let conn = (*db).clone();
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE name = ?1 AND type IN ('table', 'view')",
            params![table],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().is_some()
}

async fn row_count(db_path: &Path, table: &str) -> i64 {
    let db = TestConnection::open(db_path);
    let conn = (*db).clone();
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let mut rows = conn.query(&sql, ()).await.unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get(0).unwrap()
}

async fn cursor_key_history(db_path: &Path) -> Vec<(i64, i64, Option<i64>)> {
    let db = TestConnection::open(db_path);
    let conn = (*db).clone();
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

async fn schema_version(db_path: &Path) -> i64 {
    let db = TestConnection::open(db_path);
    let conn = (*db).clone();
    schema_version_on(&conn).await
}

async fn schema_version_on(conn: &Connection) -> i64 {
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
    let db = TestConnection::open(db_path);
    let conn = (*db).clone();
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
    let db = TestConnection::open(db_path);
    let conn = (*db).clone();
    conn.execute(
        "UPDATE session_schema_migrations
         SET applied_at = ?1
         WHERE name = 'lcm'",
        params![applied_at],
    )
    .await
    .unwrap();
}

async fn set_migration_version(db_path: &Path, version: i64) {
    let db = TestConnection::open(db_path);
    let conn = (*db).clone();
    conn.execute(
        "UPDATE session_schema_migrations
         SET version = ?1
         WHERE name = 'lcm'",
        params![version],
    )
    .await
    .unwrap();
}

async fn normalized_trigger_sql(db_path: &Path, trigger: &str) -> String {
    let db = TestConnection::open(db_path);
    let conn = (*db).clone();
    let mut rows = conn
        .query(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
            params![trigger],
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

async fn temporal_schema_object_catalog(db_path: &Path) -> Vec<(String, String)> {
    let db = TestConnection::open(db_path);
    let conn = (*db).clone();
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
            || table_name.starts_with("session_relation")
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

async fn explain_query_plan(conn: &Connection, sql: &str) -> Vec<String> {
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

async fn index_key_columns(conn: &Connection, index: &str) -> Vec<(String, i64)> {
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

async fn table_index_names(conn: &Connection, table: &str) -> Vec<String> {
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
    let db = TestConnection::open(db_path);
    let conn = (*db).clone();
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
    let source_database = TestConnection::open(source);
    source_database.checkpoint_wal_truncate().await.unwrap();
    drop(source_database);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::copy(source, target).unwrap();
}

mod lcm_schema_contract;
mod temporal_catalog;
mod temporal_constraints;
mod temporal_cursor;
