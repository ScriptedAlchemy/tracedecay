use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::{LcmError, validate_payload_ref};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PayloadFileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0o40_0000;

pub fn safe_remove_payload_file(dir: &Path, payload_ref: &str) -> Result<bool, LcmError> {
    safe_remove_payload_file_checked(dir, payload_ref, None)
}

pub(super) fn safe_remove_payload_file_checked(
    dir: &Path,
    payload_ref: &str,
    expected_identity: Option<&PayloadFileIdentity>,
) -> Result<bool, LcmError> {
    validate_payload_ref(payload_ref)?;
    let path = dir.join(payload_ref);
    ensure_contained(dir, &path)?;
    let Some((file, _opened, _lstat, identity)) = open_verified_payload_file(&path)? else {
        return Ok(false);
    };
    if let Some(expected_identity) = expected_identity {
        same_payload_file_identity(&identity, expected_identity)?;
    }
    drop(file);
    ensure_contained(dir, &path)?;
    fs::remove_file(&path).map_err(|err| LcmError::Io(err.to_string()))?;
    Ok(true)
}

pub(super) fn inspect_payload_file_for_delete(
    path: &Path,
) -> Result<(bool, Option<PayloadFileIdentity>, u64), LcmError> {
    Ok(match open_verified_payload_file(path)? {
        Some((_file, opened, _lstat, identity)) => (true, Some(identity), opened.len()),
        None => (false, None, 0),
    })
}

pub(super) fn read_payload_file_for_verify(
    path: &Path,
) -> Result<Option<(Vec<u8>, PayloadFileIdentity)>, LcmError> {
    let Some((mut file, _opened, _lstat, identity)) = open_verified_payload_file(path)? else {
        return Ok(None);
    };
    let mut content = Vec::new();
    file.read_to_end(&mut content)
        .map_err(|err| LcmError::Io(err.to_string()))?;
    Ok(Some((content, identity)))
}

pub(super) fn open_verified_payload_file(
    path: &Path,
) -> Result<Option<(fs::File, fs::Metadata, fs::Metadata, PayloadFileIdentity)>, LcmError> {
    let file = match private_file_options().read(true).open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            if fs::symlink_metadata(path)
                .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
            {
                return Err(LcmError::InvalidPayloadRef);
            }
            return Err(LcmError::Io(err.to_string()));
        }
    };
    let opened = file
        .metadata()
        .map_err(|err| LcmError::Io(err.to_string()))?;
    if !opened.is_file() {
        return Err(LcmError::InvalidPayloadRef);
    }
    let lstat = fs::symlink_metadata(path).map_err(|err| LcmError::Io(err.to_string()))?;
    if lstat.file_type().is_symlink() || !lstat.is_file() {
        return Err(LcmError::InvalidPayloadRef);
    }
    same_file_identity(&opened, &lstat)?;
    let identity = payload_file_identity(&opened);
    Ok(Some((file, opened, lstat, identity)))
}

#[cfg(unix)]
fn same_file_identity(opened: &fs::Metadata, lstat: &fs::Metadata) -> Result<(), LcmError> {
    use std::os::unix::fs::MetadataExt;

    if opened.dev() == lstat.dev() && opened.ino() == lstat.ino() {
        Ok(())
    } else {
        Err(LcmError::InvalidPayloadRef)
    }
}

#[cfg(unix)]
fn payload_file_identity(metadata: &fs::Metadata) -> PayloadFileIdentity {
    use std::os::unix::fs::MetadataExt;

    PayloadFileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

#[cfg(unix)]
pub(super) fn same_payload_file_identity(
    actual: &PayloadFileIdentity,
    expected: &PayloadFileIdentity,
) -> Result<(), LcmError> {
    if actual == expected {
        Ok(())
    } else {
        Err(LcmError::InvalidPayloadRef)
    }
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // Keep platform implementations signature-compatible.
fn same_file_identity(_opened: &fs::Metadata, _lstat: &fs::Metadata) -> Result<(), LcmError> {
    Ok(())
}

#[cfg(not(unix))]
fn payload_file_identity(_metadata: &fs::Metadata) -> PayloadFileIdentity {
    PayloadFileIdentity {}
}

#[cfg(not(unix))]
#[allow(clippy::trivially_copy_pass_by_ref, clippy::unnecessary_wraps)] // Keep the identity API uniform even where the platform identity is opaque.
pub(super) fn same_payload_file_identity(
    _actual: &PayloadFileIdentity,
    _expected: &PayloadFileIdentity,
) -> Result<(), LcmError> {
    Ok(())
}

pub(super) fn prepare_payload_dir(storage_root: &Path) -> Result<PathBuf, LcmError> {
    let root = super::canonical_storage_root(storage_root)?;
    let dir = root.join("lcm-payloads");
    match fs::symlink_metadata(&dir) {
        Ok(metadata) => ensure_actual_private_dir(&dir, &metadata)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&dir).map_err(|err| LcmError::Io(err.to_string()))?;
            set_private_dir_permissions(&dir)?;
        }
        Err(err) => return Err(LcmError::Io(err.to_string())),
    }
    ensure_payload_dir_under_root(&root, &dir)?;
    Ok(dir)
}

