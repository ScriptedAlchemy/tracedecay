use std::path::Path;

use libsql::{Builder, Connection, OpenFlags};

use crate::errors::{Result, TraceDecayError};

use super::pragmas;

pub(super) async fn validate_read_only(db_path: &Path) -> Result<()> {
    let db = Builder::new_local(db_path)
        .flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to open database for integrity validation: {e}"),
            operation: "validate_integrity".to_string(),
        })?;
    let conn = db.connect().map_err(|e| TraceDecayError::Database {
        message: format!("failed to connect for integrity validation: {e}"),
        operation: "validate_integrity".to_string(),
    })?;
    let file_size = std::fs::metadata(db_path).map_or(0, |metadata| metadata.len());
    pragmas::apply_read_only(&conn, file_size).await?;
    validate(&conn, "validate_integrity").await
}

pub(super) async fn quick_check_result(
    conn: &Connection,
    operation: &str,
    query_error: &str,
) -> Result<Option<String>> {
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

pub(super) async fn validate(conn: &Connection, operation: &str) -> Result<()> {
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
