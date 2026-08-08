use std::{
    cell::Cell,
    error::Error,
    fmt, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

#[cfg(unix)]
use std::fs::File;

use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use tracedecay_store::{
    RuntimeInterruptionV1, RuntimeRequestProbeV1, ShardWatermarkV1, StoreRuntimeBindingV1,
};

use crate::{
    RuntimeWriteAuthority, RuntimeWriteAuthorityStage,
    backup::{
        Cancellation, Sha256Digest, SqliteBackupError, SqliteBackupFilesystem, SqliteBackupOptions,
        backup_sqlite,
    },
    connection::{OpenedDatabaseFile, OpenedDatabaseFileError},
    watermark::CommittedWatermarkPublisher,
};

use super::WriterActorError;

static NEXT_STAGING_FILE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OnlineBackupReceipt {
    pub source_watermark: ShardWatermarkV1,
    pub destination_bytes: u64,
    pub destination_sha256: Sha256Digest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WriterOnlineBackupError {
    DestinationIsNotAbsolute,
    DestinationHasNoFileName,
    DestinationParentUnavailable,
    DestinationIsSource,
    DestinationExists,
    DestinationReplaced,
    Busy,
    Cancelled,
    DeadlineExceeded,
    WriterShuttingDown,
    AuthorityDenied,
    SourceWatermarkUnavailable,
    Sqlite(String),
    Io(String),
}

impl fmt::Display for WriterOnlineBackupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DestinationIsNotAbsolute => {
                formatter.write_str("online backup destination is not absolute")
            }
            Self::DestinationHasNoFileName => {
                formatter.write_str("online backup destination has no file name")
            }
            Self::DestinationParentUnavailable => {
                formatter.write_str("online backup destination parent is unavailable")
            }
            Self::DestinationIsSource => {
                formatter.write_str("online backup destination is the source database")
            }
            Self::DestinationExists => {
                formatter.write_str("online backup destination already exists")
            }
            Self::DestinationReplaced => {
                formatter.write_str("online backup destination was replaced")
            }
            Self::Busy => formatter.write_str("online backup command channel is busy"),
            Self::Cancelled => formatter.write_str("online backup was cancelled"),
            Self::DeadlineExceeded => formatter.write_str("online backup deadline was exceeded"),
            Self::WriterShuttingDown => {
                formatter.write_str("online backup stopped because the writer is shutting down")
            }
            Self::AuthorityDenied => {
                formatter.write_str("online backup runtime write authority was denied")
            }
            Self::SourceWatermarkUnavailable => {
                formatter.write_str("online backup source watermark is unavailable")
            }
            Self::Sqlite(message) => write!(formatter, "online backup SQLite failure: {message}"),
            Self::Io(message) => write!(formatter, "online backup filesystem failure: {message}"),
        }
    }
}

impl Error for WriterOnlineBackupError {}

pub(super) struct OnlineBackupCommand {
    pub(super) destination: PathBuf,
    pub(super) probe: Option<Arc<dyn RuntimeRequestProbeV1>>,
    pub(super) authority: Arc<dyn RuntimeWriteAuthority>,
    reply: oneshot::Sender<Result<OnlineBackupReceipt, WriterActorError>>,
}

impl OnlineBackupCommand {
    pub(super) fn new(
        destination: PathBuf,
        probe: Option<Arc<dyn RuntimeRequestProbeV1>>,
        authority: Arc<dyn RuntimeWriteAuthority>,
        reply: oneshot::Sender<Result<OnlineBackupReceipt, WriterActorError>>,
    ) -> Self {
        Self {
            destination,
            probe,
            authority,
            reply,
        }
    }

    pub(super) fn settle(self, result: Result<OnlineBackupReceipt, WriterActorError>) {
        let _ = self.reply.send(result);
    }
}

pub(super) fn validate_destination(
    source: &Path,
    destination: &Path,
) -> Result<PathBuf, WriterOnlineBackupError> {
    if !destination.is_absolute() {
        return Err(WriterOnlineBackupError::DestinationIsNotAbsolute);
    }
    let file_name = destination
        .file_name()
        .ok_or(WriterOnlineBackupError::DestinationHasNoFileName)?;
    let parent = destination
        .parent()
        .ok_or(WriterOnlineBackupError::DestinationParentUnavailable)?;
    let parent = parent
        .canonicalize()
        .map_err(|_| WriterOnlineBackupError::DestinationParentUnavailable)?;
    if !parent.is_dir() {
        return Err(WriterOnlineBackupError::DestinationParentUnavailable);
    }
    let destination = parent.join(file_name);
    if destination == source {
        return Err(WriterOnlineBackupError::DestinationIsSource);
    }
    if destination
        .try_exists()
        .map_err(|error| WriterOnlineBackupError::Io(error.to_string()))?
    {
        return Err(WriterOnlineBackupError::DestinationExists);
    }
    Ok(destination)
}

