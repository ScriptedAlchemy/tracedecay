use std::{
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};
use tracedecay_store::{StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

use super::{
    canonical::manifest_digest,
    model::{ArtifactIdentity, BackupSetId, StagingId, StoredBackupManifest},
    ports::BackupFilesystem,
};
use crate::{
    ExistingWriterLocator, WriterStartError,
    reader::{ExistingReaderLocator, ReaderStartError},
};

const MANIFEST_FILE: &str = "manifest.json";

#[derive(Clone)]
pub struct BackupRoot {
    path: Arc<PathBuf>,
    next_staging: Arc<AtomicU64>,
}

impl BackupRoot {
    pub fn open(path: PathBuf) -> Result<Self, BackupFilesystemError> {
        if !path.is_absolute() {
            return Err(BackupFilesystemError::RootNotAbsolute);
        }
        fs::create_dir_all(&path).map_err(BackupFilesystemError::Io)?;
        set_private_directory(&path)?;
        let path = path.canonicalize().map_err(BackupFilesystemError::Io)?;
        let root = Self {
            path: Arc::new(path),
            next_staging: Arc::new(AtomicU64::new(1)),
        };
        for child in [".staging", ".restore", "sets", "published"] {
            let path = root.path.join(child);
            fs::create_dir_all(&path).map_err(BackupFilesystemError::Io)?;
            set_private_directory(&path)?;
        }
        Ok(root)
    }

    pub(crate) fn create_restore_staging(&self) -> Result<StagingId, BackupFilesystemError> {
        self.create_staging(".restore", "restore")
    }

    pub(crate) fn staged_artifact_path(
        &self,
        staging: &StagingId,
        artifact: &ArtifactIdentity,
    ) -> PathBuf {
        self.staging_path(staging).join(artifact_filename(artifact))
    }

    pub(crate) fn publish_restore(
        &self,
        staging: StagingId,
    ) -> Result<PublishedRestore, BackupFilesystemError> {
        let source = self.staging_path(&staging);
        sync_tree(&source)?;
        let destination = self.path.join("published").join(staging.as_str());
        fs::rename(&source, &destination).map_err(BackupFilesystemError::Io)?;
        sync_directory(&self.path.join("published"))?;
        sync_directory(&self.path.join(".restore"))?;
        Ok(PublishedRestore {
            token: staging.as_str().to_owned(),
        })
    }

    pub(crate) fn abandon_restore(&self, staging: &StagingId) {
        let _ = fs::remove_dir_all(self.staging_path(staging));
    }

    pub(crate) fn create_snapshot_path(&self) -> Result<PathBuf, BackupFilesystemError> {
        let staging = self.create_staging(".staging", "snapshot")?;
        Ok(self.staging_path(&staging).join("snapshot.sqlite3"))
    }

    pub(crate) fn remove_snapshot_path(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// Binds one published store artifact to daemon-verified identity without
    /// exposing the capability-rooted path.
    pub fn bind_published_store(
        &self,
        published: &PublishedRestore,
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
    ) -> Result<PublishedStoreLocators, PublishedLocatorError> {
        let path = self
            .path
            .join("published")
            .join(published.token())
            .join(artifact_filename(&ArtifactIdentity::Store(
                binding.shard_id.clone(),
            )));
        let writer = ExistingWriterLocator::new(binding.clone(), locator.clone(), path.clone())
            .map_err(PublishedLocatorError::Writer)?;
        let reader = ExistingReaderLocator::new(binding, locator, path)
            .map_err(PublishedLocatorError::Reader)?;
        Ok(PublishedStoreLocators { writer, reader })
    }

    pub fn published_store_sha256(
        &self,
        published: &PublishedRestore,
        shard: &tracedecay_store::StoreShardIdV1,
    ) -> Result<super::model::Sha256Digest, BackupFilesystemError> {
        let path = self
            .path
            .join("published")
            .join(published.token())
            .join(artifact_filename(&ArtifactIdentity::Store(shard.clone())));
        Ok(super::canonical::sha256(&read_file(&path)?))
    }

    fn create_staging(
        &self,
        namespace: &str,
        prefix: &str,
    ) -> Result<StagingId, BackupFilesystemError> {
        for _ in 0..32 {
            let sequence = self.next_staging.fetch_add(1, Ordering::Relaxed);
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let staging =
                StagingId::new(format!("{prefix}-{}-{now}-{sequence}", std::process::id()))
                    .map_err(BackupFilesystemError::Manifest)?;
            let path = self.path.join(namespace).join(staging.as_str());
            match fs::create_dir(&path) {
                Ok(()) => {
                    set_private_directory(&path)?;
                    return Ok(staging);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(BackupFilesystemError::Io(error)),
            }
        }
        Err(BackupFilesystemError::StagingExhausted)
    }

    fn staging_path(&self, staging: &StagingId) -> PathBuf {
        let backup = self.path.join(".staging").join(staging.as_str());
        if backup.exists() {
            backup
        } else {
            self.path.join(".restore").join(staging.as_str())
        }
    }

    fn backup_path(&self, backup: &BackupSetId) -> PathBuf {
        self.path.join("sets").join(backup.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedRestore {
    token: String,
}

impl PublishedRestore {
    pub fn token(&self) -> &str {
        &self.token
    }
}

#[derive(Clone, Debug)]
pub struct PublishedStoreLocators {
    writer: ExistingWriterLocator,
    reader: ExistingReaderLocator,
}

impl PublishedStoreLocators {
    pub fn writer(&self) -> &ExistingWriterLocator {
        &self.writer
    }

    pub fn reader(&self) -> &ExistingReaderLocator {
        &self.reader
    }
}

#[derive(Debug)]
pub enum PublishedLocatorError {
    Writer(WriterStartError),
    Reader(ReaderStartError),
}

impl fmt::Display for PublishedLocatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "published restore locator failed: {self:?}")
    }
}

impl Error for PublishedLocatorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Writer(error) => Some(error),
            Self::Reader(error) => Some(error),
        }
    }
}

