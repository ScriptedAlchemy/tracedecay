//! Schema-shape introspection probes.

use crate::db::engine::params;
use crate::errors::Result;

use super::super::{MemoryV2Executor, db_error, optional_string, row_exists};

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

async fn trigger_exists(conn: &impl MemoryV2Executor, trigger: &str) -> Result<bool> {
    row_exists(
        conn,
        "SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
        params![trigger],
    )
    .await
}

/// V20/V21 retain their original feedback-backfill behavior. The V22 map and
/// history must be installed together before a backfill can write either.
async fn proposal_current_is_v22(conn: &impl MemoryV2Executor) -> Result<bool> {
    let Some(sql) = optional_string(
        conn,
        "SELECT sql FROM sqlite_master
         WHERE type = 'table' AND name = 'memory_v2_proposal_current'",
        (),
    )
    .await?
    else {
        return Ok(false);
    };
    let sql = sql.to_ascii_lowercase();
    Ok(sql.contains("'quarantined'")
        && !sql.contains("'applying'")
        && sql.contains("revision >= 1"))
}

pub(in crate::db::memory_v2) async fn proposal_schema_is_v22(
    conn: &impl MemoryV2Executor,
) -> Result<bool> {
    if !proposal_current_is_v22(conn).await? {
        return Ok(false);
    }
    let Some(transitions_sql) = optional_string(
        conn,
        "SELECT sql FROM sqlite_master
         WHERE type = 'table' AND name = 'memory_v2_proposal_transitions'",
        (),
    )
    .await?
    else {
        return Ok(false);
    };
    Ok(transitions_sql
        .to_ascii_lowercase()
        .contains("'quarantined'")
        && trigger_exists(conn, "memory_v2_proposal_transitions_no_new_applying").await?)
}
