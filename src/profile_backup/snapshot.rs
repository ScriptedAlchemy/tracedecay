//! Database-aware artifact snapshots for complete profile backups.
//!
//! A complete profile backup must capture every database as a verified,
//! self-contained artifact instead of a blind byte copy: SQLite files are
//! snapshotted through the SQLite backup API (folding any WAL), and Grafeo
//! graph stores are rebuilt through the graph database's verified
//! backup/restore path (folding any WAL sidecar and proving the artifact
//! opens under the current format authority). Write-ahead and shared-memory
//! sidecars are therefore excluded from the backup inventory.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};
use tracedecay_graph_db::{GraphCancellation, GraphDb, GraphDbError, NeverCancelled};

use super::ProfileBackupError;

fn never_cancelled() -> Arc<dyn GraphCancellation> {
    Arc::new(NeverCancelled)
}

/// Preserves the graph database's typed failure taxonomy across the profile
/// backup boundary instead of collapsing every failure into one category.
fn map_graph_error(context: &str, path: &Path, error: GraphDbError) -> ProfileBackupError {
    let message = format!("{context} '{}': {error}", path.display());
    match error {
        GraphDbError::Corrupt { .. }
        | GraphDbError::ResetRequired { .. }
        | GraphDbError::ProjectionMismatch { .. }
        | GraphDbError::GenerationMismatch { .. } => ProfileBackupError::corrupt(message),
        GraphDbError::Conflict => ProfileBackupError::conflict(message),
        GraphDbError::InvalidRequest { .. } => ProfileBackupError::invalid(message),
        GraphDbError::DurabilityUncertain { .. }
        | GraphDbError::Cancelled
        | GraphDbError::BudgetExhausted
        | GraphDbError::DeadlineExceeded
        | GraphDbError::Unavailable { .. }
        | GraphDbError::Closed => ProfileBackupError::unavailable(message),
    }
}

pub(super) fn is_database_sidecar(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.ends_with("-wal")
        || name.ends_with("-shm")
        || name.ends_with("-journal")
        || name.ends_with(".grafeo.wal")
}

pub(super) fn snapshot_artifact(
    source: &Path,
    destination: &Path,
) -> Result<(), ProfileBackupError> {
    let parent = destination
        .parent()
        .ok_or_else(|| ProfileBackupError::invalid("backup artifact destination has no parent"))?;
    fs::create_dir_all(parent).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "create backup artifact parent '{}': {error}",
            parent.display()
        ))
    })?;
    if source.extension().and_then(|extension| extension.to_str()) == Some("grafeo") {
        snapshot_graph_store(source, destination)?;
    } else if tracedecay_runtime_core::storage::has_sqlite_database_header(source).map_err(
        |error| {
            ProfileBackupError::unavailable(format!(
                "inspect SQLite source '{}': {error}",
                source.display()
            ))
        },
    )? {
        snapshot_sqlite(source, destination)?;
    } else {
        fs::copy(source, destination).map_err(|error| {
            ProfileBackupError::unavailable(format!(
                "copy backup file '{}' to '{}': {error}",
                source.display(),
                destination.display()
            ))
        })?;
        sync_file(destination)?;
    }
    super::sync_directory(parent)
}

/// Verifies a restored database artifact by opening it through its engine.
///
/// Byte-identity with the backup inventory is verified separately; this
/// proves the restored artifact is actually openable before publication.
pub(super) fn verify_restored_artifact(path: &Path) -> Result<(), ProfileBackupError> {
    if path.extension().and_then(|extension| extension.to_str()) == Some("grafeo") {
        return GraphDb::verify_closed_store(path, &never_cancelled())
            .map_err(|error| map_graph_error("verify restored graph store", path, error));
    }
    if tracedecay_runtime_core::storage::has_sqlite_database_header(path).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "inspect restored SQLite artifact '{}': {error}",
            path.display()
        ))
    })? {
        tracedecay_rusqlite_runtime::backup::verify_sqlite_snapshot(path).map_err(|error| {
            ProfileBackupError::corrupt(format!(
                "restored SQLite artifact '{}' failed verification: {error}",
                path.display()
            ))
        })?;
    }
    Ok(())
}

fn snapshot_sqlite(source: &Path, destination: &Path) -> Result<(), ProfileBackupError> {
    let source_connection = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "open SQLite backup source '{}': {error}",
            source.display()
        ))
    })?;
    let mut destination_connection = Connection::open_with_flags(
        destination,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "create SQLite backup destination '{}': {error}",
            destination.display()
        ))
    })?;
    let backup = Backup::new(&source_connection, &mut destination_connection).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "start SQLite backup '{}': {error}",
            source.display()
        ))
    })?;
    let mut retries = 0_u8;
    loop {
        match backup.step(128).map_err(|error| {
            ProfileBackupError::unavailable(format!(
                "copy SQLite backup '{}': {error}",
                source.display()
            ))
        })? {
            StepResult::Done => break,
            StepResult::More => thread::yield_now(),
            StepResult::Busy | StepResult::Locked if retries < 20 => {
                retries += 1;
                thread::sleep(Duration::from_millis(10));
            }
            StepResult::Busy | StepResult::Locked => {
                return Err(ProfileBackupError::denied(format!(
                    "SQLite backup '{}' remained busy or locked",
                    source.display()
                )));
            }
            _ => {
                return Err(ProfileBackupError::unavailable(format!(
                    "SQLite backup '{}' returned an unknown step result",
                    source.display()
                )));
            }
        }
    }
    drop(backup);
    destination_connection.close().map_err(|(_, error)| {
        ProfileBackupError::unavailable(format!(
            "close SQLite backup '{}': {error}",
            destination.display()
        ))
    })?;
    tracedecay_rusqlite_runtime::backup::verify_sqlite_snapshot(destination).map_err(|error| {
        ProfileBackupError::corrupt(format!(
            "SQLite backup snapshot '{}' failed verification: {error}",
            destination.display()
        ))
    })?;
    sync_file(destination)
}

/// Rebuilds a closed Grafeo store into a verified single-file snapshot by
/// running it through the graph database's fenced backup and restore path.
fn snapshot_graph_store(source: &Path, destination: &Path) -> Result<(), ProfileBackupError> {
    let cancellation = never_cancelled();
    let native = native_backup_path(destination)?;
    let result = GraphDb::create_verified_backup(source, &native, &cancellation)
        .and_then(|_| GraphDb::restore_verified_backup(&native, destination, &cancellation))
        .map(|_| ())
        .map_err(|error| map_graph_error("snapshot graph store", source, error));
    if native.is_dir() {
        fs::remove_dir_all(&native).map_err(|error| {
            ProfileBackupError::unavailable(format!(
                "remove materialized graph backup '{}': {error}",
                native.display()
            ))
        })?;
        if let Some(parent) = native.parent() {
            super::sync_directory(parent)?;
        }
    }
    result
}

fn native_backup_path(destination: &Path) -> Result<PathBuf, ProfileBackupError> {
    let parent = destination
        .parent()
        .ok_or_else(|| ProfileBackupError::invalid("graph snapshot destination has no parent"))?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            ProfileBackupError::invalid("graph snapshot destination has no UTF-8 filename")
        })?;
    Ok(parent.join(format!(".{name}.native-backup")))
}

fn sync_file(path: &Path) -> Result<(), ProfileBackupError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            ProfileBackupError::unavailable(format!(
                "sync backup artifact '{}': {error}",
                path.display()
            ))
        })
}
