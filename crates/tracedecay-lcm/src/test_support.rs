//! Measurement helpers shared by this crate's unit tests.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_runtime_core::db::engine::Value;

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
