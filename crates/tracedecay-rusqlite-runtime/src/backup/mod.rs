//! Verification for completed SQLite snapshots.

use std::{error::Error, fmt, io, path::Path, thread, time::Duration};

use rusqlite::{
    Connection,
    backup::{Backup, StepResult},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sha256Digest(pub [u8; 32]);

pub(crate) trait Cancellation {
    fn is_cancelled(&self) -> bool;
}

pub(crate) trait SqliteBackupFilesystem {
    type Destination;
    type Completed;
    type Error: Error + Send + Sync + 'static;

    fn create_new_private_destination(
        &mut self,
    ) -> Result<(Self::Destination, Connection), Self::Error>;
    fn close_and_sync_destination(
        &mut self,
        destination: Self::Destination,
        connection: Connection,
    ) -> Result<Self::Completed, Self::Error>;
    fn abandon_destination(&mut self, destination: Self::Destination, connection: Connection);
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SqliteBackupOptions;

#[derive(Debug)]
pub(crate) enum SqliteBackupError<E> {
    Cancelled,
    BusyLockedRetryLimitExceeded,
    UnexpectedStepResult,
    Sqlite(rusqlite::Error),
    Filesystem(E),
}

impl<E: fmt::Display> fmt::Display for SqliteBackupError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("SQLite backup cancelled"),
            Self::BusyLockedRetryLimitExceeded => {
                formatter.write_str("SQLite backup exceeded its Busy/Locked retry limit")
            }
            Self::UnexpectedStepResult => {
                formatter.write_str("SQLite returned an unknown backup step result")
            }
            Self::Sqlite(error) => write!(formatter, "SQLite backup failed: {error}"),
            Self::Filesystem(error) => {
                write!(formatter, "SQLite backup filesystem failed: {error}")
            }
        }
    }
}

pub(crate) fn backup_sqlite<F, P>(
    source: &Connection,
    filesystem: &mut F,
    _options: SqliteBackupOptions,
    cancellation: &dyn Cancellation,
    mut progress: P,
) -> Result<F::Completed, SqliteBackupError<F::Error>>
where
    F: SqliteBackupFilesystem,
    P: FnMut(()),
{
    if cancellation.is_cancelled() {
        return Err(SqliteBackupError::Cancelled);
    }
    let (destination, mut destination_connection) = filesystem
        .create_new_private_destination()
        .map_err(SqliteBackupError::Filesystem)?;
    let result = {
        let backup =
            Backup::new(source, &mut destination_connection).map_err(SqliteBackupError::Sqlite);
        match backup {
            Ok(backup) => {
                let mut retries = 0_u32;
                loop {
                    if cancellation.is_cancelled() {
                        break Err(SqliteBackupError::Cancelled);
                    }
                    match backup.step(128).map_err(SqliteBackupError::Sqlite)? {
                        StepResult::Done => break Ok(()),
                        StepResult::More => {
                            progress(());
                            thread::sleep(Duration::from_millis(10));
                        }
                        StepResult::Busy | StepResult::Locked => {
                            if retries >= 20 {
                                break Err(SqliteBackupError::BusyLockedRetryLimitExceeded);
                            }
                            retries += 1;
                            progress(());
                            thread::sleep(Duration::from_millis(10));
                        }
                        _ => break Err(SqliteBackupError::UnexpectedStepResult),
                    }
                }
            }
            Err(error) => Err(error),
        }
    };
    if let Err(error) = result {
        filesystem.abandon_destination(destination, destination_connection);
        return Err(error);
    }
    filesystem
        .close_and_sync_destination(destination, destination_connection)
        .map_err(SqliteBackupError::Filesystem)
}

/// Error returned when a completed snapshot cannot be opened or fails
/// SQLite's read-only quick check.
#[derive(Debug)]
pub enum SnapshotVerificationError {
    Open(io::Error),
    Sqlite(rusqlite::Error),
    Corrupt,
}

impl fmt::Display for SnapshotVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(error) => write!(
                formatter,
                "failed to open immutable SQLite snapshot: {error}"
            ),
            Self::Sqlite(error) => {
                write!(formatter, "SQLite snapshot verification failed: {error}")
            }
            Self::Corrupt => formatter.write_str("SQLite snapshot quick_check reported corruption"),
        }
    }
}

impl Error for SnapshotVerificationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Open(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::Corrupt => None,
        }
    }
}

/// Verifies a completed SQLite backup through the runtime's immutable,
/// read-only `PRAGMA quick_check` authority.
pub fn verify_sqlite_snapshot(path: &Path) -> Result<(), SnapshotVerificationError> {
    let connection = crate::connection::open_immutable_reader(path)
        .map_err(|error| SnapshotVerificationError::Open(io::Error::other(error.to_string())))?;
    let mut statement = connection
        .prepare("PRAGMA quick_check")
        .map_err(SnapshotVerificationError::Sqlite)?;
    let messages = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(SnapshotVerificationError::Sqlite)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(SnapshotVerificationError::Sqlite)?;
    if messages.len() == 1 && messages[0].eq_ignore_ascii_case("ok") {
        Ok(())
    } else {
        Err(SnapshotVerificationError::Corrupt)
    }
}
