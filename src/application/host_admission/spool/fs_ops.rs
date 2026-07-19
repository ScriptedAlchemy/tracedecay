use std::fs::{self, File, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::types::SpoolError;

pub(crate) fn io_error(_error: impl ToString) -> SpoolError {
    SpoolError::Io
}

pub(crate) fn file_len(path: &Path) -> Result<u64, SpoolError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(io_error(error)),
    }
}

fn temporary_path(path: &Path, kind: &str) -> PathBuf {
    static NONCE: AtomicU64 = AtomicU64::new(1);
    let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(
        ".{}.{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("spool"),
        kind,
        std::process::id(),
        nonce
    ))
}

fn remove_owned_temp(path: &Path) {
    let _ = fs::remove_file(path);
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), SpoolError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(io_error)
}

#[cfg(not(unix))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the cross-platform permission hook shares a fallible call contract"
)]
fn set_private_file_permissions(_path: &Path) -> Result<(), SpoolError> {
    // This matches the repository's current private-store convention on
    // non-Unix hosts; no ad-hoc ACL implementation is introduced here.
    Ok(())
}

pub(crate) fn tighten_existing_file(path: &Path) -> Result<(), SpoolError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    if !metadata.file_type().is_file() {
        return Err(SpoolError::Io);
    }
    set_private_file_permissions(path)
}

pub(crate) fn sync_parent_directory(path: &Path) -> Result<(), SpoolError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    super::super::sync_directory(
        parent,
        super::super::DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(io_error)
}

fn replace_file_atomically(
    temporary: &Path,
    destination: &Path,
    label: &str,
) -> Result<(), SpoolError> {
    crate::db::DatabaseAuthority::replace_file_atomically(temporary, destination, label)
        .map_err(|_| SpoolError::Io)
}

fn create_owned_temp(destination: &Path, kind: &str) -> Result<(PathBuf, File), SpoolError> {
    for _ in 0..64 {
        let path = temporary_path(destination, kind);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    Err(SpoolError::Io)
}

pub(crate) fn truncate_file(path: &Path, len: u64) -> Result<(), SpoolError> {
    tighten_existing_file(path)?;
    let output = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(io_error)?;
    output.set_len(len).map_err(io_error)?;
    output.sync_all().map_err(io_error)?;
    tighten_existing_file(path)?;
    sync_parent_directory(path)
}

pub(crate) fn with_owned_temp_publish<T>(
    destination: &Path,
    kind: &str,
    label: &str,
    write: impl FnOnce(&mut File) -> Result<T, SpoolError>,
) -> Result<T, SpoolError> {
    let (temporary, mut output) = create_owned_temp(destination, kind)?;
    let publish = (|| {
        let value = write(&mut output)?;
        output.sync_all().map_err(io_error)?;
        drop(output);
        replace_file_atomically(&temporary, destination, label)?;
        tighten_existing_file(destination)?;
        sync_parent_directory(destination)?;
        Ok(value)
    })();
    if publish.is_err() {
        remove_owned_temp(&temporary);
    }
    publish
}
