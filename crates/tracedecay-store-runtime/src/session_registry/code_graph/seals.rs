use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_graph_db::{GraphDbError, SealedGraphStateDigest};
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, sync_directory};

static STAGED_SEAL_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(super) struct StagedReplayUnlink {
    path: PathBuf,
    file: File,
    fingerprint: StagedFileFingerprint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StagedFileFingerprint {
    len: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    #[cfg(unix)]
    mode: u32,
}

pub(super) fn sealed_digest_from_generation_file(
    generation_file: &str,
) -> Result<SealedGraphStateDigest, GraphDbError> {
    let digest = generation_file
        .strip_prefix("generation-")
        .and_then(|value| value.strip_suffix(".json"))
        .ok_or_else(|| GraphDbError::invalid("sealed generation filename is invalid"))?;
    SealedGraphStateDigest::try_from(format!("sha256:{digest}"))
}

fn staged_fingerprint(metadata: &std::fs::Metadata) -> Result<StagedFileFingerprint, GraphDbError> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    Ok(StagedFileFingerprint {
        len: metadata.len(),
        modified: metadata.modified().map_err(|error| {
            GraphDbError::unavailable(format!(
                "staged project graph replay modification time is unavailable: {error}"
            ))
        })?,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
        #[cfg(unix)]
        changed_seconds: metadata.ctime(),
        #[cfg(unix)]
        changed_nanoseconds: metadata.ctime_nsec(),
        #[cfg(unix)]
        mode: metadata.mode(),
    })
}

fn staged_identity_matches(
    path: &Path,
    file: &File,
    expected: &StagedFileFingerprint,
) -> Result<bool, GraphDbError> {
    let path_metadata = path
        .symlink_metadata()
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    let handle_metadata = file
        .metadata()
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    if staged_fingerprint(&path_metadata)? != *expected
        || staged_fingerprint(&handle_metadata)? != *expected
    {
        return Ok(false);
    }
    #[cfg(windows)]
    {
        let path_file =
            File::open(path).map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let path_identity = tracedecay_private_fs::windows_file::information(&path_file)
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let handle_identity = tracedecay_private_fs::windows_file::information(file)
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        return Ok(
            path_identity.volume_serial_number == handle_identity.volume_serial_number
                && path_identity.file_index == handle_identity.file_index,
        );
    }
    #[cfg(unix)]
    {
        Ok(true)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(GraphDbError::Unavailable {
            message: "stable staged replay file identity is unsupported on this platform"
                .to_owned(),
        })
    }
}