pub struct FilesystemBackupStore {
    root: BackupRoot,
}

impl FilesystemBackupStore {
    pub fn new(root: BackupRoot) -> Self {
        Self { root }
    }

    pub fn root(&self) -> BackupRoot {
        self.root.clone()
    }
}

#[derive(Debug)]
pub enum BackupFilesystemError {
    RootNotAbsolute,
    StagingExhausted,
    MissingStaging,
    ExistingBackup,
    CorruptManifest,
    Manifest(super::model::ManifestError),
    Io(io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for BackupFilesystemError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "backup filesystem error: {self:?}")
    }
}

impl Error for BackupFilesystemError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl BackupFilesystem for FilesystemBackupStore {
    type Error = BackupFilesystemError;

    fn begin_backup(&mut self, _backup: &BackupSetId) -> Result<StagingId, Self::Error> {
        self.root.create_staging(".staging", "backup")
    }

    fn write_staged(
        &mut self,
        staging: &StagingId,
        artifact: &ArtifactIdentity,
        bytes: &[u8],
    ) -> Result<(), Self::Error> {
        let path = self.root.staged_artifact_path(staging, artifact);
        ensure_staging_parent(&path)?;
        write_new_private(&path, bytes)
    }

    fn read_staged(
        &self,
        staging: &StagingId,
        artifact: &ArtifactIdentity,
    ) -> Result<Vec<u8>, Self::Error> {
        read_file(&self.root.staged_artifact_path(staging, artifact))
    }

    fn write_manifest(
        &mut self,
        staging: &StagingId,
        manifest: &StoredBackupManifest,
    ) -> Result<(), Self::Error> {
        let bytes = serde_json::to_vec(manifest).map_err(BackupFilesystemError::Json)?;
        let path = self.root.staging_path(staging).join(MANIFEST_FILE);
        ensure_staging_parent(&path)?;
        write_new_private(&path, &bytes)
    }

    fn read_staged_manifest(
        &self,
        staging: &StagingId,
    ) -> Result<StoredBackupManifest, Self::Error> {
        read_manifest(&self.root.staging_path(staging).join(MANIFEST_FILE))
    }

    fn commit_backup(
        &mut self,
        staging: StagingId,
        backup: &BackupSetId,
    ) -> Result<(), Self::Error> {
        let source = self.root.staging_path(&staging);
        if !source.is_dir() {
            return Err(BackupFilesystemError::MissingStaging);
        }
        let destination = self.root.backup_path(backup);
        if destination.exists() {
            return Err(BackupFilesystemError::ExistingBackup);
        }
        sync_tree(&source)?;
        fs::rename(&source, &destination).map_err(BackupFilesystemError::Io)?;
        sync_directory(&self.root.path.join("sets"))?;
        sync_directory(&self.root.path.join(".staging"))
    }

