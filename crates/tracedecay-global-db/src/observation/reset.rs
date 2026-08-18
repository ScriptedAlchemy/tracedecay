//! Scoped operator recovery for a refused observation authority.
//!
//! Admission refuses a sessions store whose `observations` or
//! `source_cursor_advances` table carries a pre-release branch-local shape
//! (typed `ResetRequired` naming [`OBSERVATION_AUTHORITY`]). Because the
//! refusal fires before any runtime can mount the store, recovery runs
//! offline over a plain connection while the operator holds the profile's
//! exclusive maintenance lease.
//!
//! The reset is scoped to exactly the refused authority: it drops the
//! observation-authority tables plus their pure projection derivations,
//! recreates every one of them empty at the canonical shape (through the same
//! DDL, index, and trigger authorities the schema installer uses — attach
//! only validates existing stores, it never reinstalls), clears the
//! `session_messages` projector output (classified `Recoverable`; the cleared
//! evidence re-derives by re-ingesting provider transcripts), and preserves
//! everything else in the store — transcripts, LCM content, configuration,
//! registry, workflow, and session-temporal state. It fails closed, without
//! touching anything, when session-temporal rows reference observation rows:
//! those tables default to `Durable` in the durability model, so a scoped
//! reset must not delete them and refuses instead.

use std::collections::BTreeSet;

use tracedecay_runtime_core::errors::TraceDecayError;

use super::schema::{
    OBSERVATION_AUTHORITY, OBSERVATION_AUTHORITY_SCHEMA_SQL, OBSERVATION_CANONICAL_COLUMNS,
    OBSERVATION_SCHEMA_MIGRATION, SOURCE_CURSOR_ADVANCES_CANONICAL_COLUMNS,
};
use crate::observation_projection::{
    OBSERVATION_PROJECTION_BINDING_TRIGGERS_SQL, OBSERVATION_PROJECTION_PERFORMANCE_INDEX_SQL,
    OBSERVATION_PROJECTION_SCHEMA_SQL,
};
use crate::schema_contract::invariant_trigger_sql_for_tables;

const OPERATION: &str = "reset refused observation authority";

/// Tables owned by `ensure_observation_schema`; the next admission recreates
/// every one of them empty at the canonical shape.
const OBSERVATION_AUTHORITY_TABLES: &[&str] = &[
    "observations",
    "sanitization_receipts",
    "source_cursors",
    "source_cursor_advances",
    "projection_queue",
    "remote_writer_fences",
    "remote_observation_events",
    "observation_retrieval_anchors",
    "observation_repository_provenance",
];

/// Pure derivations of the observation stream owned by
/// `ensure_observation_projection_schema`. They are outputs of the projection
/// and rebuild machinery over `observations`, so they reset with it; the next
/// admission recreates them empty.
const OBSERVATION_PROJECTION_TABLES: &[&str] = &[
    "observation_projection_provenance",
    "observation_projection_checkpoints",
    "observation_projection_aliases",
    "observation_projection_dispositions",
    "observation_workflow_facts",
    "observation_provider_usage",
    "observation_projection_rebuilds",
    "observation_projection_rebuild_provider_usage",
    "observation_projection_rebuild_aliases",
    "observation_projection_rebuild_sessions",
    "observation_projection_rebuild_messages",
    "observation_projection_rebuild_provenance",
    "observation_projection_rebuild_dispositions",
    "observation_projection_rebuild_workflow_facts",
];

/// Session-temporal tables holding foreign keys into `observations`. The
/// durability model classifies them `Durable` by default, so a scoped
/// observation reset must not delete their rows; populated rows here make the
/// scoped reset refuse rather than orphan them.
const DURABLE_DEPENDENT_TABLES: &[&str] = &[
    "session_temporal_observation_effects",
    "session_occurrences",
];

/// Outcome of one completed scoped reset.
#[derive(Debug)]
pub struct ObservationAuthorityResetV1 {
    /// Tables dropped and recreated empty at the canonical shape.
    pub reset_tables: Vec<String>,
    /// `session_messages` projector-output rows cleared (`Recoverable`; the
    /// external-content FTS index is synchronized by its delete trigger).
    pub cleared_session_message_rows: u64,
}

fn reset_storage(error: rusqlite::Error) -> TraceDecayError {
    TraceDecayError::Database {
        operation: OPERATION.to_string(),
        message: error.to_string(),
    }
}

fn table_exists(conn: &rusqlite::Connection, table: &str) -> Result<bool, TraceDecayError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get::<_, bool>(0),
    )
    .map_err(reset_storage)
}

fn table_columns(
    conn: &rusqlite::Connection,
    table: &str,
) -> Result<BTreeSet<String>, TraceDecayError> {
    let mut statement = conn
        .prepare("SELECT name FROM pragma_table_xinfo(?1)")
        .map_err(reset_storage)?;
    let columns = statement
        .query_map([table], |row| row.get::<_, String>(0))
        .map_err(reset_storage)?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(reset_storage)?;
    Ok(columns)
}

fn row_count(conn: &rusqlite::Connection, table: &str) -> Result<u64, TraceDecayError> {
    let count = conn
        .query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(reset_storage)?;
    u64::try_from(count).map_err(|_| TraceDecayError::Database {
        operation: OPERATION.to_string(),
        message: format!("{table} row count was negative"),
    })
}

fn canonical(columns: &[&str]) -> BTreeSet<String> {
    columns.iter().map(|column| (*column).to_string()).collect()
}

