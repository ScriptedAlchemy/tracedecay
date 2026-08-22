use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tracedecay_graph_db::{GraphBudgetKind, GraphDbError, SealedGraphStateDigest};
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, sync_directory};

static STAGED_SEAL_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(super) struct StagedReplaySeal {
    path: PathBuf,
    file: File,
    fingerprint: StagedFileFingerprint,
    existing: Option<ExistingReplaySeal>,
}

struct ExistingReplaySeal {
    file: File,
    fingerprint: StagedFileFingerprint,
}

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

pub(super) fn install_project_graph_replay_seal_at(
    generations_root: &Path,
    replay_root: &Path,
    sealed_state_digest: &SealedGraphStateDigest,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    let staged =
        stage_project_graph_replay_seal(generations_root, replay_root, sealed_state_digest, check)?;
    let _pool = lock_project_graph_replay_pool(replay_root, check)?;
    publish_staged_replay_seal(staged, replay_root, sealed_state_digest, check)
}

/// Pin the exact source inode, copy it into a private create-new staged inode,
/// and verify the copy without holding either broad filesystem lock.
pub(super) fn stage_project_graph_replay_seal(
    generations_root: &Path,
    replay_root: &Path,
    sealed_state_digest: &SealedGraphStateDigest,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<StagedReplaySeal, GraphDbError> {
    ensure_replay_root(replay_root)?;
    let digest = digest_hex(sealed_state_digest)?;
    let source = generations_root.join(format!("generation-{digest}.json"));
    let source_store_root = generations_root.parent().ok_or_else(|| {
        GraphDbError::invalid("code generation replay source has no store authority root")
    })?;
    let source_lock = lock_code_generation_store(source_store_root, check)?;
    let metadata = source.symlink_metadata().map_err(|error| {
        GraphDbError::unavailable(format!(
            "code generation replay source is unavailable: {error}"
        ))
    })?;
    if !metadata.file_type().is_file() {
        return Err(GraphDbError::Corrupt {
            message: "code generation replay source is not a regular file".to_owned(),
        });
    }
    let admitted_len = metadata.len();
    if admitted_len > tracedecay_code_index::production::MAX_SEALED_CODE_GENERATION_BYTES_V1 {
        return Err(GraphDbError::ResetRequired {
            message: "sealed code generation exceeds the canonical byte limit".to_owned(),
        });
    }
    let mut source_file = File::open(&source).map_err(|error| {
        GraphDbError::unavailable(format!(
            "code generation replay source could not be pinned: {error}"
        ))
    })?;
    let sequence = STAGED_SEAL_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let staged = replay_root.join(format!(
        ".generation-{digest}.stage-{}-{}-{sequence}",
        std::process::id(),
        crate::tracedecay::current_timestamp()
    ));
    let mut staged_file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&staged)
        .map_err(|error| {
            GraphDbError::unavailable(format!(
                "project graph replay staging file could not be created: {error}"
            ))
        })?;
    drop(source_lock);
    if let Err(error) = copy_and_verify_seal(
        &mut source_file,
        &mut staged_file,
        digest,
        admitted_len,
        check,
    ) {
        remove_staged(&staged, replay_root)?;
        return Err(error);
    }
    staged_file
        .sync_all()
        .map_err(|error| GraphDbError::DurabilityUncertain {
            message: format!("staged project graph replay seal sync failed: {error}"),
        })?;
    let mut permissions = staged_file
        .metadata()
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?
        .permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&staged, permissions)
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    let fingerprint = staged_fingerprint(
        &staged_file
            .metadata()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?,
    )?;
    if !staged_identity_matches(&staged, &staged_file, &fingerprint)? {
        remove_staged(&staged, replay_root)?;
        return Err(GraphDbError::Conflict);
    }
    let destination = replay_root.join(format!("generation-{digest}.json"));
    let existing = match pin_existing_replay_seal(&destination, digest, check) {
        Ok(existing) => existing,
        Err(error) => {
            drop(staged_file);
            remove_staged(&staged, replay_root)?;
            return Err(error);
        }
    };
    Ok(StagedReplaySeal {
        path: staged,
        file: staged_file,
        fingerprint,
        existing,
    })
}

