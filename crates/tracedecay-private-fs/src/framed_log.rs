//! Crash-safe filesystem primitives for hook and host-admission framed logs.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// How a directory fsync failure is surfaced to the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectorySyncPolicy {
    /// Surface every fsync failure.
    Strict,
    /// Surface genuine IO failures but tolerate unsupported directory fsync.
    TolerateUnsupported,
}

/// Flush a directory's metadata so a preceding create/rename/remove is durable.
pub fn sync_directory(dir: &Path, policy: DirectorySyncPolicy) -> io::Result<()> {
    #[cfg(unix)]
    {
        match File::open(dir).and_then(|directory| directory.sync_all()) {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(policy, DirectorySyncPolicy::TolerateUnsupported)
                    && error.kind() == io::ErrorKind::InvalidInput =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (dir, policy);
        Ok(())
    }
}

/// Flush the parent directory of `path`, if any.
pub fn sync_parent_directory(path: &Path, policy: DirectorySyncPolicy) -> io::Result<()> {
    match path.parent() {
        Some(parent) => sync_directory(parent, policy),
        None => Ok(()),
    }
}

pub fn file_len(path: &Path) -> io::Result<u64> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

pub fn validate_regular_or_missing(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub fn tighten_existing_file(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a regular file",
        ));
    }
    set_private_file_permissions(path)
}

pub fn read_bounded(path: &Path, maximum: usize) -> io::Result<Option<Vec<u8>>> {
    if !validate_regular_or_missing(path)? {
        return Ok(None);
    }
    let length = fs::metadata(path)?.len();
    if length == 0 || length > maximum as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bounded read length is invalid",
        ));
    }
    let mut bytes = Vec::with_capacity(length as usize);
    File::open(path)?
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() != length as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bounded read length mismatch",
        ));
    }
    Ok(Some(bytes))
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

fn create_owned_temp(destination: &Path, kind: &str) -> io::Result<(PathBuf, File)> {
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
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate temporary publish file",
    ))
}

/// Publish `destination` by staging into an owned temp file, syncing, then
/// replacing through `publish`.
pub fn with_owned_temp_publish<T>(
    destination: &Path,
    kind: &str,
    publish: impl FnOnce(&Path, &Path) -> io::Result<()>,
    write: impl FnOnce(&mut File) -> io::Result<T>,
    directory_policy: DirectorySyncPolicy,
) -> io::Result<T> {
    validate_regular_or_missing(destination)?;
    let (temporary, mut output) = create_owned_temp(destination, kind)?;
    let result = (|| {
        let value = write(&mut output)?;
        output.sync_all()?;
        drop(output);
        publish(&temporary, destination)?;
        tighten_existing_file(destination)?;
        sync_parent_directory(destination, directory_policy)?;
        Ok(value)
    })();
    if result.is_err() {
        remove_owned_temp(&temporary);
    }
    result
}

pub fn replace_via_rename(temporary: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temporary, destination)
}

pub fn atomic_write(
    destination: &Path,
    kind: &str,
    bytes: &[u8],
    directory_policy: DirectorySyncPolicy,
) -> io::Result<()> {
    with_owned_temp_publish(
        destination,
        kind,
        replace_via_rename,
        |output| output.write_all(bytes),
        directory_policy,
    )
}

pub fn atomic_write_prepared(
    destination: &Path,
    kind: &str,
    bytes: &[u8],
    prepare: impl FnOnce(&Path) -> io::Result<()>,
    directory_policy: DirectorySyncPolicy,
) -> io::Result<()> {
    validate_regular_or_missing(destination)?;
    let (temporary, mut output) = create_owned_temp(destination, kind)?;
    let result = (|| {
        output.write_all(bytes)?;
        output.sync_all()?;
        prepare(&temporary)?;
        // Keep flushing through the original writable handle. Windows rejects
        // FlushFileBuffers on a read-only reopen, and `prepare` may also have
        // applied destination permissions that prevent a writable reopen.
        output.sync_all()?;
        drop(output);
        replace_via_rename(&temporary, destination)?;
        sync_parent_directory(destination, directory_policy)
    })();
    if result.is_err() {
        remove_owned_temp(&temporary);
    }
    result
}

pub fn append_durable(
    path: &Path,
    frame: &[u8],
    directory_policy: DirectorySyncPolicy,
) -> io::Result<u64> {
    tighten_existing_file(path)?;
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(path)?;
    let offset = output.seek(SeekFrom::End(0))?;
    output.write_all(frame)?;
    output.sync_all()?;
    sync_parent_directory(path, directory_policy)?;
    Ok(offset)
}

pub fn truncate_file(
    path: &Path,
    len: u64,
    directory_policy: DirectorySyncPolicy,
) -> io::Result<()> {
    tighten_existing_file(path)?;
    let output = OpenOptions::new().write(true).open(path)?;
    output.set_len(len)?;
    output.sync_all()?;
    tighten_existing_file(path)?;
    sync_parent_directory(path, directory_policy)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{DirectorySyncPolicy, atomic_write_prepared};

    fn deny_writes(path: &Path) {
        let mut permissions = fs::metadata(path).expect("staging metadata").permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o400);
        }
        #[cfg(not(unix))]
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).expect("deny staging writes");
    }

    fn restore_writes(path: &Path) {
        let mut permissions = fs::metadata(path)
            .expect("published metadata")
            .permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o600);
        }
        #[cfg(not(unix))]
        permissions.set_readonly(false);
        let _ = fs::set_permissions(path, permissions);
    }

    #[test]
    fn a_prepared_publish_survives_a_staging_file_that_denies_writes() {
        let root = tempfile::tempdir().expect("publish fixture root");
        let destination = root.path().join("config.json");

        atomic_write_prepared(
            &destination,
            "fixture",
            b"published",
            |temporary| {
                deny_writes(temporary);
                Ok(())
            },
            DirectorySyncPolicy::TolerateUnsupported,
        )
        .expect("prepared publish over a write-denied staging file");

        assert_eq!(
            fs::read(&destination).expect("published bytes"),
            b"published"
        );
        let leftovers = fs::read_dir(root.path())
            .expect("publish directory")
            .filter_map(Result::ok)
            .filter(|entry| entry.path() != destination)
            .count();
        restore_writes(&destination);
        assert_eq!(leftovers, 0, "the staging file is consumed by the rename");
    }
}
