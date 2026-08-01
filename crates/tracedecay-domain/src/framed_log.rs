//! Crash-safe framed-log primitives shared by hook and host-admission spools.
//!
//! Frame encoding and scan policy stay product-specific; this module holds the
//! deterministic checksum and append-intent evidence helpers plus the
//! append/rename/metadata I/O that makes a publish durable. Neither half owns
//! spool policy, transport, SQL, or daemon authority, so both belong in the
//! dependency-free kernel every spool implementation already links.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

/// Trailing SHA-256 over exact framed bytes (excluding the checksum suffix).
pub const CHECKSUM_BYTES: usize = 32;

/// SHA-256 over the exact bytes that precede a frame checksum suffix.
pub fn checksum(input: &[u8]) -> [u8; 32] {
    Sha256::digest(input).into()
}

/// Returns true when `tail` is a strict prefix of the unpublished frame bytes
/// recorded in an append intent.
pub fn partial_tail_matches_prefix(tail: &[u8], expected: &[u8], framed_len: usize) -> bool {
    !tail.is_empty() && tail.len() < framed_len && expected.starts_with(tail)
}

/// How a directory fsync failure is surfaced to the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectorySyncPolicy {
    /// Surface every fsync failure.
    Strict,
    /// Surface genuine IO failures but tolerate unsupported directory fsync.
    TolerateUnsupported,
    /// Never surface a fsync failure.
    BestEffort,
}

/// Flush a directory's metadata so a preceding create/rename/remove is durable.
pub fn sync_directory(dir: &Path, policy: DirectorySyncPolicy) -> io::Result<()> {
    #[cfg(unix)]
    {
        match File::open(dir).and_then(|directory| directory.sync_all()) {
            Ok(()) => Ok(()),
            Err(_) if matches!(policy, DirectorySyncPolicy::BestEffort) => Ok(()),
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
        drop(output);
        prepare(&temporary)?;
        File::open(&temporary)?.sync_all()?;
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
    use super::checksum;

    #[test]
    fn checksum_matches_sha256() {
        assert_eq!(
            checksum(b"abc"),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }
}
