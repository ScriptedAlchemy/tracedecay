//! Crash-safe filesystem primitives for hook and host-admission framed logs.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectorySyncPolicy {
    /// Surface every fsync failure.
    Strict,
    /// Surface genuine IO failures but tolerate unsupported directory fsync.
    TolerateUnsupported,
}

/// Flush a directory's metadata so a preceding create/rename/remove is durable.
#[hotpath::measure(label = "private_fs.framed_log.sync_directory")]
pub fn sync_directory(dir: &Path, policy: DirectorySyncPolicy) -> io::Result<()> {
    #[cfg(unix)]
    {
        match File::open(dir).and_then(|directory| sync_owned_file(&directory)) {
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

pub fn sync_parent_directory(path: &Path, policy: DirectorySyncPolicy) -> io::Result<()> {
    match path.parent() {
        Some(parent) => sync_directory(parent, policy),
        None => Ok(()),
    }
}

/// Flush an already-written file's contents so its bytes are durable.
///
/// The handle is opened for reading *and* writing. Windows implements
/// `File::sync_all` with `FlushFileBuffers`, which requires write access and
/// fails with `ERROR_ACCESS_DENIED` on a read-only handle, while Unix `fsync`
/// accepts a read-only descriptor; opening for write is the one shape that is
/// durable on both. Shared so no caller reintroduces the read-only open.
#[hotpath::measure(label = "private_fs.framed_log.sync_file_at")]
pub fn sync_file_at(path: &Path) -> io::Result<()> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .and_then(|file| sync_owned_file(&file))
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

/// Restrict an existing file to owner read/write (`0o600` on unix).
///
/// Call this on a path you just created. Prefer [`tighten_existing_file`]
/// when the file may be missing (that helper no-ops on `NotFound`).
#[cfg(unix)]
pub fn set_owner_private_file_mode(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
pub fn set_owner_private_file_mode(_path: &Path) -> io::Result<()> {
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
    set_owner_private_file_mode(path)
}

#[hotpath::measure(label = "private_fs.framed_log.read_bounded")]
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
    hotpath::gauge!("private_fs.framed_log.read_bytes").set(length);
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

fn remove_owned_temp_if_contents_match(path: &Path, owned_contents: &[u8]) {
    if fs::read(path).is_ok_and(|contents| contents == owned_contents) {
        remove_owned_temp(path);
    }
}

fn sync_owned_file(file: &File) -> io::Result<()> {
    hotpath::measure_block!("private_fs.framed_log.fsync", file.sync_all())
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
#[hotpath::measure(label = "private_fs.framed_log.publish")]
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
        sync_owned_file(&output)?;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConditionalPublishExpectation {
    Missing,
    Present,
}

pub struct ConditionalPublishCallbacks<
    Prepare,
    BeforePublish,
    AfterPublish,
    VerifyDisplaced,
    VerifyPublished,
> {
    pub prepare: Prepare,
    pub before_publish: BeforePublish,
    pub after_publish: AfterPublish,
    pub verify_displaced: VerifyDisplaced,
    pub verify_published: VerifyPublished,
}

fn unused_temporary_path(destination: &Path, kind: &str) -> io::Result<PathBuf> {
    for _ in 0..64 {
        let path = temporary_path(destination, kind);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(path),
            Ok(_) => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate conditional publish rollback path",
    ))
}

#[cfg(target_os = "linux")]
fn exchange_paths(first: &Path, second: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let first = CString::new(first.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))?;
    let second = CString::new(second.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            first.as_ptr(),
            libc::AT_FDCWD,
            second.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn exchange_paths(first: &Path, second: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let first = CString::new(first.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))?;
    let second = CString::new(second.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))?;
    let result = unsafe { libc::renamex_np(first.as_ptr(), second.as_ptr(), libc::RENAME_SWAP) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn exchange_paths(_first: &Path, _second: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic file exchange is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn replace_existing_with_backup(
    replacement: &Path,
    destination: &Path,
    backup: &Path,
) -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{REPLACEFILE_WRITE_THROUGH, ReplaceFileW};

    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let destination_wide = wide(destination);
    let replacement_wide = wide(replacement);
    let backup_wide = wide(backup);
    let result = unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            replacement_wide.as_ptr(),
            backup_wide.as_ptr(),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(backup.to_path_buf())
    }
}

#[cfg(unix)]
fn replace_existing_with_backup(
    replacement: &Path,
    destination: &Path,
    _backup: &Path,
) -> io::Result<PathBuf> {
    exchange_paths(replacement, destination)?;
    Ok(replacement.to_path_buf())
}

#[cfg(target_os = "linux")]
fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))?;
    let destination = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))?;
    let destination = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))?;
    let result =
        unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn rename_noreplace(_source: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-replace rename is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let source = wide(source);
    let destination = wide(destination);
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
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

#[hotpath::measure(label = "private_fs.framed_log.write_prepared")]
pub fn atomic_write_prepared(
    destination: &Path,
    kind: &str,
    bytes: &[u8],
    prepare: impl FnOnce(&Path) -> io::Result<()>,
    directory_policy: DirectorySyncPolicy,
) -> io::Result<()> {
    hotpath::gauge!("private_fs.framed_log.write_bytes").set(bytes.len());
    validate_regular_or_missing(destination)?;
    let (temporary, mut output) = create_owned_temp(destination, kind)?;
    let result = (|| {
        output.write_all(bytes)?;
        sync_owned_file(&output)?;
        prepare(&temporary)?;
        // Keep flushing through the original writable handle. Windows rejects
        // FlushFileBuffers on a read-only reopen, and `prepare` may also have
        // applied destination permissions that prevent a writable reopen.
        sync_owned_file(&output)?;
        drop(output);
        replace_via_rename(&temporary, destination)?;
        sync_parent_directory(destination, directory_policy)
    })();
    if result.is_err() {
        remove_owned_temp(&temporary);
    }
    result
}

/// Stage a prepared file and publish only against the exact expected
/// destination existence state.
///
/// A missing destination uses an atomic no-replace rename. An existing
/// destination is atomically replaced while retaining the displaced object;
/// the caller verifies that object against its exact snapshot. A mismatch is
/// rolled back before this function returns an error.
#[hotpath::measure(label = "private_fs.framed_log.write_prepared_conditionally")]
pub fn atomic_write_prepared_conditionally<
    Prepare,
    BeforePublish,
    AfterPublish,
    VerifyDisplaced,
    VerifyPublished,
>(
    destination: &Path,
    kind: &str,
    bytes: &[u8],
    expectation: ConditionalPublishExpectation,
    callbacks: ConditionalPublishCallbacks<
        Prepare,
        BeforePublish,
        AfterPublish,
        VerifyDisplaced,
        VerifyPublished,
    >,
    directory_policy: DirectorySyncPolicy,
) -> io::Result<()>
where
    Prepare: FnOnce(&Path) -> io::Result<()>,
    BeforePublish: FnOnce(),
    AfterPublish: FnOnce(),
    VerifyDisplaced: FnOnce(&Path) -> io::Result<bool>,
    VerifyPublished: FnOnce(&Path) -> io::Result<bool>,
{
    let ConditionalPublishCallbacks {
        prepare,
        before_publish,
        after_publish,
        verify_displaced,
        verify_published,
    } = callbacks;
    hotpath::gauge!("private_fs.framed_log.write_bytes").set(bytes.len());
    validate_regular_or_missing(destination)?;
    let (temporary, mut output) = create_owned_temp(destination, kind)?;
    let published_existing = std::cell::Cell::new(false);
    let result = (|| {
        output.write_all(bytes)?;
        sync_owned_file(&output)?;
        prepare(&temporary)?;
        sync_owned_file(&output)?;
        drop(output);
        before_publish();
        match expectation {
            ConditionalPublishExpectation::Missing => {
                rename_noreplace(&temporary, destination).map_err(|error| {
                    if error.kind() == io::ErrorKind::AlreadyExists {
                        io::Error::other("destination changed since it was read")
                    } else {
                        error
                    }
                })?;
                after_publish();
            }
            ConditionalPublishExpectation::Present => {
                let publish_backup = unused_temporary_path(destination, "conditional-backup")?;
                let displaced_path =
                    replace_existing_with_backup(&temporary, destination, &publish_backup)?;
                published_existing.set(true);
                after_publish();
                let displaced = verify_displaced(&displaced_path);
                if !matches!(displaced, Ok(true)) {
                    let rollback_backup =
                        unused_temporary_path(destination, "conditional-rollback")?;
                    let rolled_back_published = replace_existing_with_backup(
                        &displaced_path,
                        destination,
                        &rollback_backup,
                    )
                    .map_err(|error| {
                        io::Error::other(format!(
                            "failed to rollback conditional publication; displaced destination is retained at {}: {error}",
                            displaced_path.display()
                        ))
                    })?;
                    match verify_published(&rolled_back_published) {
                        Ok(true) => {
                            if let Err(error) = fs::remove_file(&rolled_back_published) {
                                return Err(io::Error::other(format!(
                                    "rolled back conditional publication but could not remove the staged file retained at {}: {error}",
                                    rolled_back_published.display()
                                )));
                            }
                        }
                        Ok(false) => {
                            sync_parent_directory(destination, directory_policy)?;
                            return Err(io::Error::other(format!(
                                "conditional publication was ambiguous; the boundary destination was restored and the changed staged file is retained at {}",
                                rolled_back_published.display()
                            )));
                        }
                        Err(error) => {
                            sync_parent_directory(destination, directory_policy)?;
                            return Err(io::Error::other(format!(
                                "conditional publication was ambiguous; the boundary destination was restored and the unverifiable staged file is retained at {}: {error}",
                                rolled_back_published.display()
                            )));
                        }
                    }
                    sync_parent_directory(destination, directory_policy)?;
                    return match displaced {
                        Err(error) => Err(error),
                        Ok(false) => Err(io::Error::other("destination changed since it was read")),
                        Ok(true) => Err(io::Error::other(
                            "conditional publication verification changed result",
                        )),
                    };
                }
                if let Err(error) = fs::remove_file(&displaced_path) {
                    sync_parent_directory(destination, directory_policy)?;
                    return Err(io::Error::other(format!(
                        "published replacement but could not remove the displaced destination retained at {}: {error}",
                        displaced_path.display()
                    )));
                }
            }
        }
        sync_parent_directory(destination, directory_policy)
    })();
    if result.is_err() && !published_existing.get() {
        remove_owned_temp_if_contents_match(&temporary, bytes);
    }
    result
}

/// Remove an existing destination only after atomically retaining and
/// verifying the exact object that occupied the path at publication time.
#[hotpath::measure(label = "private_fs.framed_log.remove_conditionally")]
pub fn remove_conditionally(
    destination: &Path,
    before_publish: impl FnOnce(),
    verify_displaced: impl FnOnce(&Path) -> io::Result<bool>,
    directory_policy: DirectorySyncPolicy,
) -> io::Result<()> {
    if !validate_regular_or_missing(destination)? {
        return Ok(());
    }
    let rollback = unused_temporary_path(destination, "conditional-remove")?;
    before_publish();
    rename_noreplace(destination, &rollback)?;
    let displaced = verify_displaced(&rollback);
    if !matches!(displaced, Ok(true)) {
        if let Err(rollback_error) = rename_noreplace(&rollback, destination) {
            return Err(io::Error::other(format!(
                "conditional remove verification failed and rollback is retained at {}: {rollback_error}",
                rollback.display()
            )));
        }
        sync_parent_directory(destination, directory_policy)?;
        return match displaced {
            Err(error) => Err(error),
            Ok(false) => Err(io::Error::other("destination changed since it was read")),
            Ok(true) => Err(io::Error::other(
                "conditional remove verification changed result",
            )),
        };
    }
    if let Err(error) = fs::remove_file(&rollback) {
        if let Err(rollback_error) = rename_noreplace(&rollback, destination) {
            return Err(io::Error::other(format!(
                "failed to remove retained destination ({error}); rollback is retained at {}: {rollback_error}",
                rollback.display()
            )));
        }
        sync_parent_directory(destination, directory_policy)?;
        return Err(error);
    }
    sync_parent_directory(destination, directory_policy)
}

#[hotpath::measure(label = "private_fs.framed_log.append")]
pub fn append_durable(
    path: &Path,
    frame: &[u8],
    directory_policy: DirectorySyncPolicy,
) -> io::Result<u64> {
    hotpath::gauge!("private_fs.framed_log.write_bytes").set(frame.len());
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
    sync_owned_file(&output)?;
    sync_parent_directory(path, directory_policy)?;
    Ok(offset)
}

#[hotpath::measure(label = "private_fs.framed_log.truncate")]
pub fn truncate_file(
    path: &Path,
    len: u64,
    directory_policy: DirectorySyncPolicy,
) -> io::Result<()> {
    tighten_existing_file(path)?;
    let output = OpenOptions::new().write(true).open(path)?;
    output.set_len(len)?;
    sync_owned_file(&output)?;
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