pub(super) fn run_online_backup(
    source: &Connection,
    binding: &StoreRuntimeBindingV1,
    watermark_publisher: &CommittedWatermarkPublisher,
    shutdown_requested: &AtomicBool,
    command: OnlineBackupCommand,
) {
    if command
        .authority
        .verify(RuntimeWriteAuthorityStage::Dequeued)
        .is_err()
    {
        command.settle(Err(WriterActorError::AuthorityDenied {
            stage: RuntimeWriteAuthorityStage::Dequeued,
        }));
        return;
    }
    if let Some(interruption) = command
        .probe
        .as_ref()
        .and_then(|probe| probe.interruption())
    {
        command.settle(Err(interruption_error(interruption)));
        return;
    }

    let destination = command.destination.clone();
    let probe = command.probe.clone();
    let authority = Arc::clone(&command.authority);
    let control = BackupControl {
        probe: probe.as_deref(),
        authority: authority.as_ref(),
        shutdown_requested,
        abort: Cell::new(None),
    };
    let mut filesystem = StagedBackupDestination::new(destination.clone());
    let completed = match backup_sqlite(
        source,
        &mut filesystem,
        SqliteBackupOptions,
        &control,
        |_| {},
    ) {
        Ok(completed) => completed,
        Err(error) => {
            let abort = control.abort.get();
            command.settle(Err(map_backup_error(error, abort)));
            return;
        }
    };

    let result = finish_online_backup(
        completed,
        &destination,
        binding,
        watermark_publisher,
        &control,
    )
    .map_err(|error| {
        if error == WriterOnlineBackupError::AuthorityDenied {
            WriterActorError::AuthorityDenied {
                stage: RuntimeWriteAuthorityStage::BeforeCommit,
            }
        } else {
            WriterActorError::OnlineBackupFailed(error)
        }
    });
    command.settle(result);
}

fn finish_online_backup(
    completed: CompletedStaging,
    destination: &Path,
    binding: &StoreRuntimeBindingV1,
    watermark_publisher: &CommittedWatermarkPublisher,
    control: &BackupControl<'_>,
) -> Result<OnlineBackupReceipt, WriterOnlineBackupError> {
    let prepared = (|| {
        verify_sqlite(&completed)?;
        let digest = hash_staging(&completed)?;
        control.check()?;
        let source_watermark = watermark_publisher
            .current(&binding.shard_id)
            .ok_or(WriterOnlineBackupError::SourceWatermarkUnavailable)?;
        Ok((digest, source_watermark))
    })();
    let ((destination_bytes, destination_sha256), source_watermark) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            completed.abandon();
            return Err(error);
        }
    };

    publish_staging(completed, destination, sync_parent)?;
    Ok(OnlineBackupReceipt {
        source_watermark,
        destination_bytes,
        destination_sha256,
    })
}

fn publish_staging(
    completed: CompletedStaging,
    destination: &Path,
    mut sync_parent: impl FnMut(&Path) -> Result<(), WriterOnlineBackupError>,
) -> Result<(), WriterOnlineBackupError> {
    let publication = (|| {
        match fs::hard_link(&completed.path, destination) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(WriterOnlineBackupError::DestinationExists);
            }
            Err(error) => return Err(WriterOnlineBackupError::Io(error.to_string())),
        }
        completed
            .pinned
            .verify_current_path(destination)
            .map_err(|_| WriterOnlineBackupError::DestinationReplaced)?;
        if let Err(sync_error) = sync_parent(destination) {
            let rollback = completed
                .pinned
                .verify_current_path(destination)
                .map_err(file_identity_error)
                .and_then(|()| {
                    fs::remove_file(destination)
                        .map_err(|error| WriterOnlineBackupError::Io(error.to_string()))
                })
                .and_then(|()| sync_parent(destination));
            return match rollback {
                Ok(()) => Err(sync_error),
                Err(rollback_error) => Err(WriterOnlineBackupError::Io(format!(
                    "{sync_error}; failed to roll back uncommitted backup publication: \
                     {rollback_error}"
                ))),
            };
        }
        completed
            .pinned
            .verify_current_path(destination)
            .map_err(|_| WriterOnlineBackupError::DestinationReplaced)?;
        Ok(())
    })();
    completed.abandon();
    publication
}

