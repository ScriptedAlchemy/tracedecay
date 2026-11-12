//! Capability-relative durable directory primitives shared by the quarantine
//! and retirement authorities: atomic no-replace rename between already-open
//! parent capabilities, sibling replacement, directory metadata sync, recursive no-follow removal,
//! and create-or-open of one entry that survives the Darwin create race.
//! Every mutation is relative to an open `Dir` handle so a parent path
//! swapped for a symlink cannot redirect the operation.

#[cfg(unix)]
use std::ffi::CString;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use cap_fs_ext::DirExt;
#[cfg(not(windows))]
use cap_fs_ext::OpenOptionsMaybeDirExt;
#[cfg(windows)]
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, File, OpenOptions};

#[cfg(unix)]
use crate::framed_log::normalize_no_follow_error;

/// How many times a create-or-open that reports the entry missing is asked
/// again before the answer is believed. xnu bounds its own retry of the same
/// condition at ten; downstream measurements found one extra look always
/// sufficed, so this is generous without letting a genuinely missing parent
/// spin.
const CREATE_RACE_LOOKS: usize = 8;

/// Creates or opens `name` beneath an already-open directory capability.
///
/// `options` must request `create(true)` without `create_new`: this is the
/// create-or-open form, whose two correct answers are "the entry was created"
/// and "the existing entry was opened". On macOS that form has a defect the
/// others do not. `openat(dirfd, name, O_CREAT)` without `O_EXCL` hands most
/// callers that lose the first-creation race of one name a spurious `ENOENT`
/// (xnu's `vn_open_auth` retries a create that failed with `EEXIST` as an
/// open, and the fallback is not atomic), even though the entry is present
/// the moment the error arrives. `O_CREAT|O_EXCL`, opens of an existing entry,
/// and absolute-path `open` are all correct, which is why the std-based
/// sidecar locks never see it and only the capability-relative lock and
/// ledger opens do, and those are exactly the files many writers create at
/// once.
///
/// A `NotFound` answer is therefore looked at again a bounded number of
/// times. A parent that is genuinely gone still reports `NotFound` after the
/// bound, so the caller's error mapping keeps working.
pub fn open_or_create_with(
    directory: &Dir,
    name: &OsStr,
    options: &OpenOptions,
) -> io::Result<File> {
    look_again_on_not_found(|| directory.open_with(name, options))
}

fn look_again_on_not_found<T>(mut attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut looks = 0;
    loop {
        match attempt() {
            Err(error) if error.kind() == io::ErrorKind::NotFound && looks < CREATE_RACE_LOOKS => {
                looks += 1;
                std::thread::yield_now();
            }
            result => return result,
        }
    }
}

/// Atomically renames one directory entry between already-open parent
/// capabilities without allowing an occupied destination to be replaced.
/// Platforms without a suitable primitive fail closed: retaining bytes is
/// always preferable to risking a replacement.
pub fn rename_noreplace(
    from_parent: &Dir,
    from: &OsStr,
    to_parent: &Dir,
    to: &OsStr,
) -> io::Result<()> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        crate::rename_noreplace::rename_noreplace_at(
            from_parent.as_raw_fd(),
            from,
            to_parent.as_raw_fd(),
            to,
        )
    }
    #[cfg(target_os = "macos")]
    {
        crate::rename_noreplace::rename_noreplace_at(
            from_parent.as_raw_fd(),
            from,
            to_parent.as_raw_fd(),
            to,
        )
    }
    #[cfg(windows)]
    {
        let from = dir_entry_path(from_parent, from)?;
        let to = dir_entry_path(to_parent, to)?;
        crate::rename_noreplace::rename_noreplace_paths(&from, &to)
    }
    #[cfg(not(any(
        all(target_os = "linux", target_env = "gnu"),
        target_os = "macos",
        windows
    )))]
    {
        let _ = (from_parent, from, to_parent, to);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace rename is unavailable on this platform",
        ))
    }
}

#[cfg(windows)]
fn dir_entry_path(parent: &Dir, name: &OsStr) -> io::Result<std::path::PathBuf> {
    Ok(dir_path(parent)?.join(name))
}

/// Atomically replaces a sibling file beneath a held parent capability.
/// Windows uses the shared namespace-durable rename authority, including its
/// NTFS fallback for replacement while delete-sharing readers remain open.
pub fn replace_file(parent: &Dir, source: &OsStr, destination: &OsStr) -> io::Result<()> {
    for name in [source, destination] {
        let mut components = std::path::Path::new(name).components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "file replacement requires sibling entry names",
            ));
        }
    }
    #[cfg(windows)]
    {
        let source = dir_entry_path(parent, source)?;
        let destination = dir_entry_path(parent, destination)?;
        crate::windows::rename_paths_durable(
            &source,
            &destination,
            crate::windows::RenameMode::Replace,
        )
    }
    #[cfg(not(windows))]
    parent.rename(source, parent, destination)
}

/// Resolves an open directory capability back to its live filesystem path so
/// path-based Win32 primitives can address entries below it.
#[cfg(windows)]
fn dir_path(directory: &Dir) -> io::Result<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW,
    };

    let get_path = |buffer: *mut u16, length: u32| {
        // SAFETY: `buffer`/`length` describe a writable UTF-16 buffer (or a
        // zero-length probe) and the handle is an open directory capability.
        unsafe {
            GetFinalPathNameByHandleW(
                directory.as_raw_handle(),
                buffer,
                length,
                FILE_NAME_NORMALIZED,
            )
        }
    };
    let required = get_path(std::ptr::null_mut(), 0);
    if required == 0 {
        return Err(io::Error::last_os_error());
    }
    let capacity = required.checked_add(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "directory capability path length overflowed",
        )
    })?;
    let mut buffer = vec![0_u16; capacity as usize];
    let written = get_path(buffer.as_mut_ptr(), buffer.len() as u32);
    if written == 0 {
        return Err(io::Error::last_os_error());
    }
    if written as usize >= buffer.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory capability path changed while resolving it",
        ));
    }
    buffer.truncate(written as usize);
    Ok(std::path::PathBuf::from(std::ffi::OsString::from_wide(
        &buffer,
    )))
}

/// Flushes a directory capability's metadata so a preceding create, rename,
/// or unlink beneath it is durable.
pub fn sync_directory(directory: &Dir) -> io::Result<()> {
    #[cfg(windows)]
    {
        directory.dir_metadata().map(|_| ())
    }
    #[cfg(not(windows))]
    {
        let mut options = OpenOptions::new();
        options.read(true).maybe_dir(true);
        directory
            .open_with(".", &options)
            .and_then(|file| file.sync_all())
    }
}

fn single_component(name: &OsStr) -> io::Result<()> {
    let mut parts = Path::new(name).components();
    if !matches!(parts.next(), Some(std::path::Component::Normal(_))) || parts.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory operation requires one entry name",
        ));
    }
    Ok(())
}

