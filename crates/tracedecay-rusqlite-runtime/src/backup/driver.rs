use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

use rusqlite::Connection;
use tracedecay_store::{
    FrozenWatermarkVectorV1, ShardWatermarkV1, StoreRuntimeBindingV1, StoreShardIdV1,
};

use super::{
    filesystem::{BackupFilesystemError, BackupRoot, PublishedRestore},
    model::{
        ArtifactIdentity, BackupManifest, DeletionState, FrozenFamilySnapshot, PayloadId,
        PrivacyClass, RestoreTarget, SchemaVersion, SnapshotArtifact,
    },
    ports::{BackupDriver, Cancellation},
    sqlite::{SqliteBackupError, SqliteBackupFilesystem, SqliteBackupOptions, backup_sqlite},
    validation::validate_replacement_bindings,
};
use crate::maintenance::ExclusiveMaintenancePermit;

pub struct OnlineBackupSource<'a> {
    watermark: ShardWatermarkV1,
    connection: &'a Connection,
}

impl<'a> OnlineBackupSource<'a> {
    /// Borrows the writer-owned source without opening a competing connection.
    pub fn from_writer_connection(watermark: ShardWatermarkV1, connection: &'a Connection) -> Self {
        Self {
            watermark,
            connection,
        }
    }
}

pub trait RestorePublicationAuthority {
    type Error: Error + Send + Sync + 'static;

    /// Publishes canonical higher bindings after the staged tree is durable and
    /// atomically visible. On error the replacement remains preserved and the
    /// old store must not be reopened for writes.
    fn publish_restored(
        &mut self,
        permit: ExclusiveMaintenancePermit,
        recovery_source: FrozenWatermarkVectorV1,
        replacements: Vec<StoreRuntimeBindingV1>,
        published: PublishedRestore,
    ) -> Result<(), Self::Error>;
}

pub struct SqliteOnlineBackupDriver<'a, A> {
    root: BackupRoot,
    sources: BTreeMap<StoreShardIdV1, OnlineBackupSource<'a>>,
    schema_version: SchemaVersion,
    privacy: PrivacyClass,
    deletion: DeletionState,
    payloads: BTreeMap<PayloadId, Vec<u8>>,
    options: SqliteBackupOptions,
    authority: A,
}

impl<'a, A> SqliteOnlineBackupDriver<'a, A> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        root: BackupRoot,
        sources: impl IntoIterator<Item = OnlineBackupSource<'a>>,
        schema_version: SchemaVersion,
        privacy: PrivacyClass,
        deletion: DeletionState,
        payloads: BTreeMap<PayloadId, Vec<u8>>,
        options: SqliteBackupOptions,
        authority: A,
    ) -> Option<Self> {
        let mut source_map = BTreeMap::new();
        for source in sources {
            if source_map
                .insert(source.watermark.shard_id.clone(), source)
                .is_some()
            {
                return None;
            }
        }
        let sources = source_map;
        (!sources.is_empty()).then_some(Self {
            root,
            sources,
            schema_version,
            privacy,
            deletion,
            payloads,
            options,
            authority,
        })
    }
}

#[derive(Debug)]
pub enum OnlineBackupError {
    RequiredWatermarkMismatch,
    RestorePermitMismatch,
    RestoreTargetNotNewer,
    MissingRestoreArtifact,
    CorruptRestoreArtifact,
    Filesystem(BackupFilesystemError),
    Io(io::Error),
    Sqlite(rusqlite::Error),
    Backup(String),
    Publication(String),
}

impl fmt::Display for OnlineBackupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "SQLite online backup driver failed: {self:?}")
    }
}

impl Error for OnlineBackupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Filesystem(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            _ => None,
        }
    }
}