    fn abort_staging(&mut self, staging: StagingId) {
        let _ = fs::remove_dir_all(self.root.staging_path(&staging));
    }

    fn load_manifest(&self, backup: &BackupSetId) -> Result<StoredBackupManifest, Self::Error> {
        read_manifest(&self.root.backup_path(backup).join(MANIFEST_FILE))
    }

    fn read_backup(
        &self,
        backup: &BackupSetId,
        artifact: &ArtifactIdentity,
    ) -> Result<Vec<u8>, Self::Error> {
        read_file(
            &self
                .root
                .backup_path(backup)
                .join(artifact_filename(artifact)),
        )
    }
}

fn read_manifest(path: &Path) -> Result<StoredBackupManifest, BackupFilesystemError> {
    let manifest: StoredBackupManifest =
        serde_json::from_slice(&read_file(path)?).map_err(BackupFilesystemError::Json)?;
    if manifest_digest(&manifest.manifest) != manifest.manifest_sha256 {
        return Err(BackupFilesystemError::CorruptManifest);
    }
    Ok(manifest)
}

fn artifact_filename(identity: &ArtifactIdentity) -> String {
    let (prefix, bytes) = match identity {
        ArtifactIdentity::Store(shard) => (
            "store",
            serde_json::to_vec(shard).expect("store shard identities serialize"),
        ),
        ArtifactIdentity::Payload(payload) => ("payload", payload.as_str().as_bytes().to_vec()),
    };
    format!("{prefix}-{}.bin", hex(&Sha256::digest(bytes)))
}

fn ensure_staging_parent(path: &Path) -> Result<(), BackupFilesystemError> {
    path.parent()
        .filter(|parent| parent.is_dir())
        .map(|_| ())
        .ok_or(BackupFilesystemError::MissingStaging)
}

fn write_new_private(path: &Path, bytes: &[u8]) -> Result<(), BackupFilesystemError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(BackupFilesystemError::Io)?;
    file.write_all(bytes).map_err(BackupFilesystemError::Io)?;
    file.sync_all().map_err(BackupFilesystemError::Io)
}

fn read_file(path: &Path) -> Result<Vec<u8>, BackupFilesystemError> {
    let mut file = File::open(path).map_err(BackupFilesystemError::Io)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(BackupFilesystemError::Io)?;
    Ok(bytes)
}

fn sync_tree(path: &Path) -> Result<(), BackupFilesystemError> {
    for entry in fs::read_dir(path).map_err(BackupFilesystemError::Io)? {
        let entry = entry.map_err(BackupFilesystemError::Io)?;
        if entry
            .file_type()
            .map_err(BackupFilesystemError::Io)?
            .is_file()
        {
            File::open(entry.path())
                .and_then(|file| file.sync_all())
                .map_err(BackupFilesystemError::Io)?;
        }
    }
    sync_directory(path)
}

fn sync_directory(path: &Path) -> Result<(), BackupFilesystemError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(BackupFilesystemError::Io)
}

fn set_private_directory(path: &Path) -> Result<(), BackupFilesystemError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(BackupFilesystemError::Io)?;
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a string cannot fail");
            output
        },
    )
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn root_rejects_relative_capabilities() {
        assert!(matches!(
            BackupRoot::open(PathBuf::from("relative")),
            Err(BackupFilesystemError::RootNotAbsolute)
        ));
    }

    #[test]
    fn staged_artifacts_are_not_visible_as_backup_sets() {
        let directory = TempDir::new().unwrap();
        let root = BackupRoot::open(directory.path().join("backups")).unwrap();
        let mut store = FilesystemBackupStore::new(root);
        let backup = BackupSetId::new("set-1").unwrap();
        let staging = store.begin_backup(&backup).unwrap();
        let artifact =
            ArtifactIdentity::Payload(super::super::PayloadId::new("payload-1").unwrap());
        store.write_staged(&staging, &artifact, b"private").unwrap();
        assert!(store.read_backup(&backup, &artifact).is_err());
    }
}