#[derive(Clone, Copy)]
enum BackupAbort {
    Cancelled,
    DeadlineExceeded,
    WriterShuttingDown,
    AuthorityDenied,
}

struct BackupControl<'a> {
    probe: Option<&'a dyn RuntimeRequestProbeV1>,
    authority: &'a dyn RuntimeWriteAuthority,
    shutdown_requested: &'a AtomicBool,
    abort: Cell<Option<BackupAbort>>,
}

impl BackupControl<'_> {
    fn check(&self) -> Result<(), WriterOnlineBackupError> {
        if self.is_cancelled() {
            Err(match self.abort.get() {
                Some(BackupAbort::Cancelled) => WriterOnlineBackupError::Cancelled,
                Some(BackupAbort::DeadlineExceeded) => WriterOnlineBackupError::DeadlineExceeded,
                Some(BackupAbort::WriterShuttingDown) => {
                    WriterOnlineBackupError::WriterShuttingDown
                }
                Some(BackupAbort::AuthorityDenied) => {
                    return Err(WriterOnlineBackupError::AuthorityDenied);
                }
                None => WriterOnlineBackupError::Cancelled,
            })
        } else {
            Ok(())
        }
    }
}

impl Cancellation for BackupControl<'_> {
    fn is_cancelled(&self) -> bool {
        if self.abort.get().is_some() {
            return true;
        }
        let abort = if self.shutdown_requested.load(Ordering::Acquire) {
            Some(BackupAbort::WriterShuttingDown)
        } else if let Some(interruption) = self.probe.and_then(RuntimeRequestProbeV1::interruption)
        {
            Some(match interruption {
                RuntimeInterruptionV1::Cancelled => BackupAbort::Cancelled,
                RuntimeInterruptionV1::DeadlineExceeded => BackupAbort::DeadlineExceeded,
            })
        } else if self
            .authority
            .verify(RuntimeWriteAuthorityStage::BeforeCommit)
            .is_err()
        {
            Some(BackupAbort::AuthorityDenied)
        } else {
            None
        };
        self.abort.set(abort);
        abort.is_some()
    }
}

struct StagedBackupDestination {
    final_path: PathBuf,
}

impl StagedBackupDestination {
    fn new(final_path: PathBuf) -> Self {
        Self { final_path }
    }
}

struct StagedFile {
    path: PathBuf,
    pinned: OpenedDatabaseFile,
}

impl StagedFile {
    fn abandon(self) {
        let _ = self.pinned.discard_created(&self.path);
    }
}

struct CompletedStaging {
    path: PathBuf,
    pinned: OpenedDatabaseFile,
}

impl CompletedStaging {
    fn abandon(self) {
        let _ = self.pinned.discard_created(&self.path);
    }
}

impl SqliteBackupFilesystem for StagedBackupDestination {
    type Destination = StagedFile;
    type Completed = CompletedStaging;
    type Error = WriterOnlineBackupError;

    fn create_new_private_destination(
        &mut self,
    ) -> Result<(Self::Destination, Connection), Self::Error> {
        let parent = self
            .final_path
            .parent()
            .ok_or(WriterOnlineBackupError::DestinationParentUnavailable)?;
        let file_name = self
            .final_path
            .file_name()
            .ok_or(WriterOnlineBackupError::DestinationHasNoFileName)?
            .to_string_lossy();
        for _ in 0..32 {
            let nonce = NEXT_STAGING_FILE.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                ".{file_name}.tracedecay-backup-{}-{nonce}.tmp",
                std::process::id()
            ));
            // The staging file is pinned through the handle that created it.
            // Re-pinning by pathname would hand back a read-only handle, and
            // `close_and_sync_destination` flushes the staging file through
            // that pin: Windows `FlushFileBuffers` refuses a read-only handle
            // with `ERROR_ACCESS_DENIED`, so every online backup failed there
            // while Unix `fsync` accepted the same read-only descriptor.
            match OpenedDatabaseFile::create_new_or_conflict(&path) {
                Ok(Some(pinned)) => {
                    let connection = match Connection::open_with_flags(
                        &path,
                        OpenFlags::SQLITE_OPEN_READ_WRITE
                            | OpenFlags::SQLITE_OPEN_NO_MUTEX
                            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE,
                    ) {
                        Ok(connection) => connection,
                        Err(error) => {
                            StagedFile { path, pinned }.abandon();
                            return Err(WriterOnlineBackupError::Sqlite(error.to_string()));
                        }
                    };
                    if let Err(error) = pinned.verify_current_path(&path) {
                        drop(connection);
                        StagedFile { path, pinned }.abandon();
                        return Err(file_identity_error(error));
                    }
                    return Ok((StagedFile { path, pinned }, connection));
                }
                Ok(None) => continue,
                Err(error) => return Err(file_identity_error(error)),
            }
        }
        Err(WriterOnlineBackupError::Io(
            "could not allocate a unique online backup staging file".to_owned(),
        ))
    }

    fn close_and_sync_destination(
        &mut self,
        destination: Self::Destination,
        connection: Connection,
    ) -> Result<Self::Completed, Self::Error> {
        if let Err((connection, error)) = connection.close() {
            drop(connection);
            destination.abandon();
            return Err(WriterOnlineBackupError::Sqlite(error.to_string()));
        }
        if let Err(error) = destination.pinned.verify_current_path(&destination.path) {
            destination.abandon();
            return Err(file_identity_error(error));
        }
        if let Err(error) = destination.pinned.sync_all() {
            destination.abandon();
            return Err(file_identity_error(error));
        }
        Ok(CompletedStaging {
            path: destination.path,
            pinned: destination.pinned,
        })
    }

    fn abandon_destination(&mut self, destination: Self::Destination, connection: Connection) {
        drop(connection);
        destination.abandon();
    }
}

