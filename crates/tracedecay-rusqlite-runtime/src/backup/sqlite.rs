//! Bounded use of SQLite's online-backup API.
//!
//! This is a physical primitive, not backup-set orchestration. In particular,
//! it neither discovers databases nor materializes a database as a `Vec<u8>`.

use std::{
    error::Error,
    fmt,
    num::NonZeroI32,
    thread,
    time::{Duration, Instant},
};

use rusqlite::{
    Connection,
    backup::{Backup, StepResult},
};

use super::ports::Cancellation;

pub const MAX_PAGES_PER_STEP: i32 = 4_096;
pub const MAX_STEP_PAUSE: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug)]
pub struct SqliteBackupOptions {
    pages_per_step: NonZeroI32,
    busy_locked_retry_limit: u32,
    step_pause: Duration,
    deadline: Option<Instant>,
}

impl SqliteBackupOptions {
    pub fn new(
        pages_per_step: i32,
        busy_locked_retry_limit: u32,
        step_pause: Duration,
        deadline: Option<Instant>,
    ) -> Result<Self, SqliteBackupConfigurationError> {
        let pages_per_step = NonZeroI32::new(pages_per_step)
            .filter(|pages| (1..=MAX_PAGES_PER_STEP).contains(&pages.get()))
            .ok_or(SqliteBackupConfigurationError::PagesPerStep)?;
        if step_pause > MAX_STEP_PAUSE {
            return Err(SqliteBackupConfigurationError::StepPause);
        }
        Ok(Self {
            pages_per_step,
            busy_locked_retry_limit,
            step_pause,
            deadline,
        })
    }
}

impl Default for SqliteBackupOptions {
    fn default() -> Self {
        Self {
            pages_per_step: NonZeroI32::new(128).expect("128 is non-zero"),
            busy_locked_retry_limit: 20,
            step_pause: Duration::from_millis(10),
            deadline: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqliteBackupConfigurationError {
    PagesPerStep,
    StepPause,
}

impl fmt::Display for SqliteBackupConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PagesPerStep => write!(
                formatter,
                "pages per step must be between 1 and {MAX_PAGES_PER_STEP}"
            ),
            Self::StepPause => write!(
                formatter,
                "step pause must not exceed {} ms",
                MAX_STEP_PAUSE.as_millis()
            ),
        }
    }
}

impl Error for SqliteBackupConfigurationError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SqliteBackupProgress {
    pub remaining_pages: i32,
    pub page_count: i32,
    pub steps: u64,
    pub busy_locked_retries: u32,
}

/// Capability responsible for allocating and durably finishing a private file.
///
/// `create_new_private_destination` must fail if its destination already exists
/// and must not expose that destination publicly. `close_and_sync_destination`
/// receives the live SQLite connection after the backup handle has been
/// dropped; it must close the connection, sync the file, and return only after
/// durability is established. It owns cleanup if that operation fails.
pub trait SqliteBackupFilesystem {
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

#[derive(Debug)]
pub enum SqliteBackupError<E> {
    Cancelled,
    DeadlineExceeded,
    BusyLockedRetryLimitExceeded { retries: u32 },
    UnexpectedStepResult,
    Sqlite(rusqlite::Error),
    Filesystem(E),
}

impl<E: fmt::Display> fmt::Display for SqliteBackupError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(formatter, "SQLite backup cancelled"),
            Self::DeadlineExceeded => write!(formatter, "SQLite backup deadline exceeded"),
            Self::BusyLockedRetryLimitExceeded { retries } => write!(
                formatter,
                "SQLite backup exceeded its Busy/Locked retry limit after {retries} retries"
            ),
            Self::UnexpectedStepResult => {
                write!(formatter, "SQLite returned an unknown backup step result")
            }
            Self::Sqlite(error) => write!(formatter, "SQLite backup failed: {error}"),
            Self::Filesystem(error) => {
                write!(formatter, "SQLite backup filesystem failed: {error}")
            }
        }
    }
}

impl<E: Error + 'static> Error for SqliteBackupError<E> {}