impl<A: RestorePublicationAuthority> BackupDriver for SqliteOnlineBackupDriver<'_, A> {
    type Error = OnlineBackupError;

    fn freeze_families(
        &mut self,
        required: &FrozenWatermarkVectorV1,
        cancellation: &dyn Cancellation,
    ) -> Result<FrozenFamilySnapshot, Self::Error> {
        let actual = FrozenWatermarkVectorV1::new(
            self.sources.values().map(|source| source.watermark.clone()),
        )
        .map_err(|_| OnlineBackupError::RequiredWatermarkMismatch)?;
        if &actual != required {
            return Err(OnlineBackupError::RequiredWatermarkMismatch);
        }
        let mut artifacts = Vec::with_capacity(self.sources.len() + self.payloads.len());
        for source in self.sources.values() {
            let path = self
                .root
                .create_snapshot_path()
                .map_err(OnlineBackupError::Filesystem)?;
            let mut filesystem = SnapshotDestination {
                path: path.clone(),
                root: self.root.clone(),
            };
            let completed = match backup_sqlite(
                source.connection,
                &mut filesystem,
                self.options,
                cancellation,
                |_| {},
            ) {
                Ok(completed) => completed,
                Err(error) => {
                    self.root.remove_snapshot_path(&path);
                    return Err(backup_error(error));
                }
            };
            let bytes = fs::read(&completed).map_err(OnlineBackupError::Io);
            self.root.remove_snapshot_path(&completed);
            artifacts.push(SnapshotArtifact {
                identity: ArtifactIdentity::Store(source.watermark.shard_id.clone()),
                bytes: bytes?,
            });
        }
        artifacts.extend(
            self.payloads
                .iter()
                .map(|(payload, bytes)| SnapshotArtifact {
                    identity: ArtifactIdentity::Payload(payload.clone()),
                    bytes: bytes.clone(),
                }),
        );
        Ok(FrozenFamilySnapshot {
            frozen_watermarks: actual,
            schema_version: self.schema_version,
            privacy: self.privacy,
            deletion: self.deletion,
            payload_closure: self.payloads.keys().cloned().collect(),
            artifacts,
        })
    }

    fn allocate_restore_target(
        &mut self,
        permit: &ExclusiveMaintenancePermit,
        mut replacement_bindings: Vec<StoreRuntimeBindingV1>,
    ) -> Result<RestoreTarget, Self::Error> {
        if !replacement_bindings
            .iter()
            .any(|binding| binding.shard_id == permit.binding().shard_id)
        {
            return Err(OnlineBackupError::RestorePermitMismatch);
        }
        replacement_bindings.sort_by(|left, right| left.shard_id.cmp(&right.shard_id));
        let staging = self
            .root
            .create_restore_staging()
            .map_err(OnlineBackupError::Filesystem)?;
        Ok(RestoreTarget {
            replacement_bindings,
            staging,
        })
    }

    fn verify_restore(
        &mut self,
        _permit: &ExclusiveMaintenancePermit,
        target: &RestoreTarget,
        manifest: &BackupManifest,
    ) -> Result<FrozenFamilySnapshot, Self::Error> {
        validate_replacement_bindings(&manifest.frozen_watermarks, target)
            .map_err(|_| OnlineBackupError::RestoreTargetNotNewer)?;
        let mut artifacts = Vec::with_capacity(manifest.artifacts.len());
        for artifact in &manifest.artifacts {
            let path = self
                .root
                .staged_artifact_path(&target.staging, &artifact.identity);
            let bytes = fs::read(&path).map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    OnlineBackupError::MissingRestoreArtifact
                } else {
                    OnlineBackupError::Io(error)
                }
            })?;
            if matches!(&artifact.identity, ArtifactIdentity::Store(_)) {
                verify_sqlite_snapshot(&path)?;
            }
            artifacts.push(SnapshotArtifact {
                identity: artifact.identity.clone(),
                bytes,
            });
        }
        Ok(FrozenFamilySnapshot {
            frozen_watermarks: restored_watermarks(&manifest.frozen_watermarks, target)?,
            schema_version: manifest.schema_version,
            privacy: manifest.privacy,
            deletion: manifest.deletion,
            payload_closure: manifest.payload_closure.clone(),
            artifacts,
        })
    }

    fn publish_restore(
        &mut self,
        permit: ExclusiveMaintenancePermit,
        recovery_source: &FrozenWatermarkVectorV1,
        target: RestoreTarget,
    ) -> Result<(), Self::Error> {
        validate_replacement_bindings(recovery_source, &target)
            .map_err(|_| OnlineBackupError::RestoreTargetNotNewer)?;
        let RestoreTarget {
            replacement_bindings,
            staging,
        } = target;
        let published = self
            .root
            .publish_restore(staging)
            .map_err(OnlineBackupError::Filesystem)?;
        self.authority
            .publish_restored(
                permit,
                recovery_source.clone(),
                replacement_bindings,
                published,
            )
            .map_err(|error| OnlineBackupError::Publication(error.to_string()))
    }

    fn abandon_restore(&mut self, _permit: &ExclusiveMaintenancePermit, target: RestoreTarget) {
        self.root.abandon_restore(&target.staging);
    }
}

