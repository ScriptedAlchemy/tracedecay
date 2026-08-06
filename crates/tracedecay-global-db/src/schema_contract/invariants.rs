use super::super::{global_db_operation_error, global_db_operation_message};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor};

mod audit;
mod cursor_coverage;
mod rows;
#[cfg(test)]
mod test_fixture;
mod triggers;

use audit::{AuditCheckpoint, validate_projection_authority_suffix};
use cursor_coverage::validate_observation_cursor_coverage;
use rows::{
    observation_row_audit_covers, query_has_rows, validate_observation_authority_rows,
    validate_receipt_authority_rows, validate_source_cursor_authority_rows,
};
use triggers::FOREIGN_KEY_AUDIT_QUERY;
pub(super) use triggers::{INVARIANTS, Trigger};
const OPERATION: &str = "ensure global database authority invariants";

/// Rows an authority row audit may ask the SQL channel for at once.
///
/// The channel materializes an entire result set before yielding row one and
/// rejects anything past `MAX_QUERY_ROWS` (`10_000`) or 64 MiB. Every audit below
/// walks a table (or a checkpoint suffix of one) whose length grows with the
/// store, so each scan pages with a keyset cursor instead of requesting one
/// unbounded result set. Long-lived daemons also retain allocator arenas sized
/// for the largest page, so keep this comfortably below the hard channel cap;
/// read-only validation can afford the additional round trips.
pub(super) const AUDIT_PAGE_ROWS: i64 = 128;

/// Page size for scans that carry a full observation payload.
///
/// Canonical observation records may approach the 1 MiB observation contract
/// ceiling. The audit no longer carries a duplicate receipt JSON payload, so
/// forty-eight rows leave headroom under the channel's 64 MiB materialization
/// limit while avoiding tens of thousands of SQL-channel round trips on a
/// production-sized store.
pub(super) const OBSERVATION_AUDIT_PAGE_ROWS: i64 = 48;

pub async fn ensure_authority_invariant_schema(
    conn: &impl Executor,
) -> tracedecay_runtime_core::errors::Result<()> {
    for invariant in INVARIANTS {
        for trigger in invariant.triggers {
            conn.execute_batch(trigger.create_sql)
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
        }
    }
    Ok(())
}

pub(super) async fn validate_invariant_rows(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    for invariant in INVARIANTS {
        if observation_row_audit_covers(invariant) {
            continue;
        }
        if let Some(query) = invariant.audit_query
            && query != FOREIGN_KEY_AUDIT_QUERY
            && query_has_rows(conn, query).await?
        {
            return Err(global_db_operation_message(OPERATION, invariant.violation));
        }
    }
    Ok(())
}

async fn foreign_key_violation_exists_read_only(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT schema.name
             FROM sqlite_schema AS schema
             JOIN pragma_foreign_key_list(schema.name) AS foreign_key
             WHERE schema.type = 'table'
             ORDER BY schema.name",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut tables = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        tables.push(
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        );
    }
    drop(rows);

    for table in tables {
        let mut rows = conn
            .query(
                "SELECT 1 FROM pragma_foreign_key_check(?1) LIMIT 1",
                (table,),
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        if rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub async fn validate_authority_rows_exhaustive(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    validate_receipt_authority_rows(conn, 0).await?;
    validate_observation_authority_rows(conn, 0).await?;
    validate_source_cursor_authority_rows(conn).await?;
    validate_observation_cursor_coverage(conn, 0).await?;
    validate_projection_authority_suffix(conn, AuditCheckpoint::default()).await?;
    if foreign_key_violation_exists_read_only(conn).await? {
        return Err(global_db_operation_message(
            OPERATION,
            "global database contains a foreign-key violation",
        ));
    }
    validate_invariant_rows(conn).await
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::foreign_key_violation_exists_read_only;
    use tracedecay_runtime_core::db::engine::TestConnection;

    #[tokio::test]
    async fn foreign_key_audit_finds_violations_by_child_table() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        let connection = rusqlite::Connection::open(&database_path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = OFF;
                 CREATE TABLE parent (id INTEGER PRIMARY KEY);
                 CREATE TABLE child (
                    id INTEGER PRIMARY KEY,
                    parent_id INTEGER NOT NULL REFERENCES parent(id)
                 );
                 INSERT INTO child(id, parent_id) VALUES (1, 99);",
            )
            .unwrap();
        drop(connection);
        let connection = TestConnection::open(&database_path);

        assert!(
            foreign_key_violation_exists_read_only(&connection)
                .await
                .unwrap()
        );
    }
}