/// Whether the store currently carries a shape the observation authority
/// refuses at admission. Mirrors the refusal predicates in `super::schema`
/// through the shared canonical column sets.
fn observation_authority_refused(conn: &rusqlite::Connection) -> Result<bool, TraceDecayError> {
    if table_exists(conn, "observations")? {
        let marker_recorded = table_exists(conn, "global_schema_migrations")?
            && conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM global_schema_migrations WHERE migration = ?1)",
                    [OBSERVATION_SCHEMA_MIGRATION],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(reset_storage)?;
        if !marker_recorded
            || table_columns(conn, "observations")? != canonical(OBSERVATION_CANONICAL_COLUMNS)
        {
            return Ok(true);
        }
    }
    if table_exists(conn, "source_cursor_advances")?
        && table_columns(conn, "source_cursor_advances")?
            != canonical(SOURCE_CURSOR_ADVANCES_CANONICAL_COLUMNS)
    {
        return Ok(true);
    }
    Ok(false)
}

/// Resets exactly the refused observation authority in one transaction.
///
/// Fails closed, mutating nothing, when the authority is not actually in a
/// refused shape (protecting healthy data from an accidental reset) or when
/// durable session-temporal rows still reference observation rows (a scoped
/// reset must not orphan or delete them).
pub fn reset_refused_observation_authority(
    conn: &mut rusqlite::Connection,
) -> Result<ObservationAuthorityResetV1, TraceDecayError> {
    // The reset drops the refused tables together with every table that
    // references them, so per-statement foreign-key enforcement would only
    // reject the intermediate drop states of an exclusive maintenance
    // connection. Referential coherence is restored by the next admission
    // recreating the authority empty.
    conn.pragma_update(None, "foreign_keys", false)
        .map_err(reset_storage)?;
    let transaction = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(reset_storage)?;
    if !observation_authority_refused(&transaction)? {
        return Err(TraceDecayError::Config {
            message: format!(
                "the {OBSERVATION_AUTHORITY} authority in this store is not in a refused state; \
                 nothing was reset"
            ),
        });
    }
    for table in DURABLE_DEPENDENT_TABLES {
        if !table_exists(&transaction, table)? {
            continue;
        }
        let rows = row_count(&transaction, table)?;
        if rows > 0 {
            return Err(TraceDecayError::Config {
                message: format!(
                    "a scoped {OBSERVATION_AUTHORITY} reset would orphan {rows} durable row(s) \
                     in {table}; this store needs a session-temporal remediation first, so \
                     nothing was reset"
                ),
            });
        }
    }

    // Clear the recoverable projector output before dropping the projection
    // tables: the audit-invalidation trigger on `session_messages` reads
    // `observation_projection_provenance` and must still resolve while the
    // deletes run.
    let cleared_session_message_rows = if table_exists(&transaction, "session_messages")? {
        u64::try_from(
            transaction
                .execute("DELETE FROM session_messages", [])
                .map_err(reset_storage)?,
        )
        .map_err(|_| TraceDecayError::Database {
            operation: OPERATION.to_string(),
            message: "session_messages delete count overflowed".to_string(),
        })?
    } else {
        0
    };
    let mut reset_tables = Vec::new();
    for table in OBSERVATION_AUTHORITY_TABLES
        .iter()
        .chain(OBSERVATION_PROJECTION_TABLES)
    {
        if table_exists(&transaction, table)? {
            transaction
                .execute_batch(&format!("DROP TABLE \"{table}\""))
                .map_err(reset_storage)?;
        }
        reset_tables.push((*table).to_string());
    }
    // Recreate the authority empty at the canonical shape through the same
    // DDL, index, and invariant-trigger authorities the schema installer
    // uses. Attach only validates an existing store — it never reinstalls —
    // so the reset itself must leave the store at the final contract.
    transaction
        .execute_batch(OBSERVATION_AUTHORITY_SCHEMA_SQL)
        .map_err(reset_storage)?;
    transaction
        .execute_batch(OBSERVATION_PROJECTION_SCHEMA_SQL)
        .map_err(reset_storage)?;
    transaction
        .execute_batch(OBSERVATION_PROJECTION_BINDING_TRIGGERS_SQL)
        .map_err(reset_storage)?;
    for sql in OBSERVATION_PROJECTION_PERFORMANCE_INDEX_SQL {
        transaction.execute_batch(sql).map_err(reset_storage)?;
    }
    let reset_table_names = reset_tables.iter().map(String::as_str).collect::<Vec<_>>();
    for sql in invariant_trigger_sql_for_tables(&reset_table_names) {
        transaction.execute_batch(sql).map_err(reset_storage)?;
    }
    transaction
        .execute(
            "INSERT OR IGNORE INTO global_schema_migrations(migration) VALUES (?1)",
            [OBSERVATION_SCHEMA_MIGRATION],
        )
        .map_err(reset_storage)?;
    // The observation-authority audit checkpoint attests to rows that no
    // longer exist; clear it so convergence re-audits the recreated authority
    // from the start.
    if table_exists(&transaction, "authority_audit_checkpoints")? {
        transaction
            .execute(
                "DELETE FROM authority_audit_checkpoints WHERE audit_name = 'observation-authority'",
                [],
            )
            .map_err(reset_storage)?;
    }
    transaction.commit().map_err(reset_storage)?;
    Ok(ObservationAuthorityResetV1 {
        reset_tables,
        cleared_session_message_rows,
    })
}

#[cfg(test)]
mod tests;