/// Publish the exact private inode verified by staging while the caller holds
/// the replay-pool lock. The content-addressed destination is installed with
/// no-clobber semantics; an existing exact seal is an idempotent replay and a
/// foreign destination is preserved and rejected.
pub(super) fn publish_staged_replay_seal(
    staged: StagedReplaySeal,
    replay_root: &Path,
    sealed_state_digest: &SealedGraphStateDigest,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    publish_staged_replay_seal_with_before_install(
        staged,
        replay_root,
        sealed_state_digest,
        check,
        &|| Ok(()),
    )
}

fn publish_staged_replay_seal_with_before_install(
    staged: StagedReplaySeal,
    replay_root: &Path,
    sealed_state_digest: &SealedGraphStateDigest,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    before_install: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    check()?;
    let digest = digest_hex(sealed_state_digest)?;
    let destination = replay_root.join(format!("generation-{digest}.json"));
    let staged_metadata = staged
        .path
        .symlink_metadata()
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    if !staged_metadata.file_type().is_file() {
        return Err(GraphDbError::Corrupt {
            message: "staged project graph replay seal is not a regular file".to_owned(),
        });
    }
    if !staged_identity_matches(&staged.path, &staged.file, &staged.fingerprint)? {
        return Err(GraphDbError::Conflict);
    }
    before_install()?;
    check()?;
    if !staged_identity_matches(&staged.path, &staged.file, &staged.fingerprint)? {
        return Err(GraphDbError::Conflict);
    }
    let StagedReplaySeal {
        path,
        file,
        fingerprint: _,
        existing,
    } = staged;
    match std::fs::hard_link(&path, &destination) {
        Ok(()) => {
            check()?;
            sync_replay_root(replay_root)?;
            let installed_fingerprint = staged_fingerprint(
                &file
                    .metadata()
                    .map_err(|error| GraphDbError::unavailable(error.to_string()))?,
            )?;
            if !staged_identity_matches(&destination, &file, &installed_fingerprint)?
                || !staged_identity_matches(&path, &file, &installed_fingerprint)?
            {
                return Err(GraphDbError::Conflict);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            check()?;
            // Staging pins the destination before the replay-pool lock is
            // held, so a same-digest peer can install it in between. Re-pin
            // under the held lock: a digest-exact destination is an
            // idempotent replay, and only a foreign destination conflicts.
            let existing = match existing {
                Some(existing) => existing,
                None => match pin_existing_replay_seal(&destination, digest, check) {
                    Ok(Some(existing)) => existing,
                    Ok(None) => {
                        drop(file);
                        remove_staged(&path, replay_root)?;
                        return Err(GraphDbError::Conflict);
                    }
                    Err(error) => {
                        drop(file);
                        remove_staged(&path, replay_root)?;
                        return Err(error);
                    }
                },
            };
            if !staged_identity_matches(&destination, &existing.file, &existing.fingerprint)? {
                drop(existing);
                drop(file);
                remove_staged(&path, replay_root)?;
                return Err(GraphDbError::Conflict);
            }
        }
        Err(error) => return Err(GraphDbError::unavailable(error.to_string())),
    }
    check()?;
    if !staged_identity_matches(
        &path,
        &file,
        &staged_fingerprint(
            &file
                .metadata()
                .map_err(|error| GraphDbError::unavailable(error.to_string()))?,
        )?,
    )? {
        return Err(GraphDbError::Conflict);
    }
    drop(file);
    check()?;
    std::fs::remove_file(&path).map_err(|error| GraphDbError::DurabilityUncertain {
        message: format!("published replay staging unlink failed: {error}"),
    })?;
    sync_replay_root(replay_root)?;
    Ok(())
}

fn pin_existing_replay_seal(
    destination: &Path,
    expected_digest: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Option<ExistingReplaySeal>, GraphDbError> {
    let mut file = match File::open(destination) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(GraphDbError::unavailable(error.to_string())),
    };
    let metadata = file
        .metadata()
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    if !metadata.file_type().is_file() {
        return Err(GraphDbError::Corrupt {
            message: "existing project graph replay seal is not a regular file".to_owned(),
        });
    }
    let fingerprint = staged_fingerprint(&metadata)?;
    if !staged_identity_matches(destination, &file, &fingerprint)? {
        return Err(GraphDbError::Conflict);
    }
    // The stage/publish journey installs with no-clobber semantics: an exact
    // seal is an idempotent replay, while a foreign destination is preserved
    // and rejected as a conflict rather than reported as pool corruption.
    match verify_seal_file_digest(&mut file, expected_digest, check)? {
        SealDigestOutcome::Verified => {}
        SealDigestOutcome::Mismatch => return Err(GraphDbError::Conflict),
    }
    if !staged_identity_matches(destination, &file, &fingerprint)? {
        return Err(GraphDbError::Conflict);
    }
    Ok(Some(ExistingReplaySeal { file, fingerprint }))
}

fn copy_and_verify_seal(
    source: &mut File,
    staged: &mut File,
    expected: &str,
    admitted_len: u64,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut copied = 0_u64;
    loop {
        check()?;
        let read = source
            .read(&mut buffer)
            .map_err(|error| GraphDbError::Corrupt {
                message: format!("project graph replay source read failed: {error}"),
            })?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(u64::try_from(read).map_err(|_| {
                GraphDbError::budget_exhausted(
                    GraphBudgetKind::Write,
                    tracedecay_code_index::production::MAX_SEALED_CODE_GENERATION_BYTES_V1,
                )
            })?)
            .ok_or_else(|| {
                GraphDbError::budget_exhausted(
                    GraphBudgetKind::Write,
                    tracedecay_code_index::production::MAX_SEALED_CODE_GENERATION_BYTES_V1,
                )
            })?;
        if copied > admitted_len
            || copied > tracedecay_code_index::production::MAX_SEALED_CODE_GENERATION_BYTES_V1
        {
            return Err(GraphDbError::ResetRequired {
                message: "sealed code generation grew beyond its admitted byte length".to_owned(),
            });
        }
        staged.write_all(&buffer[..read]).map_err(|error| {
            GraphDbError::unavailable(format!(
                "project graph replay staging write failed: {error}"
            ))
        })?;
        digest.update(&buffer[..read]);
    }
    check()?;
    if copied != admitted_len {
        return Err(GraphDbError::Conflict);
    }
    if hex::encode(digest.finalize()) != expected {
        return Err(GraphDbError::Corrupt {
            message: "project graph replay seal digest does not match its filename".to_owned(),
        });
    }
    Ok(())
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
        let path_identity = tracedecay_runtime_core::windows_file::information(&path_file)
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let handle_identity = tracedecay_runtime_core::windows_file::information(file)
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
                return Err(GraphDbError::Conflict);
            }
            let sequence = STAGED_SEAL_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let staged = replay_root.join(format!(
                ".generation-{digest}.unlink-{}-{}-{sequence}",
                std::process::id(),
                crate::tracedecay::current_timestamp()
            ));
            std::fs::rename(&path, &staged)
                .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
            sync_replay_root(replay_root)?;
            if !staged_identity_matches(&staged, &file, &fingerprint)? {
                return Err(GraphDbError::Conflict);
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
        return Err(GraphDbError::Conflict);
    }
    let StagedReplayUnlink { path, file, .. } = staged;
    drop(file);
    std::fs::remove_file(path).map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    sync_replay_root(replay_root)?;
    verified
}