/// Persist a new producer-owned staging receipt through a held directory.
///
/// An exclusive marker creation is the only authority for removing an
/// incomplete marker. On write/sync failure, remove only that marker and
/// attempt a nonrecursive removal of the same held directory. A substituted
/// directory containing other bytes must survive. The caller must disable
/// pathname-based temporary-directory cleanup before invoking this operation.
pub fn write_exclusive_pending_receipt_nofollow(
    directory_path: &Path,
    filename: &OsStr,
    contents: &[u8],
) -> io::Result<()> {
    single_component(filename)?;
    let directory = open_root_nofollow(directory_path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut marker = directory.open_with(filename, &options)?;
    #[cfg(unix)]
    let marker_identity = {
        use cap_std::fs::MetadataExt;
        marker
            .metadata()
            .ok()
            .map(|metadata| (metadata.dev(), metadata.ino()))
    };
    let written = marker.write_all(contents).and_then(|()| marker.sync_all());
    drop(marker);
    if let Err(error) = written {
        // Keep the original write failure. A different marker entry is not
        // ours to delete, and a nonempty substituted directory is not ours
        // to remove. Neither cleanup operation follows an ambient pathname.
        #[cfg(unix)]
        let same_marker = {
            use cap_std::fs::MetadataExt;
            marker_identity.is_some_and(|identity| {
                directory
                    .symlink_metadata(filename)
                    .ok()
                    .is_some_and(|metadata| {
                        metadata.is_file() && (metadata.dev(), metadata.ino()) == identity
                    })
            })
        };
        #[cfg(not(unix))]
        let same_marker = false;
        if same_marker {
            if let Err(cleanup) = directory.remove_file(filename) {
                tracing::warn!(%cleanup, "pending owner marker could not be removed");
            } else if let Err(cleanup) = directory.remove_open_dir() {
                tracing::warn!(%cleanup, "pending owner directory was not empty");
            }
        }
        return Err(error);
    }
    Ok(())
}

/// Publish a rebuildable marker through one held directory. Replacing the
/// pathname after acquisition cannot redirect its bytes into a foreign tree.
pub fn atomic_replace_child_file_nofollow(
    directory_path: &Path,
    filename: &OsStr,
    contents: &[u8],
) -> io::Result<()> {
    single_component(filename)?;
    let directory = open_root_nofollow(directory_path)?;
    atomic_replace_file_in_dir(&directory, filename, contents)
}

fn atomic_replace_file_in_dir(
    directory: &Dir,
    filename: &OsStr,
    contents: &[u8],
) -> io::Result<()> {
    // An accelerator may replace a regular cache file, but cannot take
    // ownership of an unrelated symlink, directory, FIFO or device entry.
    // Check relative to the held parent, never through an ambient pathname.
    fn regular_or_missing(directory: &Dir, filename: &OsStr) -> io::Result<()> {
        match directory.symlink_metadata(filename) {
            Ok(metadata) if metadata.file_type().is_file() => Ok(()),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "sealed proof destination is not a regular file",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    regular_or_missing(directory, filename)?;
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let (temporary, mut file) = (0..8)
        .find_map(|_| {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let temporary = OsString::from(format!(
                ".sealed-proof-{}-{:016x}-{sequence:016x}.tmp",
                std::process::id(),
                crate::process_instance_nonce(),
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            match directory.open_with(&temporary, &options) {
                Ok(file) => Some(Ok((temporary, file))),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not exclusively stage a sealed proof marker",
            )
        })?;
    let persisted = file.write_all(contents).and_then(|()| file.sync_all());
    drop(file);
    let result = persisted
        // Recheck before publishing in case the entry changed during staging.
        .and_then(|()| regular_or_missing(directory, filename))
        .and_then(|()| replace_file(directory, &temporary, filename))
        .and_then(|()| match sync_directory(directory) {
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
            result => result,
        });
    if result.is_err() {
        let _ = directory.remove_file(&temporary);
    }
    result
}

fn open_root_nofollow(root_path: &Path) -> io::Result<Dir> {
    let parent_path = root_path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "sealed root has no parent"))?;
    let root_name = root_path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "sealed root has no name"))?;
    let parent = Dir::open_ambient_dir(parent_path, cap_std::ambient_authority())?;
    parent.open_dir_nofollow(root_name)
}

/// Install a staged generation without replacing even an empty destination
/// directory. The final component of the sealed root and the staging entry
/// must be real directories; a link or an occupied generation is not a
/// recoverable staging collision.
///
/// The caller still owns the responsibility for the identity of the store
/// ancestors, which this pathname API cannot establish.
pub fn install_child_directory_noreplace_nofollow(
    root_path: &Path,
    staging_name: &OsStr,
    generation_name: &OsStr,
) -> io::Result<()> {
    single_component(staging_name)?;
    single_component(generation_name)?;
    let root = open_root_nofollow(root_path)?;
    let staging = root.open_dir_nofollow(staging_name)?;
    // Reject a linked staging entry, then close the extra directory handle
    // before publication for platforms that disallow renaming open children.
    drop(staging);
    rename_noreplace(&root, staging_name, &root, generation_name)
}

/// A directory held open for a producer's writes. Every mutation is relative
/// to this handle, so replacing the pathname cannot redirect bytes into a
/// foreign tree. The one path-based writer resolves [`Self::path`] from the
/// handle, which names the directory this handle owns rather than the
/// substitutable entry name.
pub struct HeldDirectory {
    directory: Dir,
}

/// Durable identity of a held directory, compared across a rename.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectoryIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    volume_serial_number: u32,
    #[cfg(windows)]
    file_index: u64,
}

impl HeldDirectory {
    /// Exclusively creates `name` beneath `root_path` and holds it. An
    /// occupied name is never adopted: the caller owns only the entry it
    /// created.
    pub fn create_exclusive(root_path: &Path, name: &OsStr) -> io::Result<Self> {
        single_component(name)?;
        let root = open_root_nofollow(root_path)?;
        root.create_dir(name)?;
        root.open_dir_nofollow(name)
            .map(|directory| Self { directory })
    }

    /// Opens an existing child of `root_path` without following a link.
    pub fn open_nofollow(root_path: &Path, name: &OsStr) -> io::Result<Self> {
        single_component(name)?;
        let root = open_root_nofollow(root_path)?;
        root.open_dir_nofollow(name)
            .map(|directory| Self { directory })
    }

    /// Writes a new file exclusively, relative to this handle.
    pub fn write_new_file(&self, name: &OsStr, contents: &[u8]) -> io::Result<()> {
        single_component(name)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = self.directory.open_with(name, &options)?;
        file.write_all(contents).and_then(|()| file.sync_all())
    }

    /// Creates an anonymous file in this directory: created exclusively and
    /// unlinked immediately, so it leaves no entry behind.
    pub fn anonymous_file(&self) -> io::Result<File> {
        static NEXT_SPOOL: AtomicU64 = AtomicU64::new(0);
        let (name, file) = (0..8)
            .find_map(|_| {
                let sequence = NEXT_SPOOL.fetch_add(1, Ordering::Relaxed);
                let name = OsString::from(format!(
                    ".spool-{}-{:016x}-{sequence:016x}",
                    std::process::id(),
                    crate::process_instance_nonce(),
                ));
                let mut options = OpenOptions::new();
                options.read(true).write(true).create_new(true);
                match self.directory.open_with(&name, &options) {
                    Ok(file) => Some(Ok((name, file))),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .transpose()?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "could not exclusively stage an anonymous spool",
                )
            })?;
        self.directory.remove_file(&name)?;
        Ok(file)
    }

    /// Hard-links `source` to `name` relative to this handle. The source's
    /// parent is opened without following a link, so neither side is an
    /// ambient pathname.
    pub fn hard_link_from(&self, source: &Path, name: &OsStr) -> io::Result<()> {
        single_component(name)?;
        let source_parent = source.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "hard-link source has no parent",
            )
        })?;
        let source_name = source.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "hard-link source has no name")
        })?;
        let parent = open_root_nofollow(source_parent)?;
        parent.hard_link(source_name, &self.directory, name)
    }

    /// Resolves this held directory to a path that names the same inode, so a
    /// path-based writer can create a child without going through a
    /// substitutable ancestor name.
    pub fn path(&self) -> io::Result<std::path::PathBuf> {
        held_directory_path(&self.directory)
    }

    /// The directory's own durable identity.
    pub fn identity(&self) -> io::Result<DirectoryIdentity> {
        directory_identity(&self.directory)
    }
}

/// Install a held staging directory as the generation artifact without
/// replacing an occupied destination. The entry at `staging_name` must still
/// be the held directory, and the installed entry must remain that same
/// directory, so a replacement between the producer's writes and publication
/// is refused rather than installed.
pub fn install_held_child_directory_noreplace(
    root_path: &Path,
    staging: &HeldDirectory,
    staging_name: &OsStr,
    generation_name: &OsStr,
) -> io::Result<()> {
    single_component(staging_name)?;
    single_component(generation_name)?;
    let root = open_root_nofollow(root_path)?;
    let entry = root.open_dir_nofollow(staging_name)?;
    let entry_identity = directory_identity(&entry)?;
    drop(entry);
    if entry_identity != staging.identity()? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "sealed staging entry is not the held producer directory",
        ));
    }
    rename_noreplace(&root, staging_name, &root, generation_name)?;
    let installed = root.open_dir_nofollow(generation_name)?;
    let installed_identity = directory_identity(&installed)?;
    drop(installed);
    if installed_identity != staging.identity()? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "sealed generation entry is not the held producer directory",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn directory_identity(directory: &Dir) -> io::Result<DirectoryIdentity> {
    use cap_std::fs::MetadataExt;
    let metadata = directory.dir_metadata()?;
    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn directory_identity(directory: &Dir) -> io::Result<DirectoryIdentity> {
    let information = crate::windows_file::information(directory)?;
    Ok(DirectoryIdentity {
        volume_serial_number: information.volume_serial_number,
        file_index: information.file_index,
    })
}

#[cfg(not(any(unix, windows)))]
fn directory_identity(_directory: &Dir) -> io::Result<DirectoryIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "directory identity is unavailable on this platform",
    ))
}

