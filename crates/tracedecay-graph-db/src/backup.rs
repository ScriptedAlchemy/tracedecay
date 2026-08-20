//! Verified full backup and restore for closed persistent graph stores.
//!
//! Backups are directories holding the native Grafeo full-backup segments
//! plus a manifest that pins the graph format, the fenced target epoch, and
//! a SHA-256 inventory of every artifact. Restore refuses partial, tampered,
//! or wrong-format material before publishing anything, verifies the
//! restored store by opening it under the current format authority, and
//! publishes atomically so an interrupted restore never leaves a
//! half-written destination.
//!
//! Both operations address a *closed* store by path: the caller holds the
//! outer store/profile authority (for example the quiesced-service profile
//! lease during a complete profile backup). Grafeo's exclusive file lock
//! turns a concurrently open store into a typed failure instead of a torn
//! copy.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use grafeo_common::types::EpochId;
use grafeo_engine::GrafeoDB;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::location::PersistentGraphStoreState;
use crate::{
    GraphCancellation, GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability,
    GraphFormatVersion,
};

const MANIFEST_FILE: &str = "tracedecay-graph-backup.json";
const MANIFEST_SCHEMA_VERSION: u32 = 1;
const NATIVE_SEGMENT_DIR: &str = "native";
static NEXT_STAGING_ID: AtomicU64 = AtomicU64::new(1);

