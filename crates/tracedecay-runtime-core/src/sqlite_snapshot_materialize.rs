use std::fs;
use std::io;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use super::control::SnapshotReadControl;
use super::with_suffix;

/// The WAL checkpoint loop checks `sqlite3_interrupt` once per frame, but does
/// not invoke `SQLite`'s virtual-machine progress handler. A short wait keeps
/// deadline and cancellation latency bounded without spinning.
const FOLD_INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(1);

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
        let mode = fold_wal_in_place(&connection, &control)?;
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

fn fold_wal_in_place(connection: &Connection, control: &SnapshotReadControl) -> io::Result<String> {
    if control.is_unlimited() {
        return set_delete_journal_mode(connection).map_err(io::Error::other);
    }

    let interrupt = connection.get_interrupt_handle();
    let watcher_control = control.clone();
    let (completed, completion) = mpsc::channel();
    let (fold, cancellation) = thread::scope(|scope| -> io::Result<_> {
        let watcher = thread::Builder::new()
            .name("tracedecay-sqlite-wal-fold-cancel".into())
            .spawn_scoped(scope, move || {
                loop {
                    if let Err(error) = watcher_control.checkpoint() {
                        // Re-interrupt until the fold thread signals
                        // completion: an interrupt delivered before the
                        // PRAGMA statement starts running is a no-op, and a
                        // single lost interrupt must not let the fold run
                        // unbounded past its deadline.
                        loop {
                            interrupt.interrupt();
                            match completion.recv_timeout(FOLD_INTERRUPT_POLL_INTERVAL) {
                                Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                                    return Some(error);
                                }
                                Err(RecvTimeoutError::Timeout) => {}
                            }
                        }
                    }
                    match completion.recv_timeout(FOLD_INTERRUPT_POLL_INTERVAL) {
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => return None,
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                }
            })
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("start SQLite WAL fold cancellation watcher: {error}"),
                )
            })?;
        let fold = set_delete_journal_mode(connection);
        let _ = completed.send(());
        let cancellation = watcher
            .join()
            .map_err(|_| io::Error::other("SQLite WAL fold cancellation watcher panicked"))?;
        Ok((fold, cancellation))
    })?;

    if let Some(error) = cancellation {
        return Err(error);
    }
    fold.map_err(io::Error::other)
}

fn set_delete_journal_mode(connection: &Connection) -> rusqlite::Result<String> {
    connection.query_row("PRAGMA journal_mode = DELETE", [], |row| {
        row.get::<_, String>(0)
    })
}