struct SnapshotDestination {
    path: PathBuf,
    root: BackupRoot,
}

impl SqliteBackupFilesystem for SnapshotDestination {
    type Destination = PathBuf;
    type Completed = PathBuf;
    type Error = OnlineBackupError;

    fn create_new_private_destination(
        &mut self,
    ) -> Result<(Self::Destination, Connection), Self::Error> {
        create_new_private_file(&self.path)?;
        let connection = Connection::open(&self.path).map_err(OnlineBackupError::Sqlite)?;
        Ok((self.path.clone(), connection))
    }

    fn close_and_sync_destination(
        &mut self,
        destination: Self::Destination,
        connection: Connection,
    ) -> Result<Self::Completed, Self::Error> {
        connection
            .close()
            .map_err(|(_, error)| OnlineBackupError::Sqlite(error))?;
        File::open(&destination)
            .and_then(|file| file.sync_all())
            .map_err(OnlineBackupError::Io)?;
        Ok(destination)
    }

    fn abandon_destination(&mut self, destination: Self::Destination, connection: Connection) {
        drop(connection);
        self.root.remove_snapshot_path(&destination);
    }
}

/// Verifies a completed SQLite backup through the runtime's read-only
/// `PRAGMA quick_check` authority.
pub fn verify_sqlite_snapshot(path: &Path) -> Result<(), OnlineBackupError> {
    let connection = crate::connection::open_immutable_reader(path).map_err(|error| {
        OnlineBackupError::Io(io::Error::other(format!(
            "failed to open immutable SQLite snapshot: {error}"
        )))
    })?;
    let mut statement = connection
        .prepare("PRAGMA quick_check")
        .map_err(OnlineBackupError::Sqlite)?;
    let messages = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(OnlineBackupError::Sqlite)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(OnlineBackupError::Sqlite)?;
    if messages.len() == 1 && messages[0].eq_ignore_ascii_case("ok") {
        Ok(())
    } else {
        Err(OnlineBackupError::CorruptRestoreArtifact)
    }
}

fn restored_watermarks(
    source: &FrozenWatermarkVectorV1,
    target: &RestoreTarget,
) -> Result<FrozenWatermarkVectorV1, OnlineBackupError> {
    let replacements = target
        .replacement_bindings
        .iter()
        .map(|binding| (&binding.shard_id, binding))
        .collect::<BTreeMap<_, _>>();
    FrozenWatermarkVectorV1::new(source.iter().map(|(shard_id, watermark)| {
        let replacement = replacements
            .get(shard_id)
            .expect("replacement bindings were validated");
        ShardWatermarkV1 {
            shard_id: shard_id.clone(),
            incarnation: replacement.incarnation,
            authority_epoch: replacement.authority_epoch,
            commit_sequence: watermark.commit_sequence,
        }
    }))
    .map_err(|_| OnlineBackupError::RestoreTargetNotNewer)
}

fn create_new_private_file(path: &PathBuf) -> Result<(), OnlineBackupError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map(drop).map_err(OnlineBackupError::Io)
}