#[cfg(windows)]
fn held_directory_path(directory: &Dir) -> io::Result<std::path::PathBuf> {
    dir_path(directory)
}

#[cfg(target_os = "linux")]
fn held_directory_path(directory: &Dir) -> io::Result<std::path::PathBuf> {
    use std::os::fd::AsRawFd;
    Ok(std::path::PathBuf::from(format!(
        "/proc/self/fd/{}",
        directory.as_raw_fd()
    )))
}

#[cfg(target_os = "macos")]
fn held_directory_path(directory: &Dir) -> io::Result<std::path::PathBuf> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStringExt;
    let descriptor = directory.as_raw_fd();
    let mut buffer = vec![0_u8; libc::PATH_MAX as usize];
    // SAFETY: `buffer` is writable for `PATH_MAX` bytes and `descriptor` is an
    // open directory descriptor owned by `directory`.
    let result = unsafe {
        libc::fcntl(
            descriptor,
            libc::F_GETPATH,
            buffer.as_mut_ptr().cast::<libc::c_char>(),
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    let length = buffer.iter().position(|byte| *byte == 0).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "held directory path was not NUL-terminated",
        )
    })?;
    buffer.truncate(length);
    Ok(std::path::PathBuf::from(std::ffi::OsString::from_vec(
        buffer,
    )))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn held_directory_path(_directory: &Dir) -> io::Result<std::path::PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "held directory path resolution is unavailable on this platform",
    ))
}

/// Bound deduplication metadata independently of the size of the store.
/// The Doctor caller reports partial coverage instead of trusting a census
/// whose inode inventory would otherwise grow without limit.
const MAX_SEALED_CENSUS_INODES: usize = 65_536;

/// Census proof-bound children through held root/child capabilities, not
/// ambient paths. Unknown entries are counted but never traversed. On Unix,
/// every visited inode consumes bounded deduplication memory until the walk
/// ends: publication can add links even to a formerly single-linked file.
/// Exhausting that evidence budget returns `OutOfMemory`, never complete
/// totals. Receipts are bounded and read from the child being charged.
/// Category ordering is authority ordering: lower keys receive shared
/// physical bytes before higher keys, independent of directory iteration.
pub fn census_receipted_directory_bytes<K: Ord + Copy>(
    root_path: &Path,
    unrecognized: K,
    receipt_for: impl FnMut(&OsStr) -> (&'static str, usize),
    classify: impl FnMut(&OsStr, Option<&[u8]>) -> K,
) -> io::Result<std::collections::BTreeMap<K, (u64, u64)>> {
    census_receipted_directory_bytes_bounded(
        root_path,
        unrecognized,
        receipt_for,
        classify,
        MAX_SEALED_CENSUS_INODES,
    )
}

fn census_receipted_directory_bytes_bounded<K: Ord + Copy>(
    root_path: &Path,
    unrecognized: K,
    receipt_for: impl FnMut(&OsStr) -> (&'static str, usize),
    classify: impl FnMut(&OsStr, Option<&[u8]>) -> K,
    max_inodes: usize,
) -> io::Result<std::collections::BTreeMap<K, (u64, u64)>> {
    let root = match open_root_nofollow(root_path) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(error) => return Err(error),
    };
    census_receipted_children(
        &root,
        root.read_dir(".")?
            .map(|entry| entry.map(|entry| entry.file_name())),
        unrecognized,
        receipt_for,
        classify,
        max_inodes,
    )
}

fn census_receipted_children<K: Ord + Copy>(
    root: &Dir,
    entries: impl Iterator<Item = io::Result<OsString>>,
    unrecognized: K,
    mut receipt_for: impl FnMut(&OsStr) -> (&'static str, usize),
    mut classify: impl FnMut(&OsStr, Option<&[u8]>) -> K,
    max_inodes: usize,
) -> io::Result<std::collections::BTreeMap<K, (u64, u64)>> {
    let mut totals = std::collections::BTreeMap::<K, (u64, u64)>::new();
    // Hard links belong to the highest-priority verified category, not the
    // first entry returned by read_dir. A link count is not a closed group:
    // concurrent publication can add an alias after its last observed link.
    let mut counted = std::collections::HashMap::<(u64, u64), (K, u64)>::new();
    for name in entries {
        let name = name?;
        single_component(&name)?;
        // A real generation that cannot be opened may still own gigabytes.
        // Do not report complete zero-byte coverage by treating I/O failure
        // as an unknown entry; preserve the failure for partial reporting.
        let child = if root.symlink_metadata(&name)?.is_dir() {
            Some(root.open_dir_nofollow(&name)?)
        } else {
            None
        };
        let (category, bytes) = if let Some(child) = child {
            let (receipt_name, maximum) = receipt_for(&name);
            // Malformed, absent, or nonregular receipts identify unrecognized
            // entries. An I/O refusal such as EACCES or EMFILE does not prove
            // absence and must leave coverage explicitly unavailable.
            let receipt =
                match read_bounded_child_receipt(&child, OsStr::new(receipt_name), maximum) {
                    Ok(bytes) => Some(bytes),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::NotFound
                                | io::ErrorKind::InvalidData
                                | io::ErrorKind::InvalidInput
                                | io::ErrorKind::IsADirectory
                        ) =>
                    {
                        None
                    }
                    Err(error) => return Err(error),
                };
            let category = classify(&name, receipt.as_deref());
            let bytes = if category == unrecognized {
                0
            } else {
                directory_bytes_from_held_dir(
                    child,
                    category,
                    &mut totals,
                    &mut counted,
                    max_inodes,
                )?
            };
            (category, bytes)
        } else {
            (unrecognized, 0)
        };
        let totals = totals.entry(category).or_default();
        totals.0 = totals.0.saturating_add(1);
        totals.1 = totals.1.saturating_add(bytes);
    }
    Ok(totals)
}

/// Walk no-follow entries relative to the same held child, even after its
/// old pathname has been renamed. The stack scales with directory depth.
fn directory_bytes_from_held_dir<K: Ord + Copy>(
    directory: Dir,
    category: K,
    totals: &mut std::collections::BTreeMap<K, (u64, u64)>,
    counted: &mut std::collections::HashMap<(u64, u64), (K, u64)>,
    max_inodes: usize,
) -> io::Result<u64> {
    let mut total = 0_u64;
    // Hold only the current ancestry chain. Opening every sibling before
    // visiting any of them exhausted the process descriptor limit on wide
    // verified generations and made the whole sealed census fail.
    let mut pending = vec![(directory.read_dir(".")?, directory)];
    while let Some((entries, parent)) = pending.last_mut() {
        let Some(entry) = entries.next() else {
            pending.pop();
            continue;
        };
        let entry = entry?;
        let name = entry.file_name();
        let metadata = parent.symlink_metadata(&name)?;
        if metadata.is_dir() {
            let child = parent.open_dir_nofollow(&name)?;
            pending.push((child.read_dir(".")?, child));
            continue;
        }
        #[cfg(unix)]
        {
            use cap_std::fs::MetadataExt;
            let key = (metadata.dev(), metadata.ino());
            if let Some((previous_kind, charged_bytes)) = counted.get_mut(&key) {
                // A lower-ordered category has stronger ownership. A
                // superseded generation encountered first cannot claim
                // hard-linked physical bytes from a later live head.
                if category < *previous_kind {
                    let previous = totals.get_mut(&*previous_kind).ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "hard-linked sealed bytes lack their prior category",
                        )
                    })?;
                    previous.1 = previous.1.checked_sub(*charged_bytes).ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "hard-linked sealed bytes exceed their prior category",
                        )
                    })?;
                    total = total.saturating_add(*charged_bytes);
                    *previous_kind = category;
                }
                continue;
            }
            if counted.len() >= max_inodes {
                return Err(io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "sealed census inode budget exhausted; coverage is partial",
                ));
            }
            counted.try_reserve(1).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "sealed census inode allocation failed; coverage is partial",
                )
            })?;
            counted.insert(key, (category, metadata.len()));
        }
        #[cfg(not(unix))]
        let _ = (&counted, max_inodes);
        total = total.saturating_add(metadata.len());
    }
    Ok(total)
}

