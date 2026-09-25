//! Measurement helpers shared by this crate's unit tests.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_runtime_core::db::engine::{Executor, Value, params};

fn sqlite_value(value: &Value) -> rusqlite::types::Value {
    match value {
        Value::Null => rusqlite::types::Value::Null,
        Value::Integer(value) => rusqlite::types::Value::Integer(*value),
        Value::Real(value) => rusqlite::types::Value::Real(*value),
        Value::Text(value) => rusqlite::types::Value::Text(value.clone()),
        Value::Blob(value) => rusqlite::types::Value::Blob(value.clone()),
    }
}

/// Counts actual SQLite virtual-machine steps for the exact SQL, rather than
/// the rows materialized by the engine test adapter.
pub(crate) fn sqlite_vm_steps(database_path: &Path, sql: &str, values: &[Value]) -> usize {
    let connection = rusqlite::Connection::open(database_path)
        .expect("open native SQLite connection for VM-step measurement");
    let steps = Arc::new(AtomicUsize::new(0));
    let counted_steps = Arc::clone(&steps);
    connection
        .progress_handler(
            1,
            Some(move || {
                counted_steps.fetch_add(1, Ordering::Relaxed);
                false
            }),
        )
        .expect("install SQLite VM progress handler");
    {
        let mut statement = connection
            .prepare(sql)
            .expect("prepare VM-step measurement statement");
        let native_values = values.iter().map(sqlite_value).collect::<Vec<_>>();
        let mut rows = statement
            .query(rusqlite::params_from_iter(native_values))
            .expect("execute VM-step measurement statement");
        while rows
            .next()
            .expect("advance VM-step measurement statement")
            .is_some()
        {}
    }
    connection
        .progress_handler(1, None::<fn() -> bool>)
        .expect("clear SQLite VM progress handler");
    steps.load(Ordering::Relaxed)
}

/// The session-temporal tables LCM summary reads join against. They are owned
/// by `tracedecay-session-temporal-store`, which this crate cannot depend on,
/// so unit fixtures declare the columns the summary, lineage, and availability
/// queries touch and seed one active generation per session. The canonical
/// publication-only columns (`summary_anchor_id`, `source_horizon_json`,
/// `publication_json`) default here because no LCM read consults them.
pub(crate) const SESSION_GENERATION_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS session_temporal_generations (
    session_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    state TEXT NOT NULL
 );
 CREATE TABLE IF NOT EXISTS session_summary_availability (
    session_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    summary_id TEXT NOT NULL,
    availability TEXT NOT NULL
 );
 CREATE TABLE IF NOT EXISTS session_summary_nodes (
    summary_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    depth INTEGER NOT NULL,
    summary_anchor_id TEXT NOT NULL DEFAULT '',
    summary_text TEXT NOT NULL,
    summary_hash TEXT NOT NULL,
    summary_token_count INTEGER NOT NULL,
    source_token_count INTEGER NOT NULL,
    source_time_start INTEGER,
    source_time_end INTEGER,
    expand_hint TEXT,
    metadata_json TEXT,
    source_horizon_json TEXT NOT NULL DEFAULT '{}',
    publication_json TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
 );
 CREATE INDEX IF NOT EXISTS idx_session_summary_nodes_depth_tokens
    ON session_summary_nodes(
        provider, session_id, depth, summary_token_count, source_token_count
    );
 CREATE TABLE IF NOT EXISTS session_summary_sources (
    summary_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    source_kind TEXT NOT NULL,
    source_id TEXT NOT NULL,
    PRIMARY KEY(summary_id, ordinal)
 );
 CREATE INDEX IF NOT EXISTS idx_session_summary_sources_source
    ON session_summary_sources(source_kind, source_id, summary_id);
 CREATE VIRTUAL TABLE IF NOT EXISTS session_summary_nodes_fts USING fts5(
    summary_text, content='session_summary_nodes', content_rowid='rowid'
 );
 CREATE TRIGGER IF NOT EXISTS session_summary_nodes_fts_insert_v1
    AFTER INSERT ON session_summary_nodes BEGIN
        INSERT INTO session_summary_nodes_fts(rowid, summary_text)
        VALUES (NEW.rowid, NEW.summary_text);
    END;";

/// Generation every fixture session is active in.
pub(crate) const FIXTURE_GENERATION: i64 = 1;

pub(crate) async fn seed_active_generation(conn: &(impl Executor + ?Sized), session_id: &str) {
    conn.execute(
        "INSERT INTO session_temporal_generations(session_id, generation, state)
         VALUES (?1, ?2, 'active')",
        params![session_id, FIXTURE_GENERATION],
    )
    .await
    .expect("active generation");
}

/// Publish `node_id` as available in the fixture generation; summary reads
/// join on this row, so a seeded node without it is invisible by design.
pub(crate) async fn mark_summary_available(
    conn: &(impl Executor + ?Sized),
    session_id: &str,
    node_id: &str,
) {
    conn.execute(
        "INSERT INTO session_summary_availability(session_id, generation, summary_id, availability)
         VALUES (?1, ?2, ?3, 'available')",
        params![session_id, FIXTURE_GENERATION, node_id],
    )
    .await
    .expect("summary availability");
}
