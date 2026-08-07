//! Schema-shape introspection probes for the memory_v2 tests.

use crate::db::engine::params;
use crate::errors::Result;

use super::super::{MemoryV2Executor, db_error, row_exists};

pub(in crate::db::memory_v2) async fn table_has_column(
    conn: &impl MemoryV2Executor,
    table: &str,
    column: &str,
    operation: &str,
) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM pragma_table_xinfo(?1) WHERE name = ?2 COLLATE NOCASE",
            params![table, column],
        )
        .await
        .map_err(|error| db_error(operation, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| db_error(operation, error))
}

pub(in crate::db::memory_v2) async fn table_exists(
    conn: &impl MemoryV2Executor,
    table: &str,
) -> Result<bool> {
    row_exists(
        conn,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
    )
    .await
}