pub(crate) fn existing_payload_dir(storage_root: &Path) -> Result<PathBuf, LcmError> {
    existing_payload_dir_opt(storage_root)?.ok_or_else(|| {
        LcmError::Io(format!(
            "payload directory missing under {}",
            storage_root.display()
        ))
    })
}

/// Like `existing_payload_dir`, but a payload directory that was never
/// created (it is made lazily on first externalization) or has been removed
/// reports as `None` instead of an I/O error. Invalid configurations —
/// symlinked dir, wrong file type, dir escaping the storage root — still
/// error.
pub(crate) fn existing_payload_dir_opt(storage_root: &Path) -> Result<Option<PathBuf>, LcmError> {
    let root = super::canonical_storage_root(storage_root)?;
    let dir = root.join("lcm-payloads");
    let metadata = match fs::symlink_metadata(&dir) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(LcmError::Io(err.to_string())),
    };
    ensure_actual_private_dir(&dir, &metadata)?;
    ensure_payload_dir_under_root(&root, &dir)?;
    Ok(Some(dir))
}

pub(crate) fn canonical_storage_root(storage_root: &Path) -> Result<PathBuf, LcmError> {
    let metadata =
        fs::symlink_metadata(storage_root).map_err(|err| LcmError::Io(err.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LcmError::InvalidPayloadRef);
    }
    storage_root
        .canonicalize()
        .map_err(|err| LcmError::Io(err.to_string()))
}

fn ensure_actual_private_dir(dir: &Path, metadata: &fs::Metadata) -> Result<(), LcmError> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LcmError::InvalidPayloadRef);
    }
    set_private_dir_permissions(dir)?;
    Ok(())
}

fn ensure_payload_dir_under_root(root: &Path, dir: &Path) -> Result<(), LcmError> {
    let canonical_dir = dir
        .canonicalize()
        .map_err(|err| LcmError::Io(err.to_string()))?;
    if canonical_dir.parent() == Some(root) {
        Ok(())
    } else {
        Err(LcmError::InvalidPayloadRef)
    }
}

pub(crate) fn ensure_contained(root: &Path, path: &Path) -> Result<(), LcmError> {
    let parent = path.parent().ok_or(LcmError::InvalidPayloadRef)?;
    if parent == root {
        Ok(())
    } else {
        Err(LcmError::InvalidPayloadRef)
    }
}

pub(super) fn write_private_file(path: &Path, content: &[u8]) -> Result<bool, LcmError> {
    let mut file = match private_file_options()
        .create_new(true)
        .write(true)
        .open(path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure_existing_payload_matches(path, content)?;
            return Ok(false);
        }
        Err(err) => return Err(LcmError::Io(err.to_string())),
    };
    if let Err(error) = file.write_all(content).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(LcmError::Io(error.to_string()));
    }
    Ok(true)
}

fn ensure_existing_payload_matches(path: &Path, content: &[u8]) -> Result<(), LcmError> {
    let mut file = private_file_options()
        .read(true)
        .open(path)
        .map_err(|err| LcmError::Io(err.to_string()))?;
    let mut existing = Vec::new();
    file.read_to_end(&mut existing)
        .map_err(|err| LcmError::Io(err.to_string()))?;
    if existing == content {
        Ok(())
    } else {
        Err(LcmError::PayloadIntegrityMismatch)
    }
}

pub(super) fn read_payload_file(path: &Path) -> Result<String, LcmError> {
    let mut file = private_file_options()
        .read(true)
        .open(path)
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                LcmError::PayloadMissing
            } else {
                LcmError::Io(err.to_string())
            }
        })?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|err| LcmError::Io(err.to_string()))?;
    Ok(content)
}

#[cfg(unix)]
fn private_file_options() -> fs::OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = fs::OpenOptions::new();
    options.mode(0o600);
    #[cfg(target_os = "linux")]
    options.custom_flags(O_NOFOLLOW);
    options
}

#[cfg(not(unix))]
fn private_file_options() -> fs::OpenOptions {
    fs::OpenOptions::new()
}

fn set_private_dir_permissions(path: &Path) -> Result<(), LcmError> {
    crate::storage::set_private_dir_permissions(path).map_err(|err| LcmError::Io(err.to_string()))
}
