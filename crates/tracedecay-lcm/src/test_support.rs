//! Measurement helpers shared by this crate's unit tests.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_runtime_core::db::engine::{TestConnection, Value, params};

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
/// so unit fixtures declare the columns the lineage and availability queries
/// touch and seed one active generation per session.
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
 );";

/// Generation every fixture session is active in.
pub(crate) const FIXTURE_GENERATION: i64 = 1;

pub(crate) async fn seed_active_generation(conn: &TestConnection, session_id: &str) {
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
pub(crate) async fn mark_summary_available(conn: &TestConnection, session_id: &str, node_id: &str) {
    conn.execute(
        "INSERT INTO session_summary_availability(session_id, generation, summary_id, availability)
         VALUES (?1, ?2, ?3, 'available')",
        params![session_id, FIXTURE_GENERATION, node_id],
    )
    .await
    .expect("summary availability");
}