/// Identity of a verified graph backup: the pinned format, the fenced epoch
/// the backup covers, and the digest of its artifact manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphBackupReceipt {
    pub graph_format_version: u32,
    pub target_epoch: u64,
    pub artifact_count: usize,
    pub manifest_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GraphBackupManifest {
    schema_version: u32,
    graph_format_version: u32,
    target_epoch: u64,
    artifacts: Vec<GraphBackupArtifact>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct GraphBackupArtifact {
    logical_path: String,
    byte_len: u64,
    sha256: String,
}

impl GraphDb {
    /// Creates a verified full backup of the closed graph store at `source`
    /// into the new directory `destination`.
    ///
    /// The store is opened exclusively under the current format authority
    /// (folding any write-ahead-log sidecar into the fenced snapshot), the
    /// native full backup is staged and hashed, and the backup directory is
    /// published atomically only after the source store closed durably.
    pub fn create_verified_backup(
        source: &Path,
        destination: &Path,
        cancellation: &Arc<dyn GraphCancellation>,
    ) -> Result<GraphBackupReceipt, GraphDbError> {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let source_metadata = fs::symlink_metadata(source).map_err(|error| {
            GraphDbError::invalid(format!(
                "graph backup source '{}' is not readable: {error}",
                source.display()
            ))
        })?;
        if source_metadata.file_type().is_symlink() || !source_metadata.is_file() {
            return Err(GraphDbError::invalid(format!(
                "graph backup source '{}' must be a regular graph database file",
                source.display()
            )));
        }
        let (parent, file_name) = validate_new_directory(destination)?;
        let destination = parent.join(&file_name);
        let database = GraphDb::open_with_store_state(
            GraphDbOpenOptions {
                location: GraphDbLocation::Persistent(source.to_path_buf()),
                expected_format: GraphFormatVersion::current(),
                durability: GraphDurability::WalSync,
                cancellation: Arc::clone(cancellation),
            },
            Some(PersistentGraphStoreState::Existing),
        )?;
        let staging = parent.join(format!(
            ".{}.tracedecay-graph-backup-{}.tmp",
            file_name,
            NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let staged = create_private_directory(&staging).and_then(|()| {
            create_staged_full(&database, cancellation.as_ref(), &staging)
                .inspect_err(|_| remove_backup_staging(&staging))
        });
        let receipt = match staged {
            Ok(receipt) => receipt,
            Err(error) => {
                let _ = database.close();
                return Err(error);
            }
        };
        if let Err(error) = database.close() {
            remove_backup_staging(&staging);
            return Err(error);
        }
        if destination
            .try_exists()
            .map_err(|error| unavailable_io("inspect backup destination", &destination, error))?
        {
            remove_backup_staging(&staging);
            return Err(GraphDbError::Conflict);
        }
        fs::rename(&staging, &destination).map_err(|error| {
            remove_backup_staging(&staging);
            unavailable_io("publish graph backup", &destination, error)
        })?;
        if let Err(error) = sync_directory(&parent) {
            return Err(GraphDbError::DurabilityUncertain {
                message: format!(
                    "graph backup '{}' was renamed but its parent directory did not sync: {error}",
                    destination.display()
                ),
            });
        }
        Ok(receipt)
    }

    /// Restores the verified backup at `backup_root` into the new graph
    /// database file `destination`.
    ///
    /// Every artifact is re-verified against the manifest before anything is
    /// written, the fenced epoch is rebuilt into a staging file, the staged
    /// store must open cleanly under the current format authority, and the
    /// destination is published atomically with rollback on failure.
    pub fn restore_verified_backup(
        backup_root: &Path,
        destination: &Path,
        cancellation: &Arc<dyn GraphCancellation>,
    ) -> Result<GraphBackupReceipt, GraphDbError> {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let expected_format = GraphFormatVersion::current();
        let verified = load_verified(backup_root)?;
        if verified.manifest.graph_format_version != expected_format.get() {
            return Err(GraphDbError::ResetRequired {
                message: format!(
                    "graph backup format mismatch: expected {}, found {}",
                    expected_format.get(),
                    verified.manifest.graph_format_version
                ),
            });
        }
        let destination = validate_destination(destination)?;
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let staging = staging_file(&destination, "restore")?;
        let result = restore_to_staging(
            &verified.root,
            &staging,
            cancellation,
            verified.manifest.target_epoch,
        );
        if let Err(error) = result {
            remove_restore_staging(&staging);
            return Err(error);
        }
        publish_file(&staging, &destination)?;
        Ok(verified.receipt)
    }

    /// Verifies that the closed graph store at `path` opens cleanly under
    /// the current format authority, then closes it again.
    pub fn verify_closed_store(
        path: &Path,
        cancellation: &Arc<dyn GraphCancellation>,
    ) -> Result<(), GraphDbError> {
        let database = GraphDb::open_with_store_state(
            GraphDbOpenOptions {
                location: GraphDbLocation::Persistent(path.to_path_buf()),
                expected_format: GraphFormatVersion::current(),
                durability: GraphDurability::WalSync,
                cancellation: Arc::clone(cancellation),
            },
            Some(PersistentGraphStoreState::Existing),
        )?;
        database.close()
    }
}

fn create_staged_full(
    database: &GraphDb,
    cancellation: &dyn GraphCancellation,
    staging: &Path,
) -> Result<GraphBackupReceipt, GraphDbError> {
    if cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    let native = staging.join(NATIVE_SEGMENT_DIR);
    create_private_directory(&native)?;
    let guard = database.write_guard()?;
    let engine = guard.as_ref().ok_or(GraphDbError::Closed)?;
    let segment = engine
        .backup_full(&native)
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    let target_epoch = segment.end_epoch.as_u64();
    drop(guard);
    if cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    let artifacts = collect_artifacts(staging)?;
    if artifacts.is_empty() {
        return Err(GraphDbError::Corrupt {
            message: "Grafeo full backup produced no artifacts".to_owned(),
        });
    }
    let manifest = GraphBackupManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        graph_format_version: GraphFormatVersion::current().get(),
        target_epoch,
        artifacts,
    };
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    write_new_synced(&staging.join(MANIFEST_FILE), &bytes)?;
    sync_directory(&native)?;
    sync_directory(staging)?;
    Ok(receipt(&manifest, &bytes))
}

fn restore_to_staging(
    backup_root: &Path,
    staging: &Path,
    cancellation: &Arc<dyn GraphCancellation>,
    target_epoch: u64,
) -> Result<(), GraphDbError> {
    GrafeoDB::restore_to_epoch(
        &backup_root.join(NATIVE_SEGMENT_DIR),
        EpochId::new(target_epoch),
        staging,
    )
    .map_err(|error| GraphDbError::Corrupt {
        message: format!("Grafeo backup restore failed: {error}"),
    })?;
    if cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    GraphDb::verify_closed_store(staging, cancellation)?;
    sync_file(staging)
}

struct VerifiedBackup {
    root: PathBuf,
    manifest: GraphBackupManifest,
    receipt: GraphBackupReceipt,
}

fn load_verified(backup_root: &Path) -> Result<VerifiedBackup, GraphDbError> {
    let root = backup_root
        .canonicalize()
        .map_err(|error| unavailable_io("canonicalize graph backup", backup_root, error))?;
    let metadata = fs::symlink_metadata(&root)
        .map_err(|error| unavailable_io("inspect graph backup", &root, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(GraphDbError::Corrupt {
            message: "graph backup root is not a regular directory".to_owned(),
        });
    }
    let manifest_path = root.join(MANIFEST_FILE);
    let bytes = fs::read(&manifest_path)
        .map_err(|error| unavailable_io("read graph backup manifest", &manifest_path, error))?;
    let manifest: GraphBackupManifest =
        serde_json::from_slice(&bytes).map_err(|error| GraphDbError::Corrupt {
            message: format!("invalid graph backup manifest: {error}"),
        })?;
    validate_manifest(&manifest)?;
    for artifact in &manifest.artifacts {
        verify_artifact(&root, artifact)?;
    }
    let actual = collect_artifacts(&root)?;
    if actual != manifest.artifacts {
        return Err(GraphDbError::Corrupt {
            message: "graph backup artifact inventory does not match its manifest".to_owned(),
        });
    }
    let receipt = receipt(&manifest, &bytes);
    Ok(VerifiedBackup {
        root,
        manifest,
        receipt,
    })
}

fn validate_manifest(manifest: &GraphBackupManifest) -> Result<(), GraphDbError> {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION
        || manifest.graph_format_version == 0
        || manifest.artifacts.is_empty()
    {
        return Err(GraphDbError::Corrupt {
            message: "invalid graph backup manifest identity".to_owned(),
        });
    }
    let mut previous: Option<&str> = None;
    for artifact in &manifest.artifacts {
        let path = Path::new(&artifact.logical_path);
        if artifact.byte_len == 0
            || artifact.sha256.len() != 64
            || path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            || previous.is_some_and(|value| value >= artifact.logical_path.as_str())
        {
            return Err(GraphDbError::Corrupt {
                message: "invalid graph backup artifact entry".to_owned(),
            });
        }
        previous = Some(&artifact.logical_path);
    }
    Ok(())
}

fn collect_artifacts(root: &Path) -> Result<Vec<GraphBackupArtifact>, GraphDbError> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    files
        .into_iter()
        .filter(|path| path.file_name().and_then(|name| name.to_str()) != Some(MANIFEST_FILE))
        .map(|path| {
            sync_file(&path)?;
            let relative = path
                .strip_prefix(root)
                .map_err(|_| GraphDbError::Corrupt {
                    message: "graph backup artifact escaped its root".to_owned(),
                })?
                .to_str()
                .ok_or_else(|| GraphDbError::Corrupt {
                    message: "graph backup artifact path is not UTF-8".to_owned(),
                })?
                .replace('\\', "/");
            let byte_len = fs::metadata(&path)
                .map_err(|error| unavailable_io("inspect graph backup artifact", &path, error))?
                .len();
            Ok(GraphBackupArtifact {
                logical_path: relative,
                byte_len,
                sha256: sha256_file(&path)?,
            })
        })
        .collect()
}

fn collect_files(root: &Path, path: &Path, files: &mut Vec<PathBuf>) -> Result<(), GraphDbError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| unavailable_io("inspect graph backup artifact", path, error))?;
    if metadata.file_type().is_symlink() {
        return Err(GraphDbError::Corrupt {
            message: format!("graph backup contains a symlink: {}", path.display()),
        });
    }
    if metadata.is_file() {
        files.push(path.to_path_buf());
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(GraphDbError::Corrupt {
            message: format!("graph backup contains a special file: {}", path.display()),
        });
    }
    let mut children = fs::read_dir(path)
        .map_err(|error| unavailable_io("read graph backup directory", path, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| unavailable_io("read graph backup entry", path, error))?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        let child_path = child.path();
        if child_path != root {
            collect_files(root, &child_path, files)?;
        }
    }
    Ok(())
}