/// Move a replay seal out of the content-addressed namespace while the caller
/// holds the replay-pool lock. Digest verification happens after releasing it.
#[hotpath::measure(label = "daemon.session_registry.replay_unlink_stage")]
pub(super) fn stage_project_graph_replay_unlink(
    replay_root: &Path,
    sealed_digest: &SealedGraphStateDigest,
) -> Result<Option<StagedReplayUnlink>, GraphDbError> {
    let digest = digest_hex(sealed_digest)?;
    let path = replay_root.join(format!("generation-{digest}.json"));
    match path.symlink_metadata() {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                return Err(GraphDbError::Corrupt {
                    message: "project graph replay seal is not a regular file".to_owned(),
                });
            }
            let file =
                File::open(&path).map_err(|error| GraphDbError::unavailable(error.to_string()))?;
            let fingerprint = staged_fingerprint(
                &file
                    .metadata()
                    .map_err(|error| GraphDbError::unavailable(error.to_string()))?,
            )?;
            if !staged_identity_matches(&path, &file, &fingerprint)? {
                return Err(GraphDbError::conflict(
                    "seals.stage_project_graph_replay_unlink",
                ));
            }
            let sequence = STAGED_SEAL_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let staged = replay_root.join(format!(
                ".generation-{digest}.unlink-{}-{}-{sequence}",
                std::process::id(),
                tracedecay_runtime_core::tracedecay::current_timestamp()
            ));
            std::fs::rename(&path, &staged)
                .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
            sync_replay_root(replay_root)?;
            // Renaming can legitimately advance ctime. Establish the stable
            // post-rename fingerprint from the still-open inode, then prove
            // that the staged path resolves to that exact handle.
            let fingerprint = staged_fingerprint(
                &file
                    .metadata()
                    .map_err(|error| GraphDbError::unavailable(error.to_string()))?,
            )?;
            if !staged_identity_matches(&staged, &file, &fingerprint)? {
                return Err(GraphDbError::conflict(
                    "seals.stage_project_graph_replay_unlink",
                ));
            }
            Ok(Some(StagedReplayUnlink {
                path: staged,
                file,
                fingerprint,
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(GraphDbError::unavailable(error.to_string())),
    }
}

/// Verify outside the replay-pool lock, then reacquire it only to prove and
/// remove the exact staged inode. A concurrent install at the canonical path
/// is independent and remains untouched.
#[hotpath::measure(label = "daemon.session_registry.replay_unlink_finalize")]
pub(super) fn finalize_project_graph_replay_unlink(
    staged: StagedReplayUnlink,
    replay_root: &Path,
    sealed_digest: &SealedGraphStateDigest,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    let digest = digest_hex(sealed_digest)?;
    let verified = verify_seal_digest(&staged.path, digest, check);
    let _pool = lock_project_graph_replay_pool(replay_root, check)?;
    if !staged_identity_matches(&staged.path, &staged.file, &staged.fingerprint)? {
        return Err(GraphDbError::conflict(
            "seals.finalize_project_graph_replay_unlink",
        ));
    }
    let StagedReplayUnlink { path, file, .. } = staged;
    drop(file);
    std::fs::remove_file(path).map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    sync_replay_root(replay_root)?;
    verified
}

#[hotpath::measure(label = "daemon.session_registry.replay_pool_lock")]
pub(super) fn lock_project_graph_replay_pool(
    replay_root: &Path,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<
    tracedecay_code_index_retention::code_index_generations::CodeGenerationStoreLockV1,
    GraphDbError,
> {
    tracedecay_runtime_core::storage::PrivateStoreIo::create_private_directory(replay_root)
        .map_err(|error| {
            let message =
                format!("project graph replay pool is not an owner-private directory: {error}");
            match error.kind() {
                std::io::ErrorKind::InvalidInput
                | std::io::ErrorKind::NotADirectory
                | std::io::ErrorKind::PermissionDenied => GraphDbError::Corrupt { message },
                _ => GraphDbError::unavailable(message),
            }
        })?;
    lock_code_generation_store(replay_root, check)
}

fn lock_code_generation_store(
    root: &Path,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<
    tracedecay_code_index_retention::code_index_generations::CodeGenerationStoreLockV1,
    GraphDbError,
> {
    loop {
        check()?;
        match tracedecay_code_index_retention::code_index_generations::try_acquire_code_generation_store_lock(root)
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?
        {
            Some(lock) => return Ok(lock),
            None => std::thread::park_timeout(Duration::from_millis(5)),
        }
    }
}

fn digest_hex(sealed: &SealedGraphStateDigest) -> Result<&str, GraphDbError> {
    sealed
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| GraphDbError::invalid("code generation replay digest is not sha256"))
}

fn sync_replay_root(replay_root: &Path) -> Result<(), GraphDbError> {
    sync_directory(replay_root, DirectorySyncPolicy::Strict)
        .map_err(|error| GraphDbError::unavailable(error.to_string()))
}

#[hotpath::measure(label = "daemon.session_registry.verify_seal_digest")]
pub(super) fn verify_seal_digest(
    path: &Path,
    expected: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    let mut file =
        std::fs::File::open(path).map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    match verify_seal_file_digest(&mut file, expected, check)? {
        SealDigestOutcome::Verified => Ok(()),
        SealDigestOutcome::Mismatch => Err(GraphDbError::Corrupt {
            message: "project graph replay seal digest does not match its filename".to_owned(),
        }),
    }
}

/// Whether a retained seal's bytes hash to the digest its filename claims. A
/// mismatch is a fact about the file that read paths report as pool corruption.
enum SealDigestOutcome {
    Verified,
    Mismatch,
}

fn verify_seal_file_digest(
    file: &mut File,
    expected: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<SealDigestOutcome, GraphDbError> {
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        check()?;
        let read = file
            .read(&mut buffer)
            .map_err(|error| GraphDbError::Corrupt {
                message: format!("project graph replay seal read failed: {error}"),
            })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    check()?;
    if encode_lowercase_hex(&digest.finalize()) != expected {
        return Ok(SealDigestOutcome::Mismatch);
    }
    Ok(SealDigestOutcome::Verified)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use tracedecay_graph_db::GraphDbError;

    use super::{
        finalize_project_graph_replay_unlink, lock_project_graph_replay_pool,
        stage_project_graph_replay_unlink, verify_seal_digest,
    };

    #[cfg(unix)]
    #[test]
    fn replay_root_is_created_owner_private() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let replay_root = temp.path().join("project-replay");

        drop(lock_project_graph_replay_pool(&replay_root, &|| Ok(())).unwrap());

        assert_eq!(
            replay_root.symlink_metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn replay_root_rejects_existing_non_directory() {
        let temp = TempDir::new().unwrap();
        let replay_root = temp.path().join("project-replay");
        std::fs::write(&replay_root, b"not a directory").unwrap();

        assert!(matches!(
            lock_project_graph_replay_pool(&replay_root, &|| Ok(())),
            Err(GraphDbError::Corrupt { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn replay_root_rejects_existing_symlink() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let destination = temp.path().join("private-directory");
        tracedecay_runtime_core::storage::PrivateStoreIo::create_private_directory(&destination)
            .unwrap();
        let replay_root = temp.path().join("project-replay");
        symlink(destination, &replay_root).unwrap();

        assert!(matches!(
            lock_project_graph_replay_pool(&replay_root, &|| Ok(())),
            Err(GraphDbError::Corrupt { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn replay_root_rejects_existing_too_permissive_directory() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let replay_root = temp.path().join("project-replay");
        std::fs::create_dir(&replay_root).unwrap();
        std::fs::set_permissions(&replay_root, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(matches!(
            lock_project_graph_replay_pool(&replay_root, &|| Ok(())),
            Err(GraphDbError::Corrupt { .. })
        ));
        assert_eq!(
            replay_root.symlink_metadata().unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn replay_seal_digest_scan_observes_control() {
        let temp = TempDir::new().unwrap();
        let bytes = vec![b'x'; 3 * 64 * 1024];
        let path = temp.path().join("seal.json");
        std::fs::write(&path, &bytes).unwrap();
        let expected = hex::encode(Sha256::digest(&bytes));
        let checks = AtomicUsize::new(0);
        assert_eq!(
            verify_seal_digest(&path, &expected, &|| {
                if checks.fetch_add(1, Ordering::SeqCst) >= 2 {
                    Err(GraphDbError::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(GraphDbError::Cancelled)
        );
    }

    #[test]
    fn replay_seal_digest_scan_preserves_deadline_error() {
        let temp = TempDir::new().unwrap();
        let bytes = vec![b'x'; 3 * 64 * 1024];
        let path = temp.path().join("seal.json");
        std::fs::write(&path, &bytes).unwrap();
        let expected = hex::encode(Sha256::digest(&bytes));
        let checks = AtomicUsize::new(0);

        assert_eq!(
            verify_seal_digest(&path, &expected, &|| {
                if checks.fetch_add(1, Ordering::SeqCst) >= 2 {
                    Err(GraphDbError::DeadlineExceeded)
                } else {
                    Ok(())
                }
            }),
            Err(GraphDbError::DeadlineExceeded)
        );
    }

    #[test]
    fn staged_replay_unlink_preserves_concurrent_canonical_reinstall() {
        let temp = TempDir::new().unwrap();
        let replay_root = temp.path().join("project-replay");
        drop(lock_project_graph_replay_pool(&replay_root, &|| Ok(())).unwrap());
        let bytes = b"sealed generation";
        let digest_hex = hex::encode(Sha256::digest(bytes));
        let digest =
            tracedecay_graph_db::SealedGraphStateDigest::try_from(format!("sha256:{digest_hex}"))
                .unwrap();
        let canonical = replay_root.join(format!("generation-{digest_hex}.json"));
        std::fs::write(&canonical, bytes).unwrap();
        let staged = {
            let _pool = lock_project_graph_replay_pool(&replay_root, &|| Ok(())).unwrap();
            stage_project_graph_replay_unlink(&replay_root, &digest)
                .unwrap()
                .unwrap()
        };
        std::fs::write(&canonical, bytes).unwrap();

        finalize_project_graph_replay_unlink(staged, &replay_root, &digest, &|| Ok(())).unwrap();

        assert_eq!(std::fs::read(canonical).unwrap(), bytes);
    }

    #[test]
    fn staged_replay_unlink_replacement_conflicts_without_deleting_evidence() {
        let temp = TempDir::new().unwrap();
        let replay_root = temp.path().join("project-replay");
        drop(lock_project_graph_replay_pool(&replay_root, &|| Ok(())).unwrap());
        let bytes = b"sealed generation";
        let digest_hex = hex::encode(Sha256::digest(bytes));
        let digest =
            tracedecay_graph_db::SealedGraphStateDigest::try_from(format!("sha256:{digest_hex}"))
                .unwrap();
        let canonical = replay_root.join(format!("generation-{digest_hex}.json"));
        std::fs::write(&canonical, bytes).unwrap();
        let staged = {
            let _pool = lock_project_graph_replay_pool(&replay_root, &|| Ok(())).unwrap();
            stage_project_graph_replay_unlink(&replay_root, &digest)
                .unwrap()
                .unwrap()
        };
        let staged_path = staged.path.clone();
        let retained = staged_path.with_extension("retained-evidence");
        std::fs::rename(&staged_path, &retained).unwrap();
        std::fs::write(&staged_path, bytes).unwrap();

        assert!(matches!(
            finalize_project_graph_replay_unlink(staged, &replay_root, &digest, &|| Ok(())),
            Err(GraphDbError::Conflict { .. })
        ));
        assert_eq!(std::fs::read(staged_path).unwrap(), bytes);
        assert_eq!(std::fs::read(retained).unwrap(), bytes);
        assert!(!canonical.exists());
    }
}
