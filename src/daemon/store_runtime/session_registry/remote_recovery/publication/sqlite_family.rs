use std::path::Path;

use tracedecay_runtime_core::storage::PrivateStoreIo;

use super::super::{Result, session_registry_error};
use super::sqlite_sidecar;

pub(super) fn checkpoint_for_publication(path: &Path) -> Result<()> {
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|error| {
        session_registry_error("open remote restore SQLite family", error.to_string())
    })?;
    let (busy, log_frames, checkpointed_frames): (i64, i64, i64) = connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", (), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(|error| {
            session_registry_error("checkpoint remote restore SQLite family", error.to_string())
        })?;
    if busy != 0 || log_frames != checkpointed_frames {
        return Err(session_registry_error(
            "checkpoint remote restore SQLite family",
            format!(
                "checkpoint remained busy={busy}, log_frames={log_frames}, checkpointed_frames={checkpointed_frames}"
            ),
        ));
    }
    drop(connection);
    for suffix in ["wal", "shm"] {
        let sidecar = sqlite_sidecar(path, suffix);
        match std::fs::remove_file(&sidecar) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(session_registry_error(
                    "remove checkpointed remote restore SQLite sidecar",
                    error.to_string(),
                ));
            }
        }
    }
    PrivateStoreIo::sync_sqlite_family(path).map_err(|error| {
        session_registry_error(
            "sync checkpointed remote restore SQLite family",
            error.to_string(),
        )
    })
}