fn verify_artifact(root: &Path, artifact: &GraphBackupArtifact) -> Result<(), GraphDbError> {
    let path = root.join(&artifact.logical_path);
    let metadata = fs::symlink_metadata(&path).map_err(|error| GraphDbError::Corrupt {
        message: format!(
            "missing graph backup artifact '{}': {error}",
            path.display()
        ),
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() != artifact.byte_len
        || sha256_file(&path)? != artifact.sha256
    {
        return Err(GraphDbError::Corrupt {
            message: format!(
                "graph backup artifact checksum mismatch: '{}'",
                path.display()
            ),
        });
    }
    Ok(())
}

fn receipt(manifest: &GraphBackupManifest, bytes: &[u8]) -> GraphBackupReceipt {
    GraphBackupReceipt {
        graph_format_version: manifest.graph_format_version,
        target_epoch: manifest.target_epoch,
        artifact_count: manifest.artifacts.len(),
        manifest_sha256: hex::encode(Sha256::digest(bytes)),
    }
}

fn validate_new_directory(destination: &Path) -> Result<(PathBuf, String), GraphDbError> {
    if destination
        .try_exists()
        .map_err(|error| unavailable_io("inspect graph backup destination", destination, error))?
    {
        return Err(GraphDbError::Conflict);
    }
    let parent = destination.parent().ok_or_else(|| {
        GraphDbError::invalid("graph backup destination must have a parent directory")
    })?;
    let parent = parent
        .canonicalize()
        .map_err(|error| unavailable_io("canonicalize graph backup parent", parent, error))?;
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| GraphDbError::invalid("graph backup destination has no UTF-8 filename"))?
        .to_owned();
    Ok((parent, file_name))
}

fn validate_destination(destination: &Path) -> Result<PathBuf, GraphDbError> {
    if destination.extension().and_then(|value| value.to_str()) != Some("grafeo") {
        return Err(GraphDbError::invalid(
            "restored graph destination must end in .grafeo",
        ));
    }
    let parent = destination.parent().ok_or_else(|| {
        GraphDbError::invalid("graph restore destination must have a parent directory")
    })?;
    let parent = parent.canonicalize().map_err(|error| {
        unavailable_io(
            "canonicalize graph restore destination parent",
            parent,
            error,
        )
    })?;
    let file_name = destination
        .file_name()
        .ok_or_else(|| GraphDbError::invalid("graph restore destination must have a filename"))?;
    let destination = parent.join(file_name);
    if destination
        .try_exists()
        .map_err(|error| unavailable_io("inspect graph restore destination", &destination, error))?
    {
        return Err(GraphDbError::Conflict);
    }
    Ok(destination)
}

fn staging_file(destination: &Path, kind: &str) -> Result<PathBuf, GraphDbError> {
    let parent = destination.parent().ok_or_else(|| {
        GraphDbError::invalid("graph restore destination must have a parent directory")
    })?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| GraphDbError::invalid("graph restore destination has no UTF-8 filename"))?;
    Ok(parent.join(format!(
        ".{name}.tracedecay-{kind}-{}.grafeo",
        NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed)
    )))
}