/// Copies one open source connection into a capability-allocated destination.
///
/// The source remains usable throughout. Cancellation and the deadline are
/// observed before allocation and between every bounded `Backup::step` call.
pub fn backup_sqlite<F, P>(
    source: &Connection,
    filesystem: &mut F,
    options: SqliteBackupOptions,
    cancellation: &dyn Cancellation,
    mut progress: P,
) -> Result<F::Completed, SqliteBackupError<F::Error>>
where
    F: SqliteBackupFilesystem,
    P: FnMut(SqliteBackupProgress),
{
    check_interruption(cancellation, options.deadline).map_err(map_drive_error)?;
    let (destination, mut destination_connection) = filesystem
        .create_new_private_destination()
        .map_err(SqliteBackupError::Filesystem)?;

    let result = {
        let backup =
            Backup::new(source, &mut destination_connection).map_err(SqliteBackupError::Sqlite);
        match backup {
            Ok(backup) => drive_steps(options, cancellation, &mut progress, || {
                let status = match backup.step(options.pages_per_step.get())? {
                    StepResult::Done => StepStatus::Done,
                    StepResult::More => StepStatus::More,
                    StepResult::Busy | StepResult::Locked => StepStatus::BusyOrLocked,
                    _ => StepStatus::Unexpected,
                };
                let state = backup.progress();
                Ok(StepObservation {
                    status,
                    remaining_pages: state.remaining,
                    page_count: state.pagecount,
                })
            })
            .map_err(map_drive_error),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StepStatus {
    Done,
    More,
    BusyOrLocked,
    Unexpected,
}

#[derive(Clone, Copy, Debug)]
struct StepObservation {
    status: StepStatus,
    remaining_pages: i32,
    page_count: i32,
}

#[derive(Debug)]
enum DriveError<E> {
    Cancelled,
    DeadlineExceeded,
    BusyLockedRetryLimitExceeded { retries: u32 },
    UnexpectedStepResult,
    Step(E),
}

fn drive_steps<E, P, S>(
    options: SqliteBackupOptions,
    cancellation: &dyn Cancellation,
    progress: &mut P,
    mut step: S,
) -> Result<(), DriveError<E>>
where
    P: FnMut(SqliteBackupProgress),
    S: FnMut() -> Result<StepObservation, E>,
{
    let mut steps = 0_u64;
    let mut retries = 0_u32;
    loop {
        check_interruption(cancellation, options.deadline)?;
        let observation = step().map_err(DriveError::Step)?;
        steps = steps.saturating_add(1);
        if observation.status == StepStatus::BusyOrLocked {
            if retries >= options.busy_locked_retry_limit {
                return Err(DriveError::BusyLockedRetryLimitExceeded { retries });
            }
            retries += 1;
        }
        progress(SqliteBackupProgress {
            remaining_pages: observation.remaining_pages,
            page_count: observation.page_count,
            steps,
            busy_locked_retries: retries,
        });
        match observation.status {
            StepStatus::Done => return Ok(()),
            StepStatus::Unexpected => return Err(DriveError::UnexpectedStepResult),
            StepStatus::More | StepStatus::BusyOrLocked => {
                pause_between_steps(options.step_pause, options.deadline);
            }
        }
    }
}

fn check_interruption<E>(
    cancellation: &dyn Cancellation,
    deadline: Option<Instant>,
) -> Result<(), DriveError<E>> {
    if cancellation.is_cancelled() {
        Err(DriveError::Cancelled)
    } else if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Err(DriveError::DeadlineExceeded)
    } else {
        Ok(())
    }
}

fn pause_between_steps(pause: Duration, deadline: Option<Instant>) {
    let pause = deadline
        .map(|deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .min(pause)
        })
        .unwrap_or(pause);
    if !pause.is_zero() {
        thread::sleep(pause);
    }
}

fn map_drive_error<E>(error: DriveError<rusqlite::Error>) -> SqliteBackupError<E> {
    match error {
        DriveError::Cancelled => SqliteBackupError::Cancelled,
        DriveError::DeadlineExceeded => SqliteBackupError::DeadlineExceeded,
        DriveError::BusyLockedRetryLimitExceeded { retries } => {
            SqliteBackupError::BusyLockedRetryLimitExceeded { retries }
        }
        DriveError::UnexpectedStepResult => SqliteBackupError::UnexpectedStepResult,
        DriveError::Step(error) => SqliteBackupError::Sqlite(error),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        fs::{File, OpenOptions},
        io,
        path::PathBuf,
    };

    use tempfile::TempDir;

    use super::*;

    struct NeverCancel;

    impl Cancellation for NeverCancel {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    struct CancelOnCheck {
        check: Cell<u32>,
        cancel_on: u32,
    }

    impl Cancellation for CancelOnCheck {
        fn is_cancelled(&self) -> bool {
            let check = self.check.get() + 1;
            self.check.set(check);
            check >= self.cancel_on
        }
    }

    #[derive(Debug)]
    enum TestFilesystemError {
        Io(io::Error),
        Sqlite(rusqlite::Error),
    }

    impl fmt::Display for TestFilesystemError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Io(error) => write!(formatter, "{error}"),
                Self::Sqlite(error) => write!(formatter, "{error}"),
            }
        }
    }

    impl Error for TestFilesystemError {}

    struct TestFilesystem {
        path: PathBuf,
        abandoned: bool,
        synced: bool,
    }

    impl SqliteBackupFilesystem for TestFilesystem {
        type Destination = PathBuf;
        type Completed = PathBuf;
        type Error = TestFilesystemError;

        fn create_new_private_destination(
            &mut self,
        ) -> Result<(Self::Destination, Connection), Self::Error> {
            create_new_private_file(&self.path).map_err(TestFilesystemError::Io)?;
            let connection = Connection::open(&self.path).map_err(TestFilesystemError::Sqlite)?;
            Ok((self.path.clone(), connection))
        }

        fn close_and_sync_destination(
            &mut self,
            destination: Self::Destination,
            connection: Connection,
        ) -> Result<Self::Completed, Self::Error> {
            connection
                .close()
                .map_err(|(_, error)| TestFilesystemError::Sqlite(error))?;
            File::open(&destination)
                .and_then(|file| file.sync_all())
                .map_err(TestFilesystemError::Io)?;
            self.synced = true;
            Ok(destination)
        }

        fn abandon_destination(&mut self, destination: Self::Destination, connection: Connection) {
            drop(connection);
            self.abandoned = true;
            let _ = std::fs::remove_file(destination);
        }
    }

    fn create_new_private_file(path: &PathBuf) -> io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path).map(drop)
    }

    fn source_with_rows(rows: usize) -> Connection {
        let mut source = Connection::open_in_memory().unwrap();
        source
            .execute_batch("PRAGMA page_size = 512; CREATE TABLE items(value BLOB);")
            .unwrap();
        let transaction = source.transaction().unwrap();
        {
            let mut insert = transaction
                .prepare("INSERT INTO items(value) VALUES (zeroblob(900))")
                .unwrap();
            for _ in 0..rows {
                insert.execute([]).unwrap();
            }
        }
        transaction.commit().unwrap();
        source
    }

    fn test_filesystem(directory: &TempDir) -> TestFilesystem {
        TestFilesystem {
            path: directory.path().join("backup.sqlite3"),
            abandoned: false,
            synced: false,
        }
    }

    #[test]
    fn completes_in_bounded_steps_and_reports_progress() {
        let directory = TempDir::new().unwrap();
        let source = source_with_rows(20);
        let mut filesystem = test_filesystem(&directory);
        let options = SqliteBackupOptions::new(1, 2, Duration::ZERO, None).unwrap();
        let mut reports = Vec::new();

        let completed = backup_sqlite(
            &source,
            &mut filesystem,
            options,
            &NeverCancel,
            |progress| reports.push(progress),
        )
        .unwrap();

        assert_eq!(completed, filesystem.path);
        assert!(filesystem.synced);
        assert!(reports.len() > 1);
        assert_eq!(reports.last().unwrap().remaining_pages, 0);
    }

    #[test]
    fn cancellation_between_steps_abandons_destination() {
        let directory = TempDir::new().unwrap();
        let source = source_with_rows(20);
        let mut filesystem = test_filesystem(&directory);
        let cancellation = CancelOnCheck {
            check: Cell::new(0),
            cancel_on: 3,
        };

        let error = backup_sqlite(
            &source,
            &mut filesystem,
            SqliteBackupOptions::new(1, 2, Duration::ZERO, None).unwrap(),
            &cancellation,
            |_| {},
        )
        .unwrap_err();

        assert!(matches!(error, SqliteBackupError::Cancelled));
        assert!(filesystem.abandoned);
        assert!(!filesystem.path.exists());
    }

    #[test]
    fn busy_and_locked_retries_stop_at_the_configured_bound() {
        let options = SqliteBackupOptions::new(1, 2, Duration::ZERO, None).unwrap();
        let mut calls = 0;
        let error = drive_steps(
            options,
            &NeverCancel,
            &mut |_| {},
            || -> Result<_, rusqlite::Error> {
                calls += 1;
                Ok(StepObservation {
                    status: StepStatus::BusyOrLocked,
                    remaining_pages: 4,
                    page_count: 4,
                })
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            DriveError::BusyLockedRetryLimitExceeded { retries: 2 }
        ));
        assert_eq!(calls, 3);
    }

    #[test]
    fn completed_destination_is_a_verified_sqlite_snapshot() {
        let directory = TempDir::new().unwrap();
        let source = source_with_rows(7);
        let mut filesystem = test_filesystem(&directory);

        let completed = backup_sqlite(
            &source,
            &mut filesystem,
            SqliteBackupOptions::default(),
            &NeverCancel,
            |_| {},
        )
        .unwrap();
        let destination = Connection::open(completed).unwrap();
        let integrity: String = destination
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        let rows: i64 = destination
            .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
            .unwrap();

        assert_eq!(integrity, "ok");
        assert_eq!(rows, 7);
    }
}