fn backup_error(error: SqliteBackupError<OnlineBackupError>) -> OnlineBackupError {
    OnlineBackupError::Backup(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, convert::Infallible};

    use tempfile::TempDir;

    use super::*;

    struct Authority;

    impl RestorePublicationAuthority for Authority {
        type Error = Infallible;

        fn publish_restored(
            &mut self,
            _permit: ExclusiveMaintenancePermit,
            _recovery_source: FrozenWatermarkVectorV1,
            _replacements: Vec<StoreRuntimeBindingV1>,
            _published: PublishedRestore,
        ) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn online_source_borrows_the_writer_owned_connection() {
        let watermark: ShardWatermarkV1 = serde_json::from_value(serde_json::json!({
            "shard_id": {
                "brain_id": "brain.backup.driver",
                "profile_id": "profile.backup.driver",
                "scope": { "kind": "project", "project_id": "project.backup.driver" }
            },
            "incarnation": 1,
            "authority_epoch": 2,
            "commit_sequence": 3
        }))
        .unwrap();
        let connection = Connection::open_in_memory().unwrap();
        let source = OnlineBackupSource::from_writer_connection(watermark.clone(), &connection);
        assert_eq!(source.watermark, watermark);
    }

    #[test]
    fn online_driver_uses_sqlite_backup_for_frozen_source() {
        let directory = TempDir::new().unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE facts(value INTEGER); INSERT INTO facts VALUES (7);")
            .unwrap();
        let watermark: ShardWatermarkV1 = serde_json::from_value(serde_json::json!({
            "shard_id": {
                "brain_id": "brain.backup.online",
                "profile_id": "profile.backup.online",
                "scope": { "kind": "project", "project_id": "project.backup.online" }
            },
            "incarnation": 1,
            "authority_epoch": 2,
            "commit_sequence": 3
        }))
        .unwrap();
        let required = FrozenWatermarkVectorV1::new([watermark.clone()]).unwrap();
        let source = OnlineBackupSource::from_writer_connection(watermark, &connection);
        let mut driver = SqliteOnlineBackupDriver::new(
            BackupRoot::open(directory.path().join("backups")).unwrap(),
            [source],
            SchemaVersion(1),
            PrivacyClass::Project,
            DeletionState::Live,
            BTreeMap::new(),
            SqliteBackupOptions::default(),
            Authority,
        )
        .unwrap();

        let snapshot = driver
            .freeze_families(
                &required,
                &CancelAfter {
                    checks_remaining: Cell::new(usize::MAX),
                },
            )
            .unwrap();

        assert_eq!(snapshot.frozen_watermarks, required);
        assert_eq!(snapshot.artifacts.len(), 1);
        assert!(matches!(
            &snapshot.artifacts[0].identity,
            ArtifactIdentity::Store(_)
        ));
    }

    struct CancelAfter {
        checks_remaining: Cell<usize>,
    }

    impl Cancellation for CancelAfter {
        fn is_cancelled(&self) -> bool {
            let remaining = self.checks_remaining.get();
            if remaining == 0 {
                true
            } else {
                self.checks_remaining.set(remaining - 1);
                false
            }
        }
    }

    #[test]
    fn online_driver_cancels_between_sqlite_backup_steps() {
        let directory = TempDir::new().unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE facts(value BLOB); INSERT INTO facts VALUES (zeroblob(1048576));",
            )
            .unwrap();
        assert!(
            connection
                .pragma_query_value(None, "page_count", |row| row.get::<_, u32>(0))
                .unwrap()
                > 1
        );
        let watermark: ShardWatermarkV1 = serde_json::from_value(serde_json::json!({
            "shard_id": {
                "brain_id": "brain.backup.cancel",
                "profile_id": "profile.backup.cancel",
                "scope": { "kind": "project", "project_id": "project.backup.cancel" }
            },
            "incarnation": 1,
            "authority_epoch": 2,
            "commit_sequence": 3
        }))
        .unwrap();
        let required = FrozenWatermarkVectorV1::new([watermark.clone()]).unwrap();
        let source = OnlineBackupSource::from_writer_connection(watermark, &connection);
        let mut driver = SqliteOnlineBackupDriver::new(
            BackupRoot::open(directory.path().join("backups")).unwrap(),
            [source],
            SchemaVersion(1),
            PrivacyClass::Project,
            DeletionState::Live,
            BTreeMap::new(),
            SqliteBackupOptions::new(1, 0, std::time::Duration::ZERO, None).unwrap(),
            Authority,
        )
        .unwrap();
        let cancellation = CancelAfter {
            checks_remaining: Cell::new(2),
        };

        let error = driver
            .freeze_families(&required, &cancellation)
            .unwrap_err();

        assert!(matches!(
            error,
            OnlineBackupError::Backup(message) if message.contains("cancelled")
        ));
        assert_eq!(cancellation.checks_remaining.get(), 0);
        assert!(
            std::fs::read_dir(directory.path().join("backups/.staging"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}