/// Remove exactly one held child directory without following a substituted
/// sealed-root or child symlink. Missing entries are idempotent; other failures
/// remain errors so callers can retain retry ownership.
pub fn remove_child_directory_nofollow(root_path: &Path, name: &OsStr) -> io::Result<bool> {
    single_component(name)?;
    let root = match open_root_nofollow(root_path) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let child = match root.open_dir_nofollow(name) {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    remove_open_dir_all_nofollow(child)?;
    Ok(true)
}

/// Retire a named generation only when a bounded receipt read from the SAME
/// opened directory proves the requested identity. Missing is idempotent;
/// unknown receipts are refusals, never permission to discard durable bytes.
pub fn remove_receipted_child_directory_nofollow(
    root_path: &Path,
    child_name: &OsStr,
    receipt_name: &OsStr,
    maximum: usize,
    authorized: impl FnOnce(&[u8]) -> bool,
) -> io::Result<bool> {
    single_component(child_name)?;
    single_component(receipt_name)?;
    let root = match open_root_nofollow(root_path) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let child = match root.open_dir_nofollow(child_name) {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let receipt = read_bounded_child_receipt(&child, receipt_name, maximum)?;
    if !authorized(&receipt) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "sealed generation retirement receipt did not prove its ownership",
        ));
    }
    remove_open_dir_all_nofollow(child)?;
    Ok(true)
}

/// Removes only children authorized by a bounded receipt read from the exact
/// directory handle that will be deleted. The sealed root and each child are
/// held capabilities: replacing either pathname after receipt validation
/// cannot redirect recursive removal to a different tree.
///
/// The caller must own exclusive store maintenance. Unknown, invalid and
/// unreadable child receipts preserve their directory; an actual deletion
/// failure is returned so retirement can retry instead of claiming success.
pub fn remove_receipted_directories_nofollow(
    root_path: &Path,
    receipt_name: &OsStr,
    maximum: usize,
    authorized: impl FnMut(&OsStr, &[u8]) -> bool,
) -> io::Result<usize> {
    remove_matching_receipted_directories_nofollow(
        root_path,
        receipt_name,
        maximum,
        |_| true,
        authorized,
    )
}

/// Scan and reclaim selected children beneath one held root. Filter names
/// before receipt I/O so unrelated installed generations are not treated as
/// abandoned staging or spills. Receipt verification and removal remain
/// anchored to the same opened child, even if the root pathname changes.
/// A failure on one entry is reported after its independent siblings are processed.
pub fn remove_matching_receipted_directories_nofollow(
    root_path: &Path,
    receipt_name: &OsStr,
    maximum: usize,
    mut candidate: impl FnMut(&OsStr) -> bool,
    mut authorized: impl FnMut(&OsStr, &[u8]) -> bool,
) -> io::Result<usize> {
    // Receipt names are always a single sibling: a slash-bearing name could
    // otherwise traverse a different directory even with O_NOFOLLOW.
    let mut receipt_components = Path::new(receipt_name).components();
    if !matches!(
        receipt_components.next(),
        Some(std::path::Component::Normal(_))
    ) || receipt_components.next().is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "receipt must be a single filename",
        ));
    }
    let root = open_root_nofollow(root_path)?;
    let outcomes = root.read_dir(".")?.map(|entry| -> io::Result<bool> {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            return Ok(false);
        }
        let name = entry.file_name();
        if !candidate(&name) {
            return Ok(false);
        }
        // Do not trust the DirEntry's former pathname kind after a rename.
        let child = match root.open_dir_nofollow(&name) {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!(%error, entry = ?name, "sealed child could not be verified");
                return Ok(false);
            }
        };
        let receipt = match read_bounded_child_receipt(&child, receipt_name, maximum) {
            Ok(receipt) => receipt,
            Err(error) => {
                tracing::warn!(%error, entry = ?name, "unverified sealed receipt preserved");
                return Ok(false);
            }
        };
        if !authorized(&name, &receipt) {
            return Ok(false);
        }
        remove_open_dir_all_nofollow(child).inspect_err(|error| {
            tracing::warn!(%error, entry = ?name, "verified child directory could not be removed");
        })?;
        Ok(true)
    });
    let mut removed = 0;
    let mut first_error = None;
    for outcome in outcomes {
        match outcome {
            Ok(true) => removed += 1,
            Ok(false) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    first_error.map_or(Ok(removed), Err)
}

#[cfg(unix)]
fn read_bounded_child_receipt(
    directory: &Dir,
    name: &OsStr,
    maximum: usize,
) -> io::Result<Vec<u8>> {
    let c_name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "receipt name contains NUL"))?;
    // SAFETY: the held directory fd is valid; name is a NUL-terminated
    // sibling component. O_NOFOLLOW prevents link traversal and O_NONBLOCK
    // prevents a substituted FIFO from blocking before the regular-file check.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c_name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        let error = io::Error::last_os_error();
        // Unix socket opens return ENXIO on Linux or EOPNOTSUPP on macOS.
        // Classify that refusal as a nonregular receipt only when the held
        // directory still names a nonregular entry. A regular file's I/O
        // failure must remain a refusal of complete census coverage.
        if matches!(error.raw_os_error(), Some(libc::ENXIO | libc::EOPNOTSUPP))
            && directory
                .symlink_metadata(name)
                .is_ok_and(|metadata| !metadata.file_type().is_file())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sealed receipt is not a regular file",
            ));
        }
        return Err(normalize_no_follow_error(error));
    }
    // SAFETY: openat returned a new owned fd and this File is its sole owner.
    let file = unsafe { std::fs::File::from_raw_fd(descriptor) };
    crate::framed_log::read_bounded_open_file(file, maximum)
}

#[cfg(windows)]
fn read_bounded_child_receipt(
    directory: &Dir,
    name: &OsStr,
    maximum: usize,
) -> io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = directory.open_with(name, &options)?.into_std();
    crate::framed_log::read_bounded_open_file(file, maximum)
}

/// Removes the held directory without following child symlinks.
pub fn remove_open_dir_all_nofollow(directory: Dir) -> io::Result<()> {
    #[cfg(not(windows))]
    {
        directory.remove_open_dir_all()
    }
    #[cfg(windows)]
    {
        // cap-std's Windows recursive removal drops its capability before
        // descending. Keep ours open until its children have been removed.
        for entry in directory.read_dir(".")? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let child = directory.open_dir_nofollow(entry.file_name())?;
                remove_open_dir_all_nofollow(child)?;
            } else {
                entry.remove_file()?;
            }
        }
        directory.remove_open_dir()
    }
}

