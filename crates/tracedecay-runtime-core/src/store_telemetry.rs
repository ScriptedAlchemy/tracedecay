//! Kernel store telemetry: file sizes, pragmas, writer owner, and reader pool.
//!
//! Snapshot wire types (`DatabaseSnapshot`, generation census, registry
//! projection) live in [`crate::runtime_telemetry`]. Callers admit those
//! values and assemble the product snapshot from this kernel collect.

use std::path::{Path, PathBuf};

use crate::db::{Database, WriterOwnership, engine::ReaderPoolSnapshot, probe_writer_owner};
use tracedecay_domain::errors::{Result, TraceDecayError};

/// Kernel facts collected from an already-open store.
#[derive(Debug, Clone)]
pub struct CollectedStoreTelemetry {
    pub project_root: PathBuf,
    pub db_path: PathBuf,
    pub canonical_db_path: PathBuf,
    pub db_size_bytes: u64,
    pub wal_size_bytes: u64,
    pub shm_size_bytes: u64,
    pub journal_mode: Option<String>,
    pub synchronous: Option<i64>,
    pub page_size: Option<u64>,
    pub quick_check_ok: Option<bool>,
    pub quick_check_error: Option<String>,
    pub writer_owner: std::result::Result<WriterOwnership, String>,
    pub reader_pool: Option<ReaderPoolSnapshot>,
}

/// Size of a store file, treating a missing file as zero bytes.
pub fn store_file_size(path: &Path) -> Result<u64> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(TraceDecayError::Database {
            operation: "stat store file".to_string(),
            message: error.to_string(),
        }),
    }
}

/// Appends a suffix (`-wal`, `-shm`, `.dirty`) to a database path.
pub fn store_path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name: std::ffi::OsString = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Collect kernel store telemetry from an admitted database and its paths.
#[hotpath::measure(label = "runtime_ports.database", future = true)]
pub async fn collect_store_telemetry(
    db: &Database,
    project_root: PathBuf,
    db_path: PathBuf,
    include_integrity: bool,
) -> Result<CollectedStoreTelemetry> {
    let canonical_db_path = db_path
        .canonicalize()
        .map_err(|error| TraceDecayError::Database {
            operation: "canonicalize store path".to_string(),
            message: error.to_string(),
        })?;
    let db_size_bytes = store_file_size(&db_path)?;
    let wal_size_bytes = store_file_size(&store_path_with_suffix(&db_path, "-wal"))?;
    let shm_size_bytes = store_file_size(&store_path_with_suffix(&db_path, "-shm"))?;
    let journal_mode = read_journal_mode(db).await.ok();
    let synchronous = db
        .query_scalar_i64("read_synchronous", "PRAGMA synchronous")
        .await
        .ok();
    let page_size = db
        .query_scalar_i64("read_page_size", "PRAGMA page_size")
        .await
        .ok()
        .and_then(|value| u64::try_from(value).ok());
    let (quick_check_ok, quick_check_error) = if include_integrity {
        match db.quick_check_report().await {
            Ok(None) => (Some(true), None),
            Ok(Some(problem)) => (Some(false), Some(problem)),
            Err(error) => (None, Some(error.to_string())),
        }
    } else {
        (None, None)
    };
    let writer_owner = probe_writer_owner(&db_path).map_err(|error| error.to_string());
    let reader_pool = db.read_connection().reader_pool_occupancy();
    Ok(CollectedStoreTelemetry {
        project_root,
        db_path,
        canonical_db_path,
        db_size_bytes,
        wal_size_bytes,
        shm_size_bytes,
        journal_mode,
        synchronous,
        page_size,
        quick_check_ok,
        quick_check_error,
        writer_owner,
        reader_pool,
    })
}

async fn read_journal_mode(db: &Database) -> Result<String> {
    let mut rows = db
        .read_connection()
        .query("PRAGMA journal_mode", ())
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to read journal_mode: {error}"),
            operation: "read_journal_mode".to_string(),
        })?;
    let row = rows
        .next()
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to read journal_mode row: {error}"),
            operation: "read_journal_mode".to_string(),
        })?
        .ok_or_else(|| TraceDecayError::Database {
            message: "no journal_mode row returned".to_string(),
            operation: "read_journal_mode".to_string(),
        })?;
    row.get::<String>(0)
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to decode journal_mode: {error}"),
            operation: "read_journal_mode".to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::store_path_with_suffix;
    use std::path::Path;

    #[test]
    fn store_path_with_suffix_appends_wal() {
        assert_eq!(
            store_path_with_suffix(Path::new("/tmp/graph.db"), "-wal"),
            Path::new("/tmp/graph.db-wal")
        );
    }
}
