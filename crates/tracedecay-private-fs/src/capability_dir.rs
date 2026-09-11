//! Capability-relative durable directory primitives shared by the quarantine
//! and retirement authorities: atomic no-replace rename between already-open
//! parent capabilities, directory metadata sync, recursive no-follow removal,
//! and create-or-open of one entry that survives the Darwin create race.
//! Every mutation is relative to an open `Dir` handle so a parent path
//! swapped for a symlink cannot redirect the operation.

use std::ffi::OsStr;
use std::io;

use cap_fs_ext::DirExt;
#[cfg(not(windows))]
use cap_fs_ext::OpenOptionsMaybeDirExt;
use cap_std::fs::{Dir, File, OpenOptions};

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
/// ledger opens do — and those are exactly the files many writers create at
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
        use std::os::fd::AsRawFd;

        crate::rename_noreplace::rename_noreplace_at(
            from_parent.as_raw_fd(),
            from,
            to_parent.as_raw_fd(),
            to,
        )
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;

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