pub(super) fn lock_project_graph_replay_pool(
    replay_root: &Path,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<crate::retention::code_index_generations::CodeGenerationStoreLockV1, GraphDbError> {
    ensure_replay_root(replay_root)?;
    lock_code_generation_store(replay_root, check)
}

fn lock_code_generation_store(
    root: &Path,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<crate::retention::code_index_generations::CodeGenerationStoreLockV1, GraphDbError> {
    loop {
        check()?;
        match crate::retention::code_index_generations::try_acquire_code_generation_store_lock(root)
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?
        {
            Some(lock) => return Ok(lock),
            None => std::thread::park_timeout(Duration::from_millis(5)),
        }
    }
}

fn ensure_replay_root(replay_root: &Path) -> Result<(), GraphDbError> {
    let validate_existing = || {
        tracedecay_private_fs::validate_private_directory(replay_root).map_err(|error| {
            let message =
                format!("project graph replay pool is not an owner-private directory: {error}");
            match error.kind() {
                std::io::ErrorKind::InvalidInput
                | std::io::ErrorKind::NotADirectory
                | std::io::ErrorKind::PermissionDenied => GraphDbError::Corrupt { message },
                _ => GraphDbError::unavailable(message),
            }
        })
    };
    match replay_root.symlink_metadata() {
        Ok(_) => validate_existing(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match tracedecay_private_fs::create_private_directory(replay_root) {
                Ok(()) => Ok(()),
                Err(create_error) => match replay_root.symlink_metadata() {
                    Ok(_) => validate_existing(),
                    Err(_) => Err(GraphDbError::unavailable(create_error.to_string())),
                },
            }
        }
        Err(error) => Err(GraphDbError::unavailable(error.to_string())),
    }
}

fn digest_hex(sealed: &SealedGraphStateDigest) -> Result<&str, GraphDbError> {
    sealed
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| GraphDbError::invalid("code generation replay digest is not sha256"))
}

fn remove_staged(staged: &Path, replay_root: &Path) -> Result<(), GraphDbError> {
    match std::fs::remove_file(staged) {
        Ok(()) => sync_replay_root(replay_root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GraphDbError::DurabilityUncertain {
            message: format!("failed to remove invalid staged replay seal: {error}"),
        }),
    }
}

fn sync_replay_root(replay_root: &Path) -> Result<(), GraphDbError> {
    sync_directory(replay_root, DirectorySyncPolicy::Strict)
        .map_err(|error| GraphDbError::unavailable(error.to_string()))
}

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

/// Whether a seal's bytes hash to the digest its filename claims. A mismatch
/// is a fact about the file, not an error: read paths report it as pool
/// corruption while publish treats the file as a preserved foreign occupant.
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
    if hex::encode(digest.finalize()) != expected {
        return Ok(SealDigestOutcome::Mismatch);
    }
    Ok(SealDigestOutcome::Verified)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use tracedecay_graph_db::GraphDbError;

    use super::{
        ensure_replay_root, install_project_graph_replay_seal_at, lock_project_graph_replay_pool,
        publish_staged_replay_seal, publish_staged_replay_seal_with_before_install,
        stage_project_graph_replay_seal, verify_seal_digest,
    };

    #[cfg(unix)]
    #[test]
    fn replay_root_is_created_owner_private() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let replay_root = temp.path().join("project-replay");

        ensure_replay_root(&replay_root).unwrap();

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
            ensure_replay_root(&replay_root),
            Err(GraphDbError::Corrupt { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn replay_root_rejects_existing_symlink() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let destination = temp.path().join("private-directory");
        tracedecay_private_fs::create_private_directory(&destination).unwrap();
        let replay_root = temp.path().join("project-replay");
        symlink(destination, &replay_root).unwrap();

        assert!(matches!(
            ensure_replay_root(&replay_root),
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
            ensure_replay_root(&replay_root),
            Err(GraphDbError::Corrupt { .. })
        ));
        assert_eq!(
            replay_root.symlink_metadata().unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn project_replay_pool_serializes_same_digest_from_distinct_sources() {
        let temp = TempDir::new().unwrap();
        let bytes = b"same sealed generation";
        let digest_hex = hex::encode(Sha256::digest(bytes));
        let digest =
            tracedecay_graph_db::SealedGraphStateDigest::try_from(format!("sha256:{digest_hex}"))
                .unwrap();
        let replay_root = temp.path().join("project-replay");
        let mut generation_roots = Vec::new();
        for source in ["source-a", "source-b"] {
            let root = temp.path().join(source).join("generations");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join(format!("generation-{digest_hex}.json")), bytes).unwrap();
            generation_roots.push(root);
        }
        let barrier = Arc::new(Barrier::new(generation_roots.len()));
        let threads = generation_roots
            .into_iter()
            .map(|generation_root| {
                let barrier = Arc::clone(&barrier);
                let replay_root = replay_root.clone();
                let digest = digest.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    install_project_graph_replay_seal_at(
                        &generation_root,
                        &replay_root,
                        &digest,
                        &|| Ok(()),
                    )
                })
            })
            .collect::<Vec<_>>();

        for thread in threads {
            thread.join().unwrap().unwrap();
        }
        assert_eq!(
            std::fs::read(replay_root.join(format!("generation-{digest_hex}.json"))).unwrap(),
            bytes
        );
    }

    #[test]
    fn replay_seal_publish_accepts_exact_existing_destination() {
        let temp = TempDir::new().unwrap();
        let bytes = b"same sealed generation";
        let digest_hex = hex::encode(Sha256::digest(bytes));
        let digest =
            tracedecay_graph_db::SealedGraphStateDigest::try_from(format!("sha256:{digest_hex}"))
                .unwrap();
        let generation_root = temp.path().join("source").join("generations");
        let replay_root = temp.path().join("project-replay");
        std::fs::create_dir_all(&generation_root).unwrap();
        tracedecay_private_fs::create_private_directory(&replay_root).unwrap();
        std::fs::write(
            generation_root.join(format!("generation-{digest_hex}.json")),
            bytes,
        )
        .unwrap();
        let destination = replay_root.join(format!("generation-{digest_hex}.json"));
        std::fs::write(&destination, bytes).unwrap();
        let staged =
            stage_project_graph_replay_seal(&generation_root, &replay_root, &digest, &|| Ok(()))
                .unwrap();
        let staged_path = staged.path.clone();
        let _pool = lock_project_graph_replay_pool(&replay_root, &|| Ok(())).unwrap();

        publish_staged_replay_seal(staged, &replay_root, &digest, &|| Ok(())).unwrap();

        assert_eq!(std::fs::read(destination).unwrap(), bytes);
        assert!(!staged_path.exists());
    }

    #[test]
    fn replay_seal_publish_preserves_foreign_existing_destination() {
        let temp = TempDir::new().unwrap();
        let bytes = b"sealed generation";
        let foreign = b"foreign destination";
        let digest_hex = hex::encode(Sha256::digest(bytes));
        let digest =
            tracedecay_graph_db::SealedGraphStateDigest::try_from(format!("sha256:{digest_hex}"))
                .unwrap();
        let generation_root = temp.path().join("source").join("generations");
        let replay_root = temp.path().join("project-replay");
        std::fs::create_dir_all(&generation_root).unwrap();
        tracedecay_private_fs::create_private_directory(&replay_root).unwrap();
        std::fs::write(
            generation_root.join(format!("generation-{digest_hex}.json")),
            bytes,
        )
        .unwrap();
        let destination = replay_root.join(format!("generation-{digest_hex}.json"));
        let staged =
            stage_project_graph_replay_seal(&generation_root, &replay_root, &digest, &|| Ok(()))
                .unwrap();
        std::fs::write(&destination, foreign).unwrap();
        let _pool = lock_project_graph_replay_pool(&replay_root, &|| Ok(())).unwrap();

        assert_eq!(
            publish_staged_replay_seal(staged, &replay_root, &digest, &|| Ok(())),
            Err(GraphDbError::Conflict)
        );
        assert_eq!(std::fs::read(destination).unwrap(), foreign);
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
    fn replay_seal_publish_rejects_staged_path_replacement() {
        let temp = TempDir::new().unwrap();
        let bytes = b"sealed generation";
        let digest_hex = hex::encode(Sha256::digest(bytes));
        let digest =
            tracedecay_graph_db::SealedGraphStateDigest::try_from(format!("sha256:{digest_hex}"))
                .unwrap();
        let generation_root = temp.path().join("source").join("generations");
        let replay_root = temp.path().join("project-replay");
        std::fs::create_dir_all(&generation_root).unwrap();
        std::fs::write(
            generation_root.join(format!("generation-{digest_hex}.json")),
            bytes,
        )
        .unwrap();
        let staged =
            stage_project_graph_replay_seal(&generation_root, &replay_root, &digest, &|| Ok(()))
                .unwrap();
        let replacement = staged.path.with_extension("replacement");
        std::fs::rename(&staged.path, &replacement).unwrap();
        std::fs::write(&staged.path, bytes).unwrap();
        let _pool = lock_project_graph_replay_pool(&replay_root, &|| Ok(())).unwrap();

        assert_eq!(
            publish_staged_replay_seal(staged, &replay_root, &digest, &|| Ok(())),
            Err(GraphDbError::Conflict)
        );
        assert!(
            !replay_root
                .join(format!("generation-{digest_hex}.json"))
                .exists()
        );
    }

    #[cfg(unix)]
    #[test]
    fn replay_seal_publish_rejects_same_inode_same_length_rewrite() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let bytes = b"sealed generation";
        let digest_hex = hex::encode(Sha256::digest(bytes));
        let digest =
            tracedecay_graph_db::SealedGraphStateDigest::try_from(format!("sha256:{digest_hex}"))
                .unwrap();
        let generation_root = temp.path().join("source").join("generations");
        let replay_root = temp.path().join("project-replay");
        std::fs::create_dir_all(&generation_root).unwrap();
        std::fs::write(
            generation_root.join(format!("generation-{digest_hex}.json")),
            bytes,
        )
        .unwrap();
        let staged =
            stage_project_graph_replay_seal(&generation_root, &replay_root, &digest, &|| Ok(()))
                .unwrap();
        let before = staged.path.symlink_metadata().unwrap();
        let mut permissions = before.permissions();
        permissions.set_mode(permissions.mode() | 0o200);
        std::fs::set_permissions(&staged.path, permissions).unwrap();
        std::fs::write(&staged.path, b"rewritten bytes!!").unwrap();
        assert_eq!(staged.path.symlink_metadata().unwrap().len(), before.len());
        let _pool = lock_project_graph_replay_pool(&replay_root, &|| Ok(())).unwrap();

        assert_eq!(
            publish_staged_replay_seal(staged, &replay_root, &digest, &|| Ok(())),
            Err(GraphDbError::Conflict)
        );
    }

    #[test]
    fn replay_seal_publish_rejects_path_swap_between_check_and_rename() {
        let temp = TempDir::new().unwrap();
        let bytes = b"sealed generation";
        let digest_hex = hex::encode(Sha256::digest(bytes));
        let digest =
            tracedecay_graph_db::SealedGraphStateDigest::try_from(format!("sha256:{digest_hex}"))
                .unwrap();
        let generation_root = temp.path().join("source").join("generations");
        let replay_root = temp.path().join("project-replay");
        std::fs::create_dir_all(&generation_root).unwrap();
        std::fs::write(
            generation_root.join(format!("generation-{digest_hex}.json")),
            bytes,
        )
        .unwrap();
        let staged =
            stage_project_graph_replay_seal(&generation_root, &replay_root, &digest, &|| Ok(()))
                .unwrap();
        let staged_path = staged.path.clone();
        let retained = staged_path.with_extension("retained");
        let _pool = lock_project_graph_replay_pool(&replay_root, &|| Ok(())).unwrap();

        assert_eq!(
            publish_staged_replay_seal_with_before_install(
                staged,
                &replay_root,
                &digest,
                &|| Ok(()),
                &|| {
                    std::fs::rename(&staged_path, &retained)
                        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
                    std::fs::write(&staged_path, bytes)
                        .map_err(|error| GraphDbError::unavailable(error.to_string()))
                },
            ),
            Err(GraphDbError::Conflict)
        );
    }
}
