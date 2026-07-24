use std::path::Path;

use crate::db::engine::QueryExecutor;
use crate::errors::{Result, TraceDecayError};

pub(super) async fn validate_read_only(db_path: &Path) -> Result<()> {
    let scratch_root = crate::storage::default_profile_root()
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to resolve the integrity-validation snapshot root: {error}"),
            operation: "validate_integrity".to_string(),
        })?
        .join("scratch")
        .join("sqlite-read");
    let snapshot = crate::sqlite_read_snapshot::open_in(db_path, &scratch_root)
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to open database for integrity validation: {error}"),
            operation: "validate_integrity".to_string(),
        })?;
    validate(snapshot.connection(), "validate_integrity").await?;
    snapshot
        .validate_source()
        .map_err(|error| TraceDecayError::Database {
            message: format!("database family changed during integrity validation: {error}"),
            operation: "validate_integrity".to_string(),
        })
}

pub(super) async fn quick_check_result<C>(
    conn: &C,
    operation: &str,
    query_error: &str,
) -> Result<Option<String>>
where
    C: QueryExecutor + ?Sized,
{
    let mut rows =
        conn.query("PRAGMA quick_check", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("{query_error}: {e}"),
                operation: operation.to_string(),
            })?;
    rows.next()
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to read quick_check result: {e}"),
            operation: operation.to_string(),
        })
        .map(|row| row.map(|row| row.get::<String>(0).unwrap_or_default()))
}

pub(super) async fn validate<C>(conn: &C, operation: &str) -> Result<()>
where
    C: QueryExecutor + ?Sized,
{
    let result = quick_check_result(conn, operation, "failed to run read-only quick_check")
        .await?
        .ok_or_else(|| TraceDecayError::Database {
            message: "quick_check returned no result".to_string(),
            operation: operation.to_string(),
        })?;
    if result == "ok" {
        Ok(())
    } else {
        Err(TraceDecayError::Database {
            message: format!("database quick_check failed: {result}"),
            operation: operation.to_string(),
        })
    }
}

pub(super) fn read_only_upgrade_error(db_path: &Path, operation: &str) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!(
            "cannot upgrade the daemon's shared read-only connection at '{}' to writable; acquire writable ownership before opening read handles",
            db_path.display()
        ),
        operation: operation.to_string(),
    }
}

pub(super) fn validate_sqlite_header(
    db_path: &Path,
    operation: &str,
    allow_fresh_path: bool,
) -> Result<()> {
    match std::fs::metadata(db_path) {
        Ok(metadata) if allow_fresh_path && metadata.len() == 0 => return Ok(()),
        Ok(_) => {}
        Err(e) if allow_fresh_path && e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(TraceDecayError::Database {
                message: format!(
                    "failed to inspect database path at '{}': {e}",
                    db_path.display()
                ),
                operation: operation.to_string(),
            });
        }
    }
    match crate::storage::has_sqlite_database_header(db_path) {
        Ok(true) => Ok(()),
        Ok(false) => Err(TraceDecayError::Database {
            message: format!(
                "file is not a database: SQLite header is missing at '{}'",
                db_path.display()
            ),
            operation: operation.to_string(),
        }),
        Err(e) => Err(TraceDecayError::Database {
            message: format!(
                "failed to read database header at '{}': {e}",
                db_path.display()
            ),
            operation: operation.to_string(),
        }),
    }
}
