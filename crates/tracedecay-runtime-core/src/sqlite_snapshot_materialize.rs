use std::fs;
use std::io;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use super::control::SnapshotReadControl;
use super::with_suffix;

/// How often SQLite invokes the progress handler during the in-place WAL fold.
/// Matches the crate's other cooperative-cancel cadence. No busy-sleep.
const FOLD_PROGRESS_INTERVAL_OPS: i32 = 1_000;

/// Folds any copied WAL frames of the private scratch family at `path` into
/// its main database file and removes the sidecars, leaving one standalone
/// file the snapshot connection can open immutably.
///
/// The scratch copy is exclusively owned, so leaving WAL journal mode
/// checkpoints every frame in place: it writes only the WAL-resident pages
/// into the copy, resets the header to a rollback journal mode, and deletes
/// the `-wal` sidecar. A `SQLite` backup would instead rewrite every page of
/// the database into a second file, doubling both the bytes written and the
/// peak scratch space for large families.
pub(super) async fn materialize(path: &Path, control: SnapshotReadControl) -> io::Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        control.checkpoint()?;
        let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(io::Error::other)?;
        // Interruptible fold: progress_handler aborts `PRAGMA journal_mode=DELETE`
        // while SQLite is rewriting WAL-resident pages. There is no Backup::step
        // loop and no busy-sleep. If cancel fires, the pragma error is mapped to
        // Interrupted/TimedOut — never Ok.
        connection
            .progress_handler(FOLD_PROGRESS_INTERVAL_OPS, {
                let control = control.clone();
                Some(move || control.checkpoint().is_err())
            })
            .map_err(io::Error::other)?;
        let fold = connection.query_row("PRAGMA journal_mode = DELETE", [], |row| {
            row.get::<_, String>(0)
        });
        let _ = connection.progress_handler(FOLD_PROGRESS_INTERVAL_OPS, None::<fn() -> bool>);
        let mode = match fold {
            Ok(mode) => mode,
            Err(error) => {
                return Err(match control.checkpoint() {
                    Err(cancel) => cancel,
                    Ok(()) => io::Error::other(error),
                });
            }
        };
        if !mode.eq_ignore_ascii_case("delete") {
            return Err(io::Error::other(format!(
                "SQLite left the snapshot copy '{}' in journal mode '{mode}'",
                path.display()
            )));
        }
        drop(connection);
        control.checkpoint()?;
        for suffix in ["-wal", "-shm"] {
            match fs::remove_file(with_suffix(&path, suffix)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    })
    .await
    .map_err(|error| io::Error::other(format!("snapshot materialization task failed: {error}")))?
}
