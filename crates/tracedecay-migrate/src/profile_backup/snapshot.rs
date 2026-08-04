use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};
use tracedecay_graph_db::{
    GraphDb, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphFormatVersion,
    NeverCancelled,
};

const FINAL_CODE_GRAPH_FORMAT_VERSION: u32 = 2;

pub(super) fn is_database_sidecar(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.ends_with("-wal")
        || name.ends_with("-shm")
        || name.ends_with("-journal")
        || name.ends_with(".grafeo.wal")
}

pub(super) fn snapshot_artifact(source: &Path, destination: &Path) -> Result<(), String> {
    let parent = destination
        .parent()
        .ok_or_else(|| "backup artifact destination has no parent".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "create backup artifact parent '{}': {error}",
            parent.display()
        )
    })?;
    if source.extension().and_then(|extension| extension.to_str()) == Some("grafeo") {
        snapshot_grafeo(source, destination)?;
    } else if tracedecay_runtime_core::storage::has_sqlite_database_header(source)
        .map_err(|error| format!("inspect SQLite source '{}': {error}", source.display()))?
    {
        snapshot_sqlite(source, destination)?;
    } else {
        fs::copy(source, destination).map_err(|error| {
            format!(
                "copy backup file '{}' to '{}': {error}",
                source.display(),
                destination.display()
            )
        })?;
        sync_file(destination)?;
    }
    super::sync_directory(parent)
}

pub(super) fn verify_restored_artifact(path: &Path) -> Result<(), String> {
    if path.extension().and_then(|extension| extension.to_str()) == Some("grafeo") {
        return GraphDb::open(GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(path.to_path_buf()),
            expected_format: GraphFormatVersion::new(FINAL_CODE_GRAPH_FORMAT_VERSION)
                .map_err(|error| error.to_string())?,
            durability: GraphDurability::Sync,
            cancellation: Arc::new(NeverCancelled),
        })
        .and_then(|database| database.close())
        .map_err(|error| error.to_string());
    }
    if tracedecay_runtime_core::storage::has_sqlite_database_header(path).map_err(|error| {
        format!(
            "inspect restored SQLite artifact '{}': {error}",
            path.display()
        )
    })? {
        tracedecay_rusqlite_runtime::backup::verify_sqlite_snapshot(path)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn snapshot_sqlite(source: &Path, destination: &Path) -> Result<(), String> {
    let source_connection = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("open SQLite backup source '{}': {error}", source.display()))?;
    let mut destination_connection = Connection::open_with_flags(
        destination,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        format!(
            "create SQLite backup destination '{}': {error}",
            destination.display()
        )
    })?;
    let backup = Backup::new(&source_connection, &mut destination_connection)
        .map_err(|error| format!("start SQLite backup '{}': {error}", source.display()))?;
    let mut retries = 0_u8;
    loop {
        match backup
            .step(128)
            .map_err(|error| format!("copy SQLite backup '{}': {error}", source.display()))?
        {
            StepResult::Done => break,
            StepResult::More => thread::yield_now(),
            StepResult::Busy | StepResult::Locked if retries < 20 => {
                retries += 1;
                thread::sleep(Duration::from_millis(10));
            }
            StepResult::Busy | StepResult::Locked => {
                return Err(format!(
                    "SQLite backup '{}' remained busy or locked",
                    source.display()
                ));
            }
            _ => {
                return Err(format!(
                    "SQLite backup '{}' returned an unknown step result",
                    source.display()
                ));
            }
        }
    }
    drop(backup);
    destination_connection.close().map_err(|(_, error)| {
        format!("close SQLite backup '{}': {error}", destination.display())
    })?;
    tracedecay_rusqlite_runtime::backup::verify_sqlite_snapshot(destination)
        .map_err(|error| error.to_string())?;
    sync_file(destination)
}

fn snapshot_grafeo(source: &Path, destination: &Path) -> Result<(), String> {
    let graph = GraphDb::open(GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(source.to_path_buf()),
        expected_format: GraphFormatVersion::new(FINAL_CODE_GRAPH_FORMAT_VERSION)
            .map_err(|error| error.to_string())?,
        durability: GraphDurability::Sync,
        cancellation: Arc::new(NeverCancelled),
    })
    .map_err(|error| error.to_string())?;
    let native = native_backup_path(destination)?;
    let result = (|| {
        graph
            .backup_full(&native)
            .map_err(|error| error.to_string())?;
        graph.close().map_err(|error| error.to_string())?;
        GraphDb::restore_full_backup(
            &native,
            destination,
            GraphFormatVersion::new(FINAL_CODE_GRAPH_FORMAT_VERSION)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = graph.close();
        let _ = fs::remove_file(destination);
    }
    if native.is_dir() {
        fs::remove_dir_all(&native).map_err(|error| {
            format!(
                "remove materialized Grafeo backup '{}': {error}",
                native.display()
            )
        })?;
        if let Some(parent) = native.parent() {
            super::sync_directory(parent)?;
        }
    }
    result
}

fn native_backup_path(destination: &Path) -> Result<PathBuf, String> {
    let parent = destination
        .parent()
        .ok_or_else(|| "Grafeo snapshot destination has no parent".to_owned())?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Grafeo snapshot destination has no UTF-8 filename".to_owned())?;
    Ok(parent.join(format!(".{name}.native-backup")))
}

fn sync_file(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("sync backup artifact '{}': {error}", path.display()))
}