fn verify_sqlite(completed: &CompletedStaging) -> Result<(), WriterOnlineBackupError> {
    completed
        .pinned
        .verify_current_path(&completed.path)
        .map_err(file_identity_error)?;
    let connection = Connection::open_with_flags(
        &completed.path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE,
    )
    .map_err(|error| WriterOnlineBackupError::Sqlite(error.to_string()))?;
    let mut statement = connection
        .prepare("PRAGMA quick_check")
        .map_err(|error| WriterOnlineBackupError::Sqlite(error.to_string()))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| WriterOnlineBackupError::Sqlite(error.to_string()))?;
    for row in rows {
        if row.map_err(|error| WriterOnlineBackupError::Sqlite(error.to_string()))? != "ok" {
            return Err(WriterOnlineBackupError::Sqlite(
                "destination quick_check failed".to_owned(),
            ));
        }
    }
    completed
        .pinned
        .verify_current_path(&completed.path)
        .map_err(file_identity_error)
}

fn hash_staging(
    completed: &CompletedStaging,
) -> Result<(u64, Sha256Digest), WriterOnlineBackupError> {
    let mut file = completed.pinned.clone_file().map_err(file_identity_error)?;
    let bytes = file
        .metadata()
        .map_err(|error| WriterOnlineBackupError::Io(error.to_string()))?
        .len();
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| WriterOnlineBackupError::Io(error.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok((bytes, Sha256Digest(hasher.finalize().into())))
}

#[cfg(unix)]
fn sync_parent(destination: &Path) -> Result<(), WriterOnlineBackupError> {
    File::open(
        destination
            .parent()
            .ok_or(WriterOnlineBackupError::DestinationParentUnavailable)?,
    )
    .and_then(|parent| parent.sync_all())
    .map_err(|error| WriterOnlineBackupError::Io(error.to_string()))?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_destination: &Path) -> Result<(), WriterOnlineBackupError> {
    Ok(())
}

fn file_identity_error(error: OpenedDatabaseFileError) -> WriterOnlineBackupError {
    match error {
        OpenedDatabaseFileError::Replaced => WriterOnlineBackupError::DestinationReplaced,
        _ => WriterOnlineBackupError::Io(error.to_string()),
    }
}

fn map_backup_error(
    error: SqliteBackupError<WriterOnlineBackupError>,
    abort: Option<BackupAbort>,
) -> WriterActorError {
    match (error, abort) {
        (_, Some(BackupAbort::AuthorityDenied)) => WriterActorError::AuthorityDenied {
            stage: RuntimeWriteAuthorityStage::BeforeCommit,
        },
        (_, Some(BackupAbort::Cancelled)) => {
            WriterActorError::OnlineBackupFailed(WriterOnlineBackupError::Cancelled)
        }
        (_, Some(BackupAbort::DeadlineExceeded)) => {
            WriterActorError::OnlineBackupFailed(WriterOnlineBackupError::DeadlineExceeded)
        }
        (_, Some(BackupAbort::WriterShuttingDown)) => {
            WriterActorError::OnlineBackupFailed(WriterOnlineBackupError::WriterShuttingDown)
        }
        (SqliteBackupError::Cancelled, None) => {
            WriterActorError::OnlineBackupFailed(WriterOnlineBackupError::Cancelled)
        }
        (SqliteBackupError::Filesystem(error), None) => WriterActorError::OnlineBackupFailed(error),
        (error, None) => {
            WriterActorError::OnlineBackupFailed(WriterOnlineBackupError::Sqlite(error.to_string()))
        }
    }
}

fn interruption_error(interruption: RuntimeInterruptionV1) -> WriterActorError {
    WriterActorError::OnlineBackupFailed(match interruption {
        RuntimeInterruptionV1::Cancelled => WriterOnlineBackupError::Cancelled,
        RuntimeInterruptionV1::DeadlineExceeded => WriterOnlineBackupError::DeadlineExceeded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A staging file must be pinned through a handle that can flush it. The
    /// pin is the same handle the durability step calls `sync_all` on, and
    /// only a handle carrying write access can answer that on Windows -- a
    /// read-only pin failed every online backup there with
    /// `ERROR_ACCESS_DENIED` while passing on Unix, where `fsync` accepts a
    /// read-only descriptor. Asserting the flush directly states the
    /// requirement on every host instead of leaving it to one of them.
    #[test]
    fn a_staged_destination_is_pinned_through_a_flushable_handle() {
        let root = tempfile::tempdir().unwrap();
        let mut filesystem = StagedBackupDestination::new(root.path().join("backup.sqlite3"));
        let (staged, connection) = filesystem.create_new_private_destination().unwrap();

        staged
            .pinned
            .sync_all()
            .expect("the staging pin must flush the file it created");

        filesystem.abandon_destination(staged, connection);
    }

    #[test]
    fn private_destination_replacement_is_detected_and_not_deleted() {
        let root = tempfile::tempdir().unwrap();
        let final_path = root.path().join("backup.sqlite3");
        let mut filesystem = StagedBackupDestination::new(final_path);
        let (staged, connection) = filesystem.create_new_private_destination().unwrap();
        let completed = filesystem
            .close_and_sync_destination(staged, connection)
            .unwrap();
        let staging_path = completed.path.clone();
        let displaced = root.path().join("displaced.sqlite3");
        fs::rename(&completed.path, &displaced).unwrap();
        fs::write(&completed.path, b"replacement").unwrap();

        assert_eq!(
            verify_sqlite(&completed),
            Err(WriterOnlineBackupError::DestinationReplaced)
        );
        completed.abandon();
        assert_eq!(fs::read(staging_path).unwrap(), b"replacement");
        assert!(displaced.exists());
    }

    #[test]
    fn parent_sync_failure_rolls_back_publication_before_removing_staging() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("backup.sqlite3");
        let mut filesystem = StagedBackupDestination::new(destination.clone());
        let (staged, connection) = filesystem.create_new_private_destination().unwrap();
        let completed = filesystem
            .close_and_sync_destination(staged, connection)
            .unwrap();
        let staging_path = completed.path.clone();
        let mut sync_attempts = 0;

        let error = publish_staging(completed, &destination, |_| {
            sync_attempts += 1;
            assert!(staging_path.exists());
            match sync_attempts {
                1 => {
                    assert!(destination.exists());
                    Err(WriterOnlineBackupError::Io(
                        "injected parent sync failure".to_owned(),
                    ))
                }
                2 => {
                    assert!(!destination.exists());
                    Ok(())
                }
                _ => panic!("unexpected parent sync attempt"),
            }
        })
        .unwrap_err();

        assert_eq!(
            error,
            WriterOnlineBackupError::Io("injected parent sync failure".to_owned())
        );
        assert_eq!(sync_attempts, 2);
        assert!(!destination.exists());
        assert!(!staging_path.exists());
    }

    #[test]
    fn destination_replacement_during_parent_sync_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("backup.sqlite3");
        let displaced = root.path().join("displaced.sqlite3");
        let mut filesystem = StagedBackupDestination::new(destination.clone());
        let (staged, connection) = filesystem.create_new_private_destination().unwrap();
        let completed = filesystem
            .close_and_sync_destination(staged, connection)
            .unwrap();
        let staging_path = completed.path.clone();

        let error = publish_staging(completed, &destination, |_| {
            fs::rename(&destination, &displaced).unwrap();
            fs::write(&destination, b"replacement").unwrap();
            Ok(())
        })
        .unwrap_err();

        assert_eq!(error, WriterOnlineBackupError::DestinationReplaced);
        assert_eq!(fs::read(&destination).unwrap(), b"replacement");
        assert!(displaced.exists());
        assert!(!staging_path.exists());
    }
}