fn publish_file(staging: &Path, destination: &Path) -> Result<(), GraphDbError> {
    match fs::hard_link(staging, destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            remove_restore_staging(staging);
            return Err(GraphDbError::Conflict);
        }
        Err(error) => {
            remove_restore_staging(staging);
            return Err(unavailable_io(
                "publish restored graph database",
                destination,
                error,
            ));
        }
    }
    if let Err(error) = sync_file(destination) {
        return rollback_linked_publication(staging, destination, error);
    }
    if let Err(error) = fs::remove_file(staging) {
        return rollback_linked_publication(
            staging,
            destination,
            unavailable_io("remove graph restore staging file", staging, error),
        );
    }
    sync_parent(destination).map_err(|error| GraphDbError::DurabilityUncertain {
        message: format!(
            "restored graph '{}' was linked but its parent directory did not sync: {error}",
            destination.display()
        ),
    })
}

fn rollback_linked_publication(
    staging: &Path,
    destination: &Path,
    cause: GraphDbError,
) -> Result<(), GraphDbError> {
    match fs::remove_file(destination) {
        Ok(()) => {
            let _ = sync_parent(destination);
            remove_restore_staging(staging);
            Err(cause)
        }
        Err(rollback_error) => Err(GraphDbError::DurabilityUncertain {
            message: format!(
                "{cause}; rollback of restored graph '{}' failed: {rollback_error}",
                destination.display()
            ),
        }),
    }
}

fn remove_backup_staging(staging: &Path) {
    let _ = fs::remove_dir_all(staging);
}

fn remove_restore_staging(staging: &Path) {
    let _ = fs::remove_file(staging);
    let sidecar = PathBuf::from(format!("{}.wal", staging.display()));
    let _ = fs::remove_dir_all(sidecar);
}

fn create_private_directory(path: &Path) -> Result<(), GraphDbError> {
    fs::create_dir(path)
        .map_err(|error| unavailable_io("create graph backup staging directory", path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| unavailable_io("restrict graph backup directory", path, error))?;
    }
    Ok(())
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<(), GraphDbError> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| unavailable_io("create graph backup manifest", path, error))?;
    file.write_all(bytes)
        .map_err(|error| unavailable_io("write graph backup manifest", path, error))?;
    file.sync_all()
        .map_err(|error| unavailable_io("sync graph backup manifest", path, error))
}

fn sha256_file(path: &Path) -> Result<String, GraphDbError> {
    let mut file = File::open(path)
        .map_err(|error| unavailable_io("open graph backup artifact", path, error))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| unavailable_io("hash graph backup artifact", path, error))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn sync_file(path: &Path) -> Result<(), GraphDbError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| unavailable_io("sync graph backup artifact", path, error))
}

fn sync_parent(path: &Path) -> Result<(), GraphDbError> {
    let parent = path.parent().ok_or_else(|| {
        GraphDbError::invalid("durable graph artifact must have a parent directory")
    })?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<(), GraphDbError> {
    tracedecay_private_fs::framed_log::sync_directory(
        path,
        tracedecay_private_fs::framed_log::DirectorySyncPolicy::Strict,
    )
    .map_err(|error| unavailable_io("sync graph backup directory", path, error))
}

fn unavailable_io(operation: &str, path: &Path, error: std::io::Error) -> GraphDbError {
    GraphDbError::unavailable(format!("{operation} '{}': {error}", path.display()))
}