#[cfg(test)]
mod replacement_tests {
    use std::io::Read;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn marker_publication_does_not_follow_a_substituted_directory() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let owned = temp.path().join("owned");
        let moved = temp.path().join("moved");
        let foreign = temp.path().join("foreign");
        std::fs::create_dir(&owned).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join("sealed.checked"), b"foreign bytes").unwrap();

        let held = open_root_nofollow(&owned).unwrap();
        std::fs::rename(&owned, &moved).unwrap();
        symlink(&foreign, &owned).unwrap();

        assert!(
            atomic_replace_child_file_nofollow(&owned, OsStr::new("sealed.checked"), b"new proof",)
                .is_err()
        );
        atomic_replace_file_in_dir(&held, OsStr::new("sealed.checked"), b"owned proof").unwrap();
        assert_eq!(
            std::fs::read(foreign.join("sealed.checked")).unwrap(),
            b"foreign bytes",
        );
        assert_eq!(
            std::fs::read(moved.join("sealed.checked")).unwrap(),
            b"owned proof",
        );
    }

    #[cfg(unix)]
    #[test]
    fn proof_publication_preserves_nonregular_marker_entries() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("sealed.checked");
        let foreign = temp.path().join("foreign");
        std::fs::write(&foreign, b"foreign").unwrap();
        symlink(&foreign, &marker).unwrap();

        assert_eq!(
            atomic_replace_child_file_nofollow(temp.path(), OsStr::new("sealed.checked"), b"proof")
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(
            std::fs::symlink_metadata(&marker)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link itself, not merely its target, must survive"
        );
        assert_eq!(std::fs::read(&foreign).unwrap(), b"foreign");

        std::fs::remove_file(&marker).unwrap();
        std::fs::create_dir(&marker).unwrap();
        assert_eq!(
            atomic_replace_child_file_nofollow(temp.path(), OsStr::new("sealed.checked"), b"proof")
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(marker.is_dir());

        std::fs::remove_dir(&marker).unwrap();
        atomic_replace_child_file_nofollow(temp.path(), OsStr::new("sealed.checked"), b"proof")
            .unwrap();
        assert_eq!(std::fs::read(&marker).unwrap(), b"proof");
        for entry in std::fs::read_dir(temp.path()).unwrap() {
            assert!(
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".sealed-proof-"),
                "rejected publications must not leave staging files"
            );
        }
    }

    #[test]
    fn replacement_publishes_complete_bytes_and_refuses_parent_traversal() {
        let root = tempfile::tempdir().unwrap();
        let parent = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        std::fs::write(root.path().join("published"), b"before").unwrap();
        std::fs::write(root.path().join("staged"), b"after").unwrap();
        let mut reader = std::fs::File::open(root.path().join("published")).unwrap();
        let error =
            replace_file(&parent, OsStr::new("staged"), OsStr::new("../escape")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read(root.path().join("published")).unwrap(),
            b"before"
        );
        assert_eq!(std::fs::read(root.path().join("staged")).unwrap(), b"after");
        replace_file(&parent, OsStr::new("staged"), OsStr::new("published")).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("published")).unwrap(),
            b"after"
        );
        let mut displaced = Vec::new();
        reader.read_to_end(&mut displaced).unwrap();
        assert_eq!(displaced, b"before");
        assert!(!root.path().join("staged").exists());
    }

    #[cfg(windows)]
    #[test]
    fn no_replace_publication_preserves_an_occupied_entry() {
        let root = tempfile::tempdir().unwrap();
        let parent = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        std::fs::write(root.path().join("staged"), b"owned").unwrap();
        std::fs::write(root.path().join("published"), b"foreign").unwrap();
        let error = rename_noreplace(
            &parent,
            OsStr::new("staged"),
            &parent,
            OsStr::new("published"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(root.path().join("staged")).unwrap(), b"owned");
        assert_eq!(
            std::fs::read(root.path().join("published")).unwrap(),
            b"foreign"
        );
        std::fs::remove_file(root.path().join("published")).unwrap();
        rename_noreplace(
            &parent,
            OsStr::new("staged"),
            &parent,
            OsStr::new("published"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(root.path().join("published")).unwrap(),
            b"owned"
        );
        assert!(!root.path().join("staged").exists());
    }
}

#[cfg(all(
    test,
    any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))
))]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt, symlink};
    #[cfg(unix)]
    use std::os::unix::net::UnixListener;

    use cap_std::ambient_authority;

    use super::*;

    fn open(path: &std::path::Path) -> Dir {
        Dir::open_ambient_dir(path, ambient_authority()).expect("open test directory")
    }

    #[cfg(unix)]
    #[test]
    fn wide_verified_generation_census_stays_within_descriptor_budget() {
        const CHILD: &str = "TRACEDECAY_WIDE_CENSUS_RLIMIT_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Resource limits are process-wide; isolate the reduced budget
            // from concurrently executing tests in the parent libtest runner.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "capability_dir::tests::wide_verified_generation_census_stays_within_descriptor_budget",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "census under reduced descriptor budget failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "the child must execute the actual behavioral regression: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sealed");
        let generation = root.join("generation");
        std::fs::create_dir_all(&generation).unwrap();
        std::fs::write(generation.join("sealed.json"), b"owned").unwrap();
        for index in 0..128 {
            let column = generation.join(format!("column-{index}"));
            std::fs::create_dir(&column).unwrap();
            std::fs::write(column.join("value"), b"x").unwrap();
        }

        // The old breadth-first handle stack needed >=128 live handles
        // here; the corrected walk needs only the current three levels.
        let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
            0
        );
        limit.rlim_cur = limit.rlim_cur.min(64);
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);

        let totals = census_receipted_directory_bytes(
            &root,
            false,
            |_| ("sealed.json", 64),
            |_, receipt| receipt == Some(b"owned".as_slice()),
        )
        .unwrap();
        assert_eq!(totals.get(&true), Some(&(1, 133)));
        assert_eq!(
            std::fs::read(generation.join("column-127/value")).unwrap(),
            b"x"
        );
    }

    /// Hard-linked base bytes count once and belong to the live head even
    /// when a superseded generation was physically visited first.
    #[cfg(unix)]
    #[test]
    fn shared_sealed_bytes_follow_head_priority_not_directory_visitation() {
        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        enum Kind {
            Head,
            Superseded,
            Unrecognized,
        }

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sealed");
        let superseded = root.join("old");
        let head = root.join("head");
        std::fs::create_dir_all(&superseded).unwrap();
        std::fs::create_dir(&head).unwrap();
        std::fs::write(superseded.join("owner"), b"old").unwrap();
        std::fs::write(head.join("owner"), b"head").unwrap();
        std::fs::write(superseded.join("shared"), b"shared-bytes").unwrap();
        std::fs::hard_link(superseded.join("shared"), head.join("shared")).unwrap();

        // Exercise the actual walker in the adverse order without relying on
        // the operating system's unspecified read_dir ordering.
        let mut counted = std::collections::HashMap::new();
        let mut charged = std::collections::BTreeMap::<Kind, (u64, u64)>::new();
        for (kind, path) in [(Kind::Superseded, &superseded), (Kind::Head, &head)] {
            let bytes = directory_bytes_from_held_dir(
                open(path),
                kind,
                &mut charged,
                &mut counted,
                MAX_SEALED_CENSUS_INODES,
            )
            .unwrap();
            let entry = charged.entry(kind).or_default();
            entry.0 += 1;
            entry.1 += bytes;
        }
        assert_eq!(charged.get(&Kind::Superseded), Some(&(1, 3)));
        assert_eq!(charged.get(&Kind::Head), Some(&(1, 16)));

        // The production census also classifies receipt-bound children and
        // must report one physical copy without charging retired bytes twice.
        let reported = census_receipted_directory_bytes(
            &root,
            Kind::Unrecognized,
            |_| ("owner", 64),
            |_, receipt| match receipt {
                Some(bytes) if bytes == b"head" => Kind::Head,
                Some(bytes) if bytes == b"old" => Kind::Superseded,
                _ => Kind::Unrecognized,
            },
        )
        .unwrap();
        assert_eq!(reported.get(&Kind::Head), Some(&(1, 16)));
        assert_eq!(reported.get(&Kind::Superseded), Some(&(1, 3)));
        assert_eq!(reported.get(&Kind::Unrecognized), None);
        assert_eq!(std::fs::read(head.join("shared")).unwrap(), b"shared-bytes");
    }

    /// Disjoint pairs still require evidence for the full walk: a publisher
    /// can add another alias after any pair completes. A small budget must
    /// refuse; a sufficient budget must count every pair exactly once.
    #[cfg(unix)]
    #[test]
    fn complete_link_pairs_require_retained_census_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sealed");
        let generation = root.join("generation");
        std::fs::create_dir_all(&generation).unwrap();
        std::fs::write(generation.join("owner"), b"owned").unwrap();
        for index in 0..4 {
            let column = generation.join(format!("column-{index}"));
            std::fs::create_dir(&column).unwrap();
            let original = column.join("original");
            std::fs::write(&original, b"rows").unwrap();
            std::fs::hard_link(&original, column.join("alias")).unwrap();
        }

        let refused = census_receipted_directory_bytes_bounded(
            &root,
            false,
            |_| ("owner", 64),
            |_, receipt| receipt == Some(b"owned".as_slice()),
            2,
        )
        .unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::OutOfMemory, "{refused}");
        let census = census_receipted_directory_bytes_bounded(
            &root,
            false,
            |_| ("owner", 64),
            |_, receipt| receipt == Some(b"owned".as_slice()),
            5,
        )
        .unwrap();
        assert_eq!(census.get(&true), Some(&(1, 21)));
        for index in 0..4 {
            let column = generation.join(format!("column-{index}"));
            assert_eq!(std::fs::read(column.join("original")).unwrap(), b"rows");
            assert_eq!(std::fs::read(column.join("alias")).unwrap(), b"rows");
        }
    }

    /// An adversarial store with many distinct hard-linked inodes must
    /// decline its complete census before unbounded heap growth. A caller
    /// can then truthfully mark coverage partial rather than report zeros as
    /// a complete inventory. The ordinary production entrypoint still counts
    /// all the same real files when within its budget.
    #[cfg(unix)]
    #[test]
    fn sealed_census_refuses_excess_shared_inodes_without_losing_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sealed");
        let generation = root.join("generation");
        std::fs::create_dir_all(&generation).unwrap();
        std::fs::write(generation.join("owner"), b"owned").unwrap();
        for index in 0..3 {
            let source = generation.join(format!("payload-{index}"));
            std::fs::write(&source, b"x").unwrap();
            std::fs::hard_link(&source, temp.path().join(format!("alias-{index}"))).unwrap();
        }

        let refused = census_receipted_directory_bytes_bounded(
            &root,
            false,
            |_| ("owner", 64),
            |_, receipt| receipt == Some(b"owned".as_slice()),
            3,
        )
        .unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::OutOfMemory, "{refused}");

        let complete = census_receipted_directory_bytes(
            &root,
            false,
            |_| ("owner", 64),
            |_, receipt| receipt == Some(b"owned".as_slice()),
        )
        .unwrap();
        assert_eq!(complete.get(&true), Some(&(1, 8)));
        for index in 0..3 {
            assert_eq!(
                std::fs::read(generation.join(format!("payload-{index}"))).unwrap(),
                b"x"
            );
            assert_eq!(
                std::fs::read(temp.path().join(format!("alias-{index}"))).unwrap(),
                b"x"
            );
        }
    }

    /// A link group is not closed by its observed nlink count: a layered
    /// publisher can add another alias after the census has visited every
    /// old link. The head must inherit the one physical charge in both cases.
    #[cfg(unix)]
    #[test]
    fn census_retains_inode_identity_across_later_link_publication() {
        for initial_peer in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("sealed");
            let old = root.join("old");
            std::fs::create_dir_all(&old).unwrap();
            std::fs::write(old.join("owner"), b"old").unwrap();
            let base = old.join("shared");
            std::fs::write(&base, b"rows").unwrap();
            let mut initial = vec![OsString::from("old")];
            if initial_peer {
                let peer = root.join("old-peer");
                std::fs::create_dir(&peer).unwrap();
                std::fs::write(peer.join("owner"), b"old").unwrap();
                std::fs::hard_link(&base, peer.join("shared")).unwrap();
                initial.push(OsString::from("old-peer"));
            }
            assert_eq!(
                std::fs::metadata(&base).unwrap().nlink(),
                if initial_peer { 2 } else { 1 },
            );
            let root_dir = open_root_nofollow(&root).unwrap();
            let mut names = initial.into_iter();
            let mut published = false;
            let entries = std::iter::from_fn(|| {
                if let Some(name) = names.next() {
                    return Some(Ok(name));
                }
                if published {
                    return None;
                }
                let head = root.join("head");
                std::fs::create_dir(&head).unwrap();
                std::fs::write(head.join("owner"), b"head").unwrap();
                std::fs::hard_link(&base, head.join("shared")).unwrap();
                published = true;
                Some(Ok(OsString::from("head")))
            });
            let census = census_receipted_children(
                &root_dir,
                entries,
                2_u8,
                |_| ("owner", 16),
                |_, receipt| {
                    if receipt == Some(b"head".as_slice()) {
                        0
                    } else if receipt == Some(b"old".as_slice()) {
                        1
                    } else {
                        2
                    }
                },
                MAX_SEALED_CENSUS_INODES,
            )
            .unwrap();
            assert!(published, "the head must have been published mid-census");
            assert_eq!(
                std::fs::metadata(&base).unwrap().nlink(),
                if initial_peer { 3 } else { 2 },
            );
            assert_eq!(census.get(&0), Some(&(1, 8)), "live head owns shared bytes");
            assert_eq!(
                census.get(&1),
                Some(&(
                    if initial_peer { 2 } else { 1 },
                    if initial_peer { 6 } else { 3 }
                )),
                "old generations retain only their own receipts"
            );
            assert!(!census.contains_key(&2));
            assert_eq!(std::fs::read(root.join("head/shared")).unwrap(), b"rows");
            assert_eq!(std::fs::read(&base).unwrap(), b"rows");
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_receipt_is_unrecognized_without_hiding_the_verified_head() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sealed");
        let head = root.join("head");
        let unknown = root.join("unknown");
        std::fs::create_dir_all(&head).unwrap();
        std::fs::create_dir(&unknown).unwrap();
        std::fs::write(head.join("owner"), b"owned").unwrap();
        std::fs::write(head.join("payload"), b"durable").unwrap();
        std::fs::write(unknown.join("sentinel"), b"preserve").unwrap();
        symlink(head.join("owner"), unknown.join("owner")).unwrap();

        let receipt = read_bounded_child_receipt(&open(&unknown), OsStr::new("owner"), 64);
        assert_eq!(receipt.unwrap_err().kind(), io::ErrorKind::InvalidInput);

        let totals = census_receipted_directory_bytes(
            &root,
            false,
            |_| ("owner", 64),
            |_, receipt| receipt == Some(b"owned".as_slice()),
        )
        .unwrap();
        assert_eq!(totals.get(&true), Some(&(1, 12)));
        assert_eq!(totals.get(&false), Some(&(1, 0)));
        assert_eq!(std::fs::read(head.join("payload")).unwrap(), b"durable");
        assert_eq!(std::fs::read(head.join("owner")).unwrap(), b"owned");
        assert_eq!(
            std::fs::read(unknown.join("sentinel")).unwrap(),
            b"preserve"
        );
        assert!(
            std::fs::symlink_metadata(unknown.join("owner"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn socket_receipt_is_unrecognized_without_hiding_the_verified_head() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("s");
        let head = root.join("head");
        let unknown = root.join("unknown");
        std::fs::create_dir_all(&head).unwrap();
        std::fs::create_dir(&unknown).unwrap();
        std::fs::write(head.join("owner"), b"owned").unwrap();
        std::fs::write(head.join("payload"), b"durable").unwrap();
        std::fs::write(unknown.join("sentinel"), b"preserve").unwrap();
        let _socket = UnixListener::bind(unknown.join("owner")).unwrap();

        // openat on a socket fails with ENXIO or EOPNOTSUPP before fstat.
        // The held directory's metadata proves the entry is nonregular.
        let receipt = read_bounded_child_receipt(&open(&unknown), OsStr::new("owner"), 64);
        assert_eq!(receipt.unwrap_err().kind(), io::ErrorKind::InvalidData);

        let totals = census_receipted_directory_bytes(
            &root,
            2_u8,
            |_| ("owner", 64),
            |_, receipt| {
                if receipt == Some(b"owned".as_slice()) {
                    0
                } else {
                    2
                }
            },
        )
        .unwrap();
        assert_eq!(totals.get(&0), Some(&(1, 12)));
        assert_eq!(totals.get(&2), Some(&(1, 0)));
        assert_eq!(std::fs::read(head.join("payload")).unwrap(), b"durable");
        assert_eq!(
            std::fs::read(unknown.join("sentinel")).unwrap(),
            b"preserve"
        );
        assert!(unknown.join("owner").exists());
    }

    #[cfg(unix)]
    #[test]
    fn inaccessible_generation_refuses_complete_census_and_recovers() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sealed");
        let generation = root.join("generation");
        std::fs::create_dir_all(&generation).unwrap();
        std::fs::write(generation.join("owner"), b"owned").unwrap();
        std::fs::write(generation.join("payload"), b"durable").unwrap();

        let original = std::fs::metadata(&generation).unwrap().permissions();
        std::fs::set_permissions(&generation, std::fs::Permissions::from_mode(0o000)).unwrap();
        let refused = census_receipted_directory_bytes(
            &root,
            false,
            |_| ("owner", 16),
            |_, receipt| receipt == Some(b"owned".as_slice()),
        );
        // Restore access before assertions so even a failed test can reclaim
        // its fixture, and so recovery exercises the exact same stored bytes.
        std::fs::set_permissions(&generation, original).unwrap();
        assert_eq!(
            refused
                .expect_err("an unreadable generation cannot be reported as zero")
                .kind(),
            io::ErrorKind::PermissionDenied,
        );
        let owner_marker = generation.join("owner");
        let marker_permissions = std::fs::metadata(&owner_marker).unwrap().permissions();
        std::fs::set_permissions(&owner_marker, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable_marker = census_receipted_directory_bytes(
            &root,
            false,
            |_| ("owner", 16),
            |_, receipt| receipt == Some(b"owned".as_slice()),
        );
        std::fs::set_permissions(&owner_marker, marker_permissions).unwrap();
        assert_eq!(
            unreadable_marker
                .expect_err("unreadable receipt cannot silently become unknown")
                .kind(),
            io::ErrorKind::PermissionDenied,
        );

        let complete = census_receipted_directory_bytes(
            &root,
            false,
            |_| ("owner", 16),
            |_, receipt| receipt == Some(b"owned".as_slice()),
        )
        .unwrap();
        assert_eq!(complete.get(&true), Some(&(1, 12)));
        assert_eq!(
            std::fs::read(generation.join("payload")).unwrap(),
            b"durable"
        );
    }

    #[test]
    fn staged_generation_install_preserves_an_occupied_empty_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sealed");
        std::fs::create_dir(&root).unwrap();
        let staged_name = OsStr::new(".staging-generation");
        let installed_name = OsStr::new("generation");
        let staged = root.join(staged_name);
        let installed = root.join(installed_name);
        std::fs::create_dir(&staged).unwrap();
        std::fs::write(staged.join("sealed.json"), b"built").unwrap();
        std::fs::create_dir(&installed).unwrap();

        let result = install_child_directory_noreplace_nofollow(&root, staged_name, installed_name);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert!(
            installed.is_dir(),
            "an empty foreign target is not expendable"
        );
        assert_eq!(std::fs::read(staged.join("sealed.json")).unwrap(), b"built");

        std::fs::remove_dir(&installed).unwrap();
        install_child_directory_noreplace_nofollow(&root, staged_name, installed_name).unwrap();
        assert!(!staged.exists());
        assert_eq!(
            std::fs::read(installed.join("sealed.json")).unwrap(),
            b"built"
        );
    }

    #[cfg(unix)]
    #[test]
    fn staged_generation_install_refuses_linked_root_and_staging_entries() {
        let temp = tempfile::tempdir().unwrap();
        let foreign = temp.path().join("foreign");
        let real = temp.path().join("real");
        let link = temp.path().join("link");
        std::fs::create_dir(&foreign).unwrap();
        std::fs::create_dir(&real).unwrap();
        std::fs::write(foreign.join("sentinel"), b"preserve").unwrap();
        std::fs::create_dir(foreign.join("staged")).unwrap();
        std::os::unix::fs::symlink(&foreign, &link).unwrap();
        let stage = OsStr::new("staged");
        let published = OsStr::new("installed");
        assert!(install_child_directory_noreplace_nofollow(&link, stage, published).is_err());

        std::os::unix::fs::symlink(&foreign, real.join("staged")).unwrap();
        assert!(install_child_directory_noreplace_nofollow(&real, stage, published).is_err());
        assert!(!foreign.join("installed").exists());
        assert!(!real.join("installed").exists());
        assert_eq!(
            std::fs::read(foreign.join("sentinel")).unwrap(),
            b"preserve"
        );
    }

    #[test]
    fn rename_noreplace_moves_between_parents_and_refuses_occupied_destinations() {
        let root = tempfile::tempdir().expect("create rename fixture");
        std::fs::create_dir(root.path().join("source")).expect("create source parent");
        std::fs::create_dir(root.path().join("target")).expect("create target parent");
        std::fs::write(root.path().join("source/payload"), b"owned").expect("write payload");
        let source = open(&root.path().join("source"));
        let target = open(&root.path().join("target"));

        rename_noreplace(
            &source,
            OsStr::new("payload"),
            &target,
            OsStr::new("payload"),
        )
        .expect("cross-parent rename succeeds into a free destination");
        assert_eq!(
            std::fs::read(root.path().join("target/payload")).expect("moved payload"),
            b"owned"
        );

        std::fs::write(root.path().join("source/payload"), b"replacement")
            .expect("write replacement");
        let error = rename_noreplace(
            &source,
            OsStr::new("payload"),
            &target,
            OsStr::new("payload"),
        )
        .expect_err("an occupied destination must never be replaced");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(root.path().join("target/payload")).expect("retained payload"),
            b"owned"
        );
    }

    #[test]
    fn recursive_removal_unlinks_symlinks_without_following_them() {
        let root = tempfile::tempdir().expect("create removal fixture");
        let external = tempfile::tempdir().expect("create external target");
        std::fs::write(external.path().join("sentinel"), b"preserve").expect("write sentinel");
        let tree = root.path().join("tree");
        std::fs::create_dir_all(tree.join("nested")).expect("create nested tree");
        std::fs::write(tree.join("nested/file"), b"bytes").expect("write nested file");
        std::os::unix::fs::symlink(external.path(), tree.join("escape"))
            .expect("plant symlink escape");

        remove_open_dir_all_nofollow(open(&tree)).expect("recursive removal consumes the tree");

        assert!(!tree.exists());
        assert_eq!(
            std::fs::read(external.path().join("sentinel")).expect("sentinel survives"),
            b"preserve"
        );
    }

    /// An ambient-path census would charge the 100,041 foreign bytes after
    /// a root swap. This census must charge only its held 41-byte payload.
    #[cfg(unix)]
    #[test]
    fn receipted_census_keeps_opened_child_after_root_substitution() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("graph.sealed");
        let displaced = temp.path().join("displaced");
        let foreign = temp.path().join("foreign");
        std::fs::create_dir_all(root.join("generation")).unwrap();
        std::fs::create_dir_all(foreign.join("generation")).unwrap();
        std::fs::write(root.join("generation/owner"), b"owned").unwrap();
        std::fs::write(root.join("generation/payload"), vec![b'a'; 41]).unwrap();
        std::fs::write(foreign.join("generation/owner"), b"owned").unwrap();
        std::fs::write(foreign.join("generation/payload"), vec![b'f'; 100_041]).unwrap();
        #[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
        enum Entry {
            Owned,
            Unknown,
        }
        let totals = census_receipted_directory_bytes(
            &root,
            Entry::Unknown,
            |_| ("owner", 16),
            |name, receipt| {
                assert_eq!(name, OsStr::new("generation"));
                assert_eq!(receipt, Some(b"owned".as_slice()));
                std::fs::rename(&root, &displaced).unwrap();
                std::os::unix::fs::symlink(&foreign, &root).unwrap();
                Entry::Owned
            },
        )
        .unwrap();
        assert_eq!(totals.get(&Entry::Owned), Some(&(1, 46)));
        assert_eq!(
            std::fs::read(foreign.join("generation/payload")).unwrap(),
            vec![b'f'; 100_041],
        );
        assert_eq!(
            std::fs::read(displaced.join("generation/payload")).unwrap(),
            vec![b'a'; 41],
        );
    }

    /// Candidate filtering must not reopen a potentially substituted root:
    /// only the receipted owned child goes away; unrelated and foreign trees survive.
    #[cfg(unix)]
    #[test]
    fn filtered_receipt_sweep_holds_root_through_path_substitution() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("graph.sealed");
        let displaced = temp.path().join("owned-displaced");
        let foreign = temp.path().join("foreign");
        std::fs::create_dir_all(root.join(".rows-old")).unwrap();
        std::fs::create_dir_all(root.join("generation")).unwrap();
        std::fs::create_dir_all(foreign.join(".rows-old")).unwrap();
        std::fs::write(root.join(".rows-old/.owner"), b"verified").unwrap();
        std::fs::write(root.join("generation/live"), b"live").unwrap();
        std::fs::write(foreign.join(".rows-old/.owner"), b"verified").unwrap();
        std::fs::write(foreign.join(".rows-old/sentinel"), b"foreign").unwrap();

        let removed = remove_matching_receipted_directories_nofollow(
            &root,
            OsStr::new(".owner"),
            512,
            |name| name == OsStr::new(".rows-old"),
            |name, receipt| {
                assert_eq!(name, OsStr::new(".rows-old"));
                assert_eq!(receipt, b"verified");
                std::fs::rename(&root, &displaced).unwrap();
                std::os::unix::fs::symlink(&foreign, &root).unwrap();
                true
            },
        )
        .unwrap();

        assert_eq!(removed, 1);
        assert!(!displaced.join(".rows-old").exists());
        assert_eq!(
            std::fs::read(displaced.join("generation/live")).unwrap(),
            b"live"
        );
        assert_eq!(
            std::fs::read(foreign.join(".rows-old/sentinel")).unwrap(),
            b"foreign"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_verified_unlink_still_reclaims_independent_siblings() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("graph.sealed");
        for name in ["a", "b"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("owner"), name).unwrap();
        }
        let mut verified = Vec::new();
        let error = remove_matching_receipted_directories_nofollow(
            &root,
            OsStr::new("owner"),
            64,
            |_| true,
            |name, receipt| {
                assert_eq!(name.as_encoded_bytes(), receipt);
                if verified.is_empty() {
                    // A concurrent cleanup wins after this child was verified.
                    // Its now-unlinked capability cannot be removed a second time.
                    std::fs::remove_dir_all(root.join(name)).unwrap();
                }
                verified.push(name.to_owned());
                true
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{error}");
        assert_eq!(verified.len(), 2);
        assert!(!root.join(&verified[1]).exists());
    }

    /// A parent-path swap after receipt validation must not redirect cleanup:
    /// the already-open child is removed, and matching foreign bytes survive.
    #[cfg(unix)]
    #[test]
    fn verified_cleanup_remains_bound_when_sealed_root_is_substituted() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("graph.sealed");
        let displaced = temp.path().join("owned-displaced");
        let foreign = temp.path().join("foreign");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(foreign.join("a")).unwrap();
        std::fs::write(root.join("a/sealed.json"), b"owned receipt").unwrap();
        std::fs::write(root.join("a/owned"), b"owned").unwrap();
        std::fs::write(foreign.join("a/sentinel"), b"foreign").unwrap();

        let removed = remove_receipted_directories_nofollow(
            &root,
            OsStr::new("sealed.json"),
            1024,
            |name, bytes| {
                assert_eq!(name, OsStr::new("a"));
                assert_eq!(bytes, b"owned receipt");
                std::fs::rename(&root, &displaced).unwrap();
                std::os::unix::fs::symlink(&foreign, &root).unwrap();
                true
            },
        )
        .unwrap();
        assert_eq!(removed, 1);
        assert!(!displaced.join("a").exists());
        assert_eq!(
            std::fs::read(foreign.join("a/sentinel")).unwrap(),
            b"foreign"
        );
    }

    /// The child also remains the exact opened directory when its original
    /// entry name becomes occupied by an unrelated real directory.
    #[cfg(unix)]
    #[test]
    fn verified_cleanup_does_not_remove_a_replacement_real_child() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("graph.sealed");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a/sealed.json"), b"owned receipt").unwrap();
        let moved = root.join("moved");
        let removed = remove_receipted_directories_nofollow(
            &root,
            OsStr::new("sealed.json"),
            1024,
            |_, bytes| {
                assert_eq!(bytes, b"owned receipt");
                std::fs::rename(root.join("a"), &moved).unwrap();
                std::fs::create_dir(root.join("a")).unwrap();
                std::fs::write(root.join("a/sentinel"), b"foreign").unwrap();
                true
            },
        )
        .unwrap();
        assert_eq!(removed, 1);
        assert!(!moved.exists());
        assert_eq!(std::fs::read(root.join("a/sentinel")).unwrap(), b"foreign");
    }

    #[test]
    fn a_receipt_filename_cannot_escape_its_held_child() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sealed");
        std::fs::create_dir(&root).unwrap();
        for receipt in ["../outside", "a/b", "."] {
            let error =
                remove_receipted_directories_nofollow(&root, OsStr::new(receipt), 512, |_, _| true)
                    .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[cfg(unix)]
    #[test]
    fn exact_receipted_retirement_preserves_unknown_and_swapped_real_children() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("graph.sealed");
        let child = root.join("a");
        let moved = root.join("moved");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(child.join("sealed.json"), b"owned receipt").unwrap();
        std::fs::write(child.join("owned"), b"owned").unwrap();
        assert_eq!(
            remove_receipted_child_directory_nofollow(
                &root,
                OsStr::new("a"),
                OsStr::new("sealed.json"),
                1024,
                |_| false,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::PermissionDenied,
        );
        assert_eq!(std::fs::read(child.join("owned")).unwrap(), b"owned");
        let removed = remove_receipted_child_directory_nofollow(
            &root,
            OsStr::new("a"),
            OsStr::new("sealed.json"),
            1024,
            |receipt| {
                assert_eq!(receipt, b"owned receipt");
                std::fs::rename(&child, &moved).unwrap();
                std::fs::create_dir(&child).unwrap();
                std::fs::write(child.join("foreign"), b"preserve").unwrap();
                true
            },
        )
        .unwrap();
        assert!(removed);
        assert!(
            !moved.exists(),
            "only the receipt-authorized directory is deleted"
        );
        assert_eq!(std::fs::read(child.join("foreign")).unwrap(), b"preserve");
    }

    #[cfg(unix)]
    #[test]
    fn exact_cleanup_refuses_a_substituted_root_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("graph.sealed");
        let displaced = temp.path().join("original");
        let foreign = temp.path().join("foreign");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(foreign.join("a")).unwrap();
        std::fs::write(root.join("a/sealed.json"), b"owned").unwrap();
        std::fs::write(foreign.join("a/sealed.json"), b"foreign").unwrap();
        std::fs::write(foreign.join("a/sentinel"), b"preserve").unwrap();
        std::fs::rename(&root, &displaced).unwrap();
        std::os::unix::fs::symlink(&foreign, &root).unwrap();
        assert!(remove_child_directory_nofollow(&root, OsStr::new("a")).is_err());
        assert!(
            remove_receipted_child_directory_nofollow(
                &root,
                OsStr::new("a"),
                OsStr::new("sealed.json"),
                1024,
                |_| true,
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(foreign.join("a/sentinel")).unwrap(),
            b"preserve"
        );
        assert_eq!(
            std::fs::read(displaced.join("a/sealed.json")).unwrap(),
            b"owned"
        );
        assert_eq!(
            remove_child_directory_nofollow(&displaced, OsStr::new("../a"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput,
        );
    }

    #[test]
    fn a_transient_missing_answer_is_looked_at_again() {
        let mut attempts = 0;
        let value = look_again_on_not_found(|| {
            attempts += 1;
            if attempts <= 2 {
                Err(io::Error::from(io::ErrorKind::NotFound))
            } else {
                Ok(attempts)
            }
        })
        .expect("the entry that appears within the bound is returned");
        assert_eq!(value, 3);
    }

    #[test]
    fn a_persistent_missing_answer_is_still_reported_after_the_bound() {
        let mut attempts = 0;
        let error = look_again_on_not_found(|| -> io::Result<()> {
            attempts += 1;
            Err(io::Error::from(io::ErrorKind::NotFound))
        })
        .expect_err("a parent that is genuinely gone must not spin");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(attempts, CREATE_RACE_LOOKS + 1);
    }

    #[test]
    fn other_errors_are_not_looked_at_again() {
        let mut attempts = 0;
        let error = look_again_on_not_found(|| -> io::Result<()> {
            attempts += 1;
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .expect_err("only the create race is retried");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(attempts, 1);
    }

    /// Many threads racing the first creation of one name must all come back
    /// holding the same entry. On macOS this is the `openat(O_CREAT)` race
    /// the helper exists for; elsewhere it is a plain concurrency smoke test.
    #[test]
    fn concurrent_creators_of_one_entry_all_open_it() {
        use std::os::unix::fs::MetadataExt;

        let root = tempfile::tempdir().expect("create race fixture");
        for round in 0..16 {
            let name = format!("racy-{round}.lock");
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let handles = (0..8)
                .map(|_| {
                    let path = root.path().to_path_buf();
                    let name = name.clone();
                    let barrier = std::sync::Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        let directory = open(&path);
                        let mut options = OpenOptions::new();
                        options.read(true).write(true).create(true);
                        barrier.wait();
                        open_or_create_with(&directory, OsStr::new(&name), &options)
                            .expect("a create-or-open loser must still be handed the entry")
                            .into_std()
                            .metadata()
                            .expect("metadata of the opened entry")
                            .ino()
                    })
                })
                .collect::<Vec<_>>();
            let inodes = handles
                .into_iter()
                .map(|handle| handle.join().expect("creator thread"))
                .collect::<Vec<_>>();
            assert!(
                inodes.iter().all(|inode| *inode == inodes[0]),
                "every racer must open the one created entry"
            );
        }
    }
}
