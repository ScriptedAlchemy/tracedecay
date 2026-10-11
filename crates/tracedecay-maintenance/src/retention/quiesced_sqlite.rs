//! Copy-free reads of a quiesced `SQLite` family.

use std::io;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

/// Opens `database` read-only without locks, sidecars, or a copy, or refuses
/// with [`io::ErrorKind::WouldBlock`] while its `-wal` holds frames.
///
/// `immutable=1` reads only the main file: it takes no lock and never opens,
/// creates, or writes a `-wal` or `-shm`, so nothing beside the store changes.
/// That read is exact only once every frame is checkpointed. A clean runtime
/// shutdown truncates the WAL, so a closed store qualifies; a store with a
/// live writer or a crashed one does not, and its owner must answer instead.
pub(crate) fn open_quiesced(database: &Path) -> io::Result<Connection> {
    let mut wal = database.as_os_str().to_os_string();
    wal.push("-wal");
    match std::fs::symlink_metadata(&wal) {
        Ok(metadata) if metadata.len() > 0 => {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "'{}' has un-checkpointed WAL frames; only its owning runtime can read it",
                    database.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Connection::open_with_flags(
        immutable_uri(database)?,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(io::Error::other)
}

fn immutable_uri(path: &Path) -> io::Result<String> {
    let raw = path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("SQLite path '{}' is not UTF-8", path.display()),
        )
    })?;
    let mut uri = String::with_capacity(raw.len() + 32);
    uri.push_str("file:");
    for ch in raw.chars() {
        match ch {
            '?' => uri.push_str("%3f"),
            '#' => uri.push_str("%23"),
            '%' => uri.push_str("%25"),
            other => uri.push(other),
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    Ok(uri)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn listing(directory: &Path) -> Vec<(String, Vec<u8>)> {
        let mut entries = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    std::fs::read(entry.path()).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    /// Catches a reader that copies, materializes sidecars, or writes beside
    /// the store: the directory must be byte-identical after the read.
    #[test]
    fn checkpointed_wal_store_reads_without_writing_beside_it() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("store #1?.db");
        let writer = Connection::open(&database).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE facts (id INTEGER PRIMARY KEY);
                 INSERT INTO facts VALUES (7);
                 PRAGMA wal_checkpoint(TRUNCATE);",
            )
            .unwrap();
        let before = listing(directory.path());

        let reader = open_quiesced(&database).unwrap();
        let id: i64 = reader
            .query_row("SELECT id FROM facts", [], |row| row.get(0))
            .unwrap();
        drop(reader);

        assert_eq!(id, 7);
        assert_eq!(listing(directory.path()), before);
        drop(writer);
    }

    /// Catches a reader that answers from the main file while committed rows
    /// still sit in the WAL, which would report a stale or empty store.
    #[test]
    fn outstanding_wal_frames_are_refused_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("live.db");
        let writer = Connection::open(&database).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 CREATE TABLE facts (id INTEGER PRIMARY KEY);
                 INSERT INTO facts VALUES (7);",
            )
            .unwrap();
        let before = listing(directory.path());

        let error = open_quiesced(&database).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(listing(directory.path()), before);
        drop(writer);
    }
}
