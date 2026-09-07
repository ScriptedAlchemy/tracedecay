//! Capability-relative durable directory primitives shared by the quarantine
//! and retirement authorities: atomic no-replace rename between already-open
//! parent capabilities, directory metadata sync, and recursive no-follow
//! removal. Every mutation is relative to an open `Dir` handle so a parent
//! path swapped for a symlink cannot redirect the operation.

use std::ffi::OsStr;
use std::io;

use cap_fs_ext::DirExt;
#[cfg(not(windows))]
use cap_fs_ext::OpenOptionsMaybeDirExt;
use cap_std::fs::Dir;
#[cfg(not(windows))]
use cap_std::fs::OpenOptions;

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
        use std::os::fd::AsRawFd;

        let from = component_cstring(from)?;
        let to = component_cstring(to)?;
        // SAFETY: both names are single-component C strings and both fds are
        // already-open directory capabilities.
        let result = unsafe {
            libc::renameat2(
                from_parent.as_raw_fd(),
                from.as_ptr(),
                to_parent.as_raw_fd(),
                to.as_ptr(),
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
    {
        use std::os::fd::AsRawFd;

        let from = component_cstring(from)?;
        let to = component_cstring(to)?;
        // SAFETY: both names are single-component C strings and RENAME_EXCL
        // refuses an occupied destination.
        let result = unsafe {
            libc::renameatx_np(
                from_parent.as_raw_fd(),
                from.as_ptr(),
                to_parent.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

        let from = dir_entry_wide(from_parent, from)?;
        let to = dir_entry_wide(to_parent, to)?;
        // SAFETY: both UTF-16 strings are NUL-terminated. Omitting
        // MOVEFILE_REPLACE_EXISTING makes an occupied destination fail.
        let moved = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) };
        if moved != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
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

#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
fn component_cstring(name: &OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;

    std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))
}

#[cfg(windows)]
fn dir_entry_wide(parent: &Dir, name: &OsStr) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;

    Ok(dir_path(parent)?
        .join(name)
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect())
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

/// Recursively removes an already-open directory without following symlinks,
/// so an entry swapped for a symlink cannot redirect cleanup outside the
/// tree. `interrupt` runs before every child operation; returning an error
/// stops the descent and leaves the remaining entries in place for a later
/// reconciliation.
pub fn remove_open_dir_all_nofollow(
    directory: Dir,
    interrupt: &mut dyn FnMut() -> io::Result<()>,
) -> io::Result<()> {
    interrupt()?;
    for entry in directory.read_dir(".")? {
        interrupt()?;
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name();
            let child = directory.open_dir_nofollow(&name)?;
            remove_open_dir_all_nofollow(child, interrupt)?;
        } else {
            entry.remove_file()?;
        }
    }
    interrupt()?;
    directory.remove_open_dir()
}

#[cfg(all(
    test,
    any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))
))]
mod tests {
    use cap_std::ambient_authority;

    use super::*;

    fn open(path: &std::path::Path) -> Dir {
        Dir::open_ambient_dir(path, ambient_authority()).expect("open test directory")
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

        remove_open_dir_all_nofollow(open(&tree), &mut || Ok(()))
            .expect("recursive removal consumes the tree");

        assert!(!tree.exists());
        assert_eq!(
            std::fs::read(external.path().join("sentinel")).expect("sentinel survives"),
            b"preserve"
        );
    }

    #[test]
    fn interrupted_removal_stops_and_retains_remaining_entries() {
        let root = tempfile::tempdir().expect("create interrupt fixture");
        let tree = root.path().join("tree");
        std::fs::create_dir(&tree).expect("create tree");
        std::fs::write(tree.join("file"), b"bytes").expect("write file");

        let error = remove_open_dir_all_nofollow(open(&tree), &mut || {
            Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
        })
        .expect_err("interruption must surface");

        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(tree.join("file").exists());
    }

    #[test]
    fn sync_directory_flushes_an_open_capability() {
        let root = tempfile::tempdir().expect("create sync fixture");
        sync_directory(&open(root.path())).expect("sync an open directory capability");
    }
}
